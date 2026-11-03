//! 网关（`--role gateway`）：账本单写者，工人经帧协议来拉任务、续约、提交。
//!
//! 账本逻辑与单主机相同（[`Ledger`]）；这里只做会话管理、成员表推送、日志传送给温备，
//! 以及判定失联的工人。
//!
//! 失联判定不另设心跳超时：工人的一个租约到期、而它从拿到（或最后续约）这个租约起再没发过任何帧，
//! 就判它失联，移出成员表，它负责的站点按 HRW 迁给其他工人。在线的工人要么在续约，要么在拉取，
//! 不会满足这个条件；空闲的工人不持有租约，也就不会因此被移出。连接断开时立即移出。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hyper::body::Incoming;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Notify;

use crate::coordinator::{BoxFuture, Coordinator, Deliver};
use crate::frame::{Msg, WireLease};
use crate::hrw::HostMap;
use crate::journal::Origin;
use crate::ledger::{Expired, Ledger, Tick};
use crate::local::LedgerFront;
use crate::transport::auth::TokenStore;
use crate::transport::server::{Accept, serve_connection};
use crate::transport::{FrameReader, Outbox, default_max_frame};
use crate::types::{
    Error, Lease, Owner, Priors, Renewal, Resources, Scope, SiteKey, StatsDelta, Task, TaskId,
    WorkerId, lock,
};

#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub max_frame: usize,
    /// 日志空闲时给温备发心跳的间隔。
    pub heartbeat: Duration,
    /// 网关自己发统计时用的启动编号（区分重启，见 [`Origin`]）。
    pub boot: u64,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            max_frame: default_max_frame(),
            // [策略] 心跳只决定温备发现主网关失联的延迟量级（失联判定本身取心跳间隔分布的 q(1−δ)，
            // 见 standby）；每秒一个约 40 字节的帧对带宽可以忽略，秒级发现满足"温备接管"的可用性目标
            heartbeat: Duration::from_secs(1),
            boot: 0,
        }
    }
}

struct Session {
    id: u64,
    out: Outbox,
    last_seen: Arc<AtomicU64>,
    /// 网关主动断开（吊销）时通知会话结束。
    stop: Arc<Stop>,
}

#[derive(Default)]
struct Stop {
    set: std::sync::atomic::AtomicBool,
    notify: Notify,
}

/// 工人握手时报来的参数。
struct Joined {
    corr: u64,
    boot: u64,
    weight: f64,
    rtt: Duration,
}

struct Gw {
    front: LedgerFront<Vec<u8>>,
    tokens: TokenStore,
    cfg: GatewayConfig,
    sessions: Mutex<HashMap<WorkerId, Session>>,
    next_session: AtomicU64,
}

/// 网关一侧的协调器。
#[derive(Clone)]
pub struct GatewayCoordinator {
    gw: Arc<Gw>,
}

impl GatewayCoordinator {
    /// `ledger` 以多服务器方式打开（`LedgerConfig::new(clock, false)`）。
    pub fn new(
        ledger: Arc<Ledger>,
        sink: Arc<dyn Deliver<Vec<u8>>>,
        tokens: TokenStore,
        cfg: GatewayConfig,
    ) -> Self {
        assert!(
            !ledger.is_local(),
            "GatewayCoordinator needs a cluster ledger"
        );
        let id: fn(Vec<u8>) -> Vec<u8> = |b| b;
        let front = LedgerFront::with(ledger, sink, "gateway", cfg.boot, Some((id, id)));
        Self {
            gw: Arc::new(Gw {
                front,
                tokens,
                cfg,
                sessions: Mutex::new(HashMap::new()),
                next_session: AtomicU64::new(1),
            }),
        }
    }

    pub fn ledger(&self) -> &Arc<Ledger> {
        self.gw.ledger()
    }

    pub fn tokens(&self) -> &TokenStore {
        &self.gw.tokens
    }

    /// 服务一条连接（h2c，或 TLS 握手之后），直到连接关闭。
    pub async fn serve<IO>(&self, io: IO) -> Result<(), Error>
    where
        IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        serve_connection(io, Arc::clone(&self.gw)).await
    }

    /// 处理到期并移出失联工人（用手动时钟的测试直接调用）。
    pub fn tick(&self) -> Tick {
        let t = self.ledger().tick();
        self.gw.on_expired(&t.expired);
        t
    }

    /// 按真实时钟运行计时器。
    pub async fn run_ticker(&self) {
        let gw = Arc::clone(&self.gw);
        Arc::clone(self.ledger())
            .run_ticker(move |e| gw.on_expired(e))
            .await;
    }

    /// 断开一个工人（例如吊销令牌之后）。
    pub fn disconnect(&self, worker: &WorkerId) {
        self.gw.evict(worker, true);
    }
}

