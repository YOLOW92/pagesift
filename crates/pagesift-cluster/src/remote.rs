//! 工人一侧（`--role worker`）：经帧协议找网关。
//!
//! 一条连接上的请求与应答按关联号配对，可以并发；网关推送的成员表随时更新本地副本，
//! `owner_of` 用同一个 HRW 函数在本地计算。成员表变化时，本工人失去的站点立即把礼貌状态
//! 经网关交给新负责人（[`SiteStates`]）。
//!
//! 往返时间：每个请求的往返都记进 DDSketch，`q(1−δ)` 的置信上界随拉取、续约报给网关，
//! 用来定租约时长。样本还不够给出上界时用目前见过的最大往返（冷启动，包括握手那一次）。
//!
//! 栅栏：工人记住见过的最高网关任期，任期更低的网关（失联后又冒出来的旧主）发来的租约一律丢弃；
//! 换连新网关时把 [`RemoteCoordinator::term`] 作为 `min_term` 传给新连接。

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use hyper::body::Incoming;
use pagesift_stat::Delta;
use pagesift_stat::sketch::Sketch;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::oneshot;

use crate::clock::{Clock, ms};
use crate::coordinator::{BoxFuture, Coordinator};
use crate::frame::Msg;
use crate::hrw::HostMap;
use crate::ledger::renew_after;
use crate::transport::FrameReader;
use crate::transport::Outbox;
use crate::transport::client::{Dial, open};
use crate::types::{
    Deadline, Error, Lease, Owner, Priors, Renewal, Resources, Scope, SiteKey, SiteState,
    StatsDelta, Task, TaskId, lock,
};

/// 本工人负责的站点的礼貌状态（由礼貌模块实现）。
pub trait SiteStates: Send + Sync {
    /// 本工人上有礼貌状态的站点。
    fn sites(&self) -> Vec<SiteKey>;
    /// 交出一个站点的状态（并在本地删除）；`wait` 是新负责人至少还要等多久。
    fn export(&self, site: SiteKey) -> Option<SiteState>;
    /// 接收旧负责人交来的状态。
    fn import(&self, state: SiteState);
}

/// 工人的配置。
#[derive(Clone)]
pub struct WorkerConfig {
    pub dial: Dial,
    /// 相对容量（HRW 权重）。
    pub weight: f64,
    /// 本次启动的编号（统计增量去重用），通常取启动时刻。
    pub boot: u64,
    /// 只接受任期不低于它的网关（换连新网关时传上一条连接见过的任期）。
    pub min_term: u64,
    pub profile: Vec<u8>,
    pub delta: Delta,
    pub sites: Option<Arc<dyn SiteStates>>,
}

struct Inner {
    out: Outbox,
    clock: Arc<dyn Clock>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Msg>>>,
    corr: AtomicU64,
    map: RwLock<Arc<HostMap>>,
    term: AtomicU64,
    rtt: Mutex<Sketch>,
    rtt_hi_ms: AtomicU64,
    /// 本工人当前的准入容量（`f64` 的位），每次拉取报给网关。
    capacity: AtomicU64,
    priors: Mutex<HashMap<Scope, Priors>>,
    closed: AtomicBool,
    cfg: WorkerConfig,
}

pub struct RemoteCoordinator<A> {
    inner: Arc<Inner>,
    _result: PhantomData<fn(A)>,
}