impl Gw {
    fn ledger(&self) -> &Arc<Ledger> {
        self.front.ledger()
    }

    fn broadcast(&self, map: &HostMap) {
        for s in lock(&self.sessions).values() {
            let _ = s.out.send(0, &Msg::HostMap { map: map.clone() });
        }
    }

    /// 移出成员表；`stop` 时同时结束会话（吊销），否则连接保留，工人再说话时重新加入。
    fn evict(&self, worker: &WorkerId, stop: bool) {
        let removed = {
            let mut m = lock(&self.sessions);
            let s = m.remove(worker);
            // 在锁内置位：会话循环在同一把锁内检查它再决定是否重新加入
            if stop && let Some(s) = &s {
                s.stop.set.store(true, Ordering::Release);
                s.stop.notify.notify_one();
            }
            s
        };
        if removed.is_some() {
            let map = self.ledger().leave(worker);
            self.broadcast(&map);
        }
    }

    fn on_expired(&self, expired: &[Expired]) {
        for e in expired {
            let Some(w) = &e.worker else { continue };
            let silent = lock(&self.sessions)
                .get(w)
                .is_some_and(|s| s.last_seen.load(Ordering::Acquire) <= e.since.0);
            if silent {
                self.evict(w, false);
            }
        }
    }

    /// 工人连接的第一帧是 `Hello`，温备连接的第一帧是 `Ship`。
    async fn run(
        self: Arc<Self>,
        worker: WorkerId,
        mut reader: FrameReader<Incoming>,
        out: Outbox,
    ) {
        match reader.next().await {
            Ok(Some((_, Msg::Ship { from }))) => self.ship(from, reader, out).await,
            Ok(Some((
                corr,
                Msg::Hello {
                    worker: said,
                    boot,
                    weight,
                    rtt,
                    ..
                },
            ))) => {
                if said != worker {
                    let r = Err(Error::Rejected("worker id mismatch".into()));
                    let _ = out.send(corr, &Msg::Ack { r });
                    return;
                }
                let hello = Joined {
                    corr,
                    boot,
                    weight,
                    rtt,
                };
                self.serve_worker(worker, hello, reader, out).await;
            }
            _ => {}
        }
    }

    async fn serve_worker(
        self: Arc<Self>,
        worker: WorkerId,
        hello: Joined,
        mut reader: FrameReader<Incoming>,
        out: Outbox,
    ) {
        let l = self.ledger();
        let id = self.next_session.fetch_add(1, Ordering::Relaxed);
        let last_seen = Arc::new(AtomicU64::new(l.clock().now_ms()));
        let stop = Arc::new(Stop::default());
        let session = || Session {
            id,
            out: out.clone(),
            last_seen: Arc::clone(&last_seen),
            stop: Arc::clone(&stop),
        };
        lock(&self.sessions).insert(worker.clone(), session());
        let map = l.join(&worker, hello.weight, hello.rtt);
        let welcome = Msg::Welcome {
            term: l.term(),
            map: (*map).clone(),
        };
        let _ = out.send(hello.corr, &welcome);
        self.broadcast(&map);
        let origin = Origin {
            worker: worker.clone(),
            boot: hello.boot,
        };
        loop {
            let next = tokio::select! {
                biased;
                () = stop.notify.notified() => return,
                r = reader.next() => r,
            };
            let Ok(Some((corr, msg))) = next else { break };
            last_seen.store(l.clock().now_ms(), Ordering::Release);
            // 曾被判失联、连接却还在的工人重新说话：重新加入成员表（被主动断开的除外）
            let rejoin = {
                let mut m = lock(&self.sessions);
                if stop.set.load(Ordering::Acquire) {
                    return;
                }
                let absent = !m.contains_key(&worker);
                if absent {
                    m.insert(worker.clone(), session());
                }
                absent
            };
            if rejoin {
                let map = l.join(&worker, hello.weight, hello.rtt);
                self.broadcast(&map);
            }
            if let Some(reply) = self.dispatch(&worker, &origin, msg) {
                let _ = out.send(corr, &reply);
            }
        }
        let mine = lock(&self.sessions)
            .get(&worker)
            .is_some_and(|s| s.id == id);
        if mine {
            self.evict(&worker, false);
        }
    }

    fn dispatch(&self, w: &WorkerId, origin: &Origin, msg: Msg) -> Option<Msg> {
        let l = self.ledger();
        Some(match msg {
            Msg::Pull {
                free,
                rtt_hi,
                capacity,
            } => {
                l.set_rtt(w, rtt_hi);
                l.set_capacity(w, capacity);
                let lease = l.pull(Some(w), &free, rtt_hi).ok().flatten();
                Msg::Granted {
                    lease: lease.map(|x| WireLease {
                        task: x.task,
                        fence: x.fence(),
                        ttl: x.ttl,
                        work: (*x.work).clone(),
                        handoff: x.handoff,
                    }),
                }
            }
            Msg::Renew {
                task,
                fence,
                rtt_hi,
            } => Msg::Renewed {
                r: l.renew(task, fence, Some(w), rtt_hi),
            },
            Msg::Commit {
                task,
                fence,
                ran,
                result,
            } => Msg::Ack {
                r: self.front.commit_with(task, fence, result, Some(ran)),
            },
            Msg::Submit { task } => Msg::Submitted { r: l.submit(task) },
            Msg::Cancel { task } => Msg::Cancelled { r: l.cancel(task) },
            Msg::PriorsReq { scope } => Msg::Priors {
                tables: l.priors(scope).tables,
            },
            Msg::Stats { delta } => {
                let _ = l.merge_stats(origin, delta);
                return None;
            }
            Msg::Handoff { state } => {
                l.handoff(w, state);
                return None;
            }
            _ => return None,
        })
    }

    /// 日志传送：把 `from` 之后的记录持续推给温备；空闲时每个心跳间隔发一个空批。
    async fn ship(self: Arc<Self>, from: u64, mut reader: FrameReader<Incoming>, out: Outbox) {
        let Some(journal) = self.ledger().journal() else {
            let r = Err(Error::Rejected("gateway has no journal".into()));
            let _ = out.send(0, &Msg::Ack { r });
            return;
        };
        // 温备断开时读端结束，借此停止推送
        let gone = tokio::spawn(async move { while let Ok(Some(_)) = reader.next().await {} });
        let mut next = from;
        // [推导] 一批记录占帧上限的一半，留出每条 4 字节长度前缀与帧头的余量
        let budget = self.cfg.max_frame / 2;
        while !gone.is_finished() {
            let notified = journal.notified();
            let Ok(batch) = journal.read(next, budget) else {
                break;
            };
            if batch.is_empty() {
                tokio::select! {
                    () = notified => continue,
                    () = tokio::time::sleep(self.cfg.heartbeat) => {}
                }
            }
            // 温备落后到已截断的部分时，读出的第一条是最近一次快照的开始
            let start = batch.first().map_or(next, |(i, _)| *i);
            next = start + batch.len() as u64;
            let msg = Msg::Records {
                start,
                heartbeat: self.cfg.heartbeat,
                entries: batch.into_iter().map(|(_, raw)| raw).collect(),
            };
            if out.send(0, &msg).is_err() {
                break;
            }
        }
        gone.abort();
    }
}

impl Accept for Gw {
    fn tokens(&self) -> &TokenStore {
        &self.tokens
    }

    fn max_frame(&self) -> usize {
        self.cfg.max_frame
    }

    fn session(
        self: Arc<Self>,
        worker: WorkerId,
        reader: FrameReader<Incoming>,
        out: Outbox,
    ) -> BoxFuture<'static, ()> {
        Box::pin(self.run(worker, reader, out))
    }
}

/// 与单主机同一个账本前端：网关本身不执行任务，`pull` 总是 `None`；
/// 续约与提交按租约令牌照常处理，结果与完成标记写进同一条日志记录。
impl Coordinator<Vec<u8>> for GatewayCoordinator {
    fn submit(&self, task: Task) -> BoxFuture<'_, Result<TaskId, Error>> {
        self.gw.front.submit(task)
    }

    fn pull(&self, free: Resources) -> BoxFuture<'_, Option<Lease>> {
        self.gw.front.pull(free)
    }

    fn renew<'a>(&'a self, lease: &'a Lease) -> BoxFuture<'a, Result<Renewal, Error>> {
        self.gw.front.renew(lease)
    }

    fn commit<'a>(&'a self, lease: &'a Lease, result: Vec<u8>) -> BoxFuture<'a, Result<(), Error>> {
        self.gw.front.commit(lease, result)
    }

    fn cancel(&self, task: TaskId) -> BoxFuture<'_, Result<bool, Error>> {
        self.gw.front.cancel(task)
    }

    fn owner_of(&self, site: SiteKey) -> Owner {
        self.gw.front.owner_of(site)
    }

    fn publish_stats(&self, delta: StatsDelta) {
        self.gw.front.publish_stats(delta);
    }

    fn priors(&self, scope: Scope) -> Option<Priors> {
        self.gw.front.priors(scope)
    }
}