impl<A: Into<Vec<u8>> + Send + 'static> RemoteCoordinator<A> {
    /// 在已建立的连接上握手（h2c 或 TLS 之后），返回可用的协调器。
    pub async fn connect<IO>(
        io: IO,
        cfg: WorkerConfig,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, Error>
    where
        IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let t0 = clock.now_ms();
        let mut s = open(io, &cfg.dial).await?;
        let handshake = Duration::from_millis(clock.now_ms() - t0);
        s.out.send(
            1,
            &Msg::Hello {
                worker: cfg.dial.worker.clone(),
                boot: cfg.boot,
                weight: cfg.weight,
                rtt: handshake,
                profile: cfg.profile.clone(),
            },
        )?;
        let (term, map) = match s.reader.next().await? {
            Some((_, Msg::Welcome { term, map })) => (term, map),
            Some((_, Msg::Ack { r: Err(e) })) => return Err(e),
            _ => return Err(Error::Rejected("no welcome".into())),
        };
        if term < cfg.min_term {
            return Err(Error::Fenced {
                current: crate::types::Fence {
                    term: cfg.min_term,
                    epoch: 0,
                },
            });
        }
        let mut rtt = Sketch::default();
        rtt.push(handshake.as_secs_f64());
        let inner = Arc::new(Inner {
            out: s.out,
            clock,
            pending: Mutex::new(HashMap::new()),
            // 关联号 1 已用于握手
            corr: AtomicU64::new(2),
            map: RwLock::new(Arc::new(map)),
            term: AtomicU64::new(term),
            rtt: Mutex::new(rtt),
            rtt_hi_ms: AtomicU64::new(ms(handshake)),
            capacity: AtomicU64::new(cfg.weight.to_bits()),
            priors: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            cfg,
        });
        tokio::spawn(Arc::clone(&inner).read_loop(s.reader));
        Ok(Self {
            inner,
            _result: PhantomData,
        })
    }

    /// 更新本工人的准入容量，随下一次拉取报给网关（开始时是握手时的权重）。
    pub fn set_capacity(&self, capacity: f64) {
        self.inner
            .capacity
            .store(capacity.to_bits(), Ordering::Relaxed);
    }

    /// 见过的最高网关任期。
    pub fn term(&self) -> u64 {
        self.inner.term.load(Ordering::Acquire)
    }

    pub fn host_map(&self) -> Arc<HostMap> {
        Arc::clone(&self.inner.map.read().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    /// 向网关请求某作用域的先验并更新本地缓存（[`Coordinator::priors`] 读的就是这份缓存）。
    pub async fn refresh_priors(&self, scope: Scope) -> Result<Priors, Error> {
        self.inner.refresh_priors(scope).await
    }

    /// 当前往返时间的 `q(1−δ)` 上界。
    pub fn rtt_hi(&self) -> Duration {
        self.inner.rtt_hi()
    }
}

impl Inner {
    fn rtt_hi(&self) -> Duration {
        Duration::from_millis(self.rtt_hi_ms.load(Ordering::Acquire))
    }

    fn observe_rtt(&self, d: Duration) {
        let mut s = lock(&self.rtt);
        s.push(d.as_secs_f64());
        let delta = self.cfg.delta;
        let hi = crate::ledger::upper_or_max(&s, delta);
        self.rtt_hi_ms
            .store(ms(Duration::from_secs_f64(hi)), Ordering::Release);
    }

    async fn rpc(&self, m: Msg) -> Result<Msg, Error> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        let corr = self.corr.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(corr, tx);
        if self.closed.load(Ordering::Acquire) {
            lock(&self.pending).remove(&corr);
            return Err(Error::Closed);
        }
        let t0 = self.clock.now_ms();
        if let Err(e) = self.out.send(corr, &m) {
            lock(&self.pending).remove(&corr);
            return Err(e);
        }
        let reply = rx.await.map_err(|_| Error::Closed)?;
        self.observe_rtt(Duration::from_millis(
            self.clock.now_ms().saturating_sub(t0),
        ));
        Ok(reply)
    }

    fn send(&self, m: &Msg) {
        let _ = self.out.send(0, m);
    }

    async fn read_loop(self: Arc<Self>, mut reader: FrameReader<Incoming>) {
        while let Ok(Some((corr, msg))) = reader.next().await {
            if corr != 0 {
                if let Some(tx) = lock(&self.pending).remove(&corr) {
                    let _ = tx.send(msg);
                }
                continue;
            }
            if let Msg::HostMap { map } = msg {
                self.on_host_map(map);
            }
        }
        self.closed.store(true, Ordering::Release);
        // 丢掉等待中的应答通道，挂起的请求得到 Closed
        lock(&self.pending).clear();
    }

    fn on_host_map(&self, map: HostMap) {
        let old = {
            let mut cur = self.map.write().unwrap_or_else(|e| e.into_inner());
            if map.epoch() <= cur.epoch() {
                return;
            }
            std::mem::replace(&mut *cur, Arc::new(map))
        };
        let Some(states) = &self.cfg.sites else {
            return;
        };
        let me = &self.cfg.dial.worker;
        let new = self.map.read().unwrap_or_else(|e| e.into_inner()).clone();
        for (site, from, to) in old.moved(&new, states.sites()) {
            if from.as_ref() == Some(me)
                && to.as_ref() != Some(me)
                && let Some(s) = states.export(site)
            {
                self.send(&Msg::Handoff { state: s });
            }
        }
    }

    async fn refresh_priors(&self, scope: Scope) -> Result<Priors, Error> {
        match self.rpc(Msg::PriorsReq { scope }).await? {
            Msg::Priors { tables } => {
                let p = Priors { tables };
                lock(&self.priors).insert(scope, p.clone());
                Ok(p)
            }
            _ => Err(Error::Transport("unexpected reply".into())),
        }
    }
}

fn unexpected() -> Error {
    Error::Transport("unexpected reply".into())
}

impl<A: Into<Vec<u8>> + Send + 'static> Coordinator<A> for RemoteCoordinator<A> {
    fn submit(&self, task: Task) -> BoxFuture<'_, Result<TaskId, Error>> {
        Box::pin(async move {
            match self.inner.rpc(Msg::Submit { task }).await? {
                Msg::Submitted { r } => r,
                _ => Err(unexpected()),
            }
        })
    }

    fn pull(&self, free: Resources) -> BoxFuture<'_, Option<Lease>> {
        Box::pin(async move {
            let i = &self.inner;
            let rtt_hi = i.rtt_hi();
            let capacity = f64::from_bits(i.capacity.load(Ordering::Relaxed));
            let pull = Msg::Pull {
                free,
                rtt_hi,
                capacity,
            };
            let Ok(Msg::Granted { lease: Some(l) }) = i.rpc(pull).await else {
                return None;
            };
            // 旧任期的网关发来的租约不接
            if l.fence.term < i.term.load(Ordering::Acquire) {
                return None;
            }
            let now = i.clock.now_ms();
            if let (Some(states), Some(h)) = (&i.cfg.sites, &l.handoff) {
                states.import(h.clone());
            }
            // 网关从发出时开始计时，租约到这里最多已过去一个往返上界
            let expires = Deadline(now + ms(l.ttl.saturating_sub(rtt_hi)));
            Some(Lease {
                task: l.task,
                epoch: l.fence.epoch,
                term: l.fence.term,
                expires,
                received: Deadline(now),
                ttl: l.ttl,
                renew_after: renew_after(l.ttl, rtt_hi),
                work: Arc::new(l.work),
                handoff: l.handoff,
            })
        })
    }

    fn renew<'a>(&'a self, lease: &'a Lease) -> BoxFuture<'a, Result<Renewal, Error>> {
        Box::pin(async move {
            let i = &self.inner;
            let msg = Msg::Renew {
                task: lease.task,
                fence: lease.fence(),
                rtt_hi: i.rtt_hi(),
            };
            match i.rpc(msg).await? {
                Msg::Renewed {
                    r: Ok(Renewal::Extended { ttl, .. }),
                } => Ok(Renewal::Extended {
                    expires: Deadline(i.clock.now_ms() + ms(ttl.saturating_sub(i.rtt_hi()))),
                    ttl,
                }),
                Msg::Renewed { r } => r,
                _ => Err(unexpected()),
            }
        })
    }

    fn commit<'a>(&'a self, lease: &'a Lease, result: A) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            let i = &self.inner;
            let ran = Duration::from_millis(i.clock.now_ms().saturating_sub(lease.received.0));
            let msg = Msg::Commit {
                task: lease.task,
                fence: lease.fence(),
                ran,
                result: result.into(),
            };
            match i.rpc(msg).await? {
                Msg::Ack { r } => r,
                _ => Err(unexpected()),
            }
        })
    }

    fn cancel(&self, task: TaskId) -> BoxFuture<'_, Result<bool, Error>> {
        Box::pin(async move {
            match self.inner.rpc(Msg::Cancel { task }).await? {
                Msg::Cancelled { r } => r,
                _ => Err(unexpected()),
            }
        })
    }

    fn owner_of(&self, site: SiteKey) -> Owner {
        self.host_map().owner_of(site, false)
    }

    fn publish_stats(&self, delta: StatsDelta) {
        self.inner.send(&Msg::Stats { delta });
    }

    fn priors(&self, scope: Scope) -> Option<Priors> {
        if let Some(p) = lock(&self.inner.priors).get(&scope) {
            return Some(p.clone());
        }
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let _ = inner.refresh_priors(scope).await;
        });
        None
    }
}
