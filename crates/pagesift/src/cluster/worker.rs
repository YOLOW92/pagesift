//! 工人一侧：连网关，循环"拉取 → 用本机的 `Scraper` 执行 → 提交结果"。
//!
//! - 并发：在执行的任务数小于统一准入的当前目标（[`Scraper::concurrency`]，限速器的 USL 控制器）
//!   时才拉取；
//! - 续约：每个任务在 `renew_after` 时续约，续约带回 `Cancel` 或被栅栏拒绝就停下这一页；
//! - 断线：在执行的任务全部放弃（网关在租约到期后重新分配），按网关列表依次重连，
//!   带上见过的最高任期，旧任期的网关拒绝连接；
//! - 空闲（拉不到任务）和重连失败时的等待见 [`Idle`]。

use std::sync::Arc;
use std::time::{Duration, Instant};

use pagesift_cluster::{
    Coordinator, Error as ClusterError, Lease, MonoClock, RemoteCoordinator, Renewal, Resources,
    WorkerConfig, renew_after,
};
use pagesift_stat::Delta;
use pagesift_stat::sketch::Sketch;
use tokio::task::JoinSet;

use super::net::{Link, dial};
use super::sites::BackoffStates;
use super::stats::Stats;
use super::task::{decode_task, encode_outcome};
use crate::Scraper;

type Remote = RemoteCoordinator<Vec<u8>>;

/// 工人的设置。
pub struct WorkerOptions {
    /// 网关地址（`host:port`），按顺序尝试：主网关在前、温备在后。
    pub gateways: Vec<String>,
    pub link: Link,
    /// 任务耗时的先验：本机抓取的总时限（冷启动时的典型任务时长上界）。
    pub task_prior: Duration,
}

/// 空闲时的重试间隔：二进制指数退避（R. M. Metcalfe, D. R. Boggs, "Ethernet", CACM 19(7), 1976），
/// 下限是到网关往返时间（本工人测的拉取耗时）`q(1−δ)` 的置信上界，样本不够时取见过的最大值——
/// 比一个往返更快地重试只会让请求在路上排队；
/// 上限是 `δ·T`，`T` 是本工人任务耗时 `q(1−δ)` 的置信上界：空闲时新任务最多晚一个间隔开始，
/// 这样多出的等待不超过一个任务耗时的 `δ`。`T` 的上界在样本够之前（Conover 1999 §3.2 的次序统计量
/// 上界，`pagesift_stat::hazard::quantile_bounds`）取先验（本机抓取的总时限，相当于一份观测）。
/// 拉到任务就回到下限。
struct Idle {
    delta: Delta,
    runs: Sketch,
    rtts: Sketch,
    prior: Duration,
    next: Option<Duration>,
}

impl Idle {
    fn new(prior: Duration) -> Self {
        Self {
            delta: Delta::DEFAULT,
            runs: Sketch::default(),
            rtts: Sketch::default(),
            prior,
            next: None,
        }
    }

    fn ceiling(&self) -> Duration {
        let d = self.delta.get();
        let t = pagesift_stat::hazard::quantile_bounds(&self.runs, 1.0 - d, self.delta)
            .2
            .map_or(self.prior, Duration::from_secs_f64);
        t.mul_f64(d)
    }

    fn observe_run(&mut self, ran: Duration) {
        self.runs.push(ran.as_secs_f64());
    }

    fn observe_rtt(&mut self, rtt: Duration) {
        self.rtts.push(rtt.as_secs_f64());
    }

    /// 往返时间 `q(1−δ)` 的上界；样本不够时取见过的最大值（不低估），没有样本时为 0。
    fn rtt_hi(&self) -> Duration {
        if self.rtts.count() == 0 {
            return Duration::ZERO;
        }
        let d = self.delta.get();
        let hi = pagesift_stat::hazard::quantile_bounds(&self.rtts, 1.0 - d, self.delta)
            .2
            .unwrap_or_else(|| self.rtts.max());
        Duration::from_secs_f64(hi.max(0.0))
    }

    fn reset(&mut self) {
        self.next = None;
    }

    /// 下一次等多久；`floor` 是当前的往返上界（没有连接时为 0，此时从上限开始）。
    fn wait(&mut self, floor: Duration) -> Duration {
        let cap = self.ceiling().max(floor);
        let w = match self.next {
            None if floor.is_zero() => cap,
            None => floor,
            Some(w) => (w * 2).min(cap),
        };
        self.next = Some(w);
        w
    }
}

/// 运行工人，直到 `shutdown` 完成。
pub async fn run(
    scraper: Arc<Scraper>,
    opts: WorkerOptions,
    shutdown: impl std::future::Future<Output = ()>,
) -> anyhow::Result<()> {
    anyhow::ensure!(!opts.gateways.is_empty(), "[cluster] gateways is empty");
    let clock = MonoClock::new();
    let boot = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let sites = Arc::new(BackoffStates::new(scraper.clone()));
    let ctx = Arc::new(Ctx {
        scraper: scraper.clone(),
        sites: sites.clone(),
        stats: Stats::attach(&scraper),
        id: opts.link.id.as_str().into(),
    });
    let mut idle = Idle::new(opts.task_prior);
    let mut term = 0u64;
    tracing::info!(
        "worker {}: token hash {} (register it under [[cluster.workers]] on the gateway)",
        opts.link.id,
        pagesift_cluster::token_hash(&opts.link.token).to_hex()
    );
    tokio::pin!(shutdown);
    let mut k = 0usize;
    loop {
        let addr = &opts.gateways[k % opts.gateways.len()];
        k += 1;
        let cfg = WorkerConfig {
            dial: opts.link.dial(addr),
            // HRW 权重 = 握手时本机统一准入的值；之后容量随每次拉取更新，权重不变（站点归属不抖动）
            weight: admitted(&scraper),
            boot,
            min_term: term,
            profile: Vec::new(),
            delta: Delta::DEFAULT,
            sites: Some(sites.clone()),
        };
        let connected = async {
            let io = dial(addr, &opts.link).await?;
            Remote::connect(io, cfg, clock.clone())
                .await
                .map_err(anyhow::Error::from)
        };
        let rc = tokio::select! {
            () = &mut shutdown => return Ok(()),
            r = connected => r,
        };
        match rc {
            Ok(rc) => {
                tracing::info!(
                    "worker {}: connected to {addr} (term {})",
                    opts.link.id,
                    rc.term()
                );
                idle.reset();
                let rc = Arc::new(rc);
                ctx.stats.connect(Some(rc.clone()));
                let stop = tokio::select! {
                    () = &mut shutdown => true,
                    () = work(&ctx, &rc, &mut idle) => false,
                };
                ctx.stats.connect(None);
                term = term.max(rc.term());
                if stop {
                    return Ok(());
                }
                tracing::warn!("worker {}: lost the connection to {addr}", opts.link.id);
            }
            Err(e) => tracing::warn!("worker {}: connecting to {addr}: {e:#}", opts.link.id),
        }
        // 列表里还有没试过的网关就马上试下一个；一轮都失败再等
        if k.is_multiple_of(opts.gateways.len()) {
            let floor = idle.rtt_hi();
            let w = idle.wait(floor);
            tokio::select! {
                () = &mut shutdown => return Ok(()),
                () = tokio::time::sleep(w) => {}
            }
        }
    }
}

/// 本机统一准入的当前值（`Engine::admitted_concurrency`），报给网关的准入容量和握手时的 HRW 权重。
fn admitted(scraper: &Scraper) -> f64 {
    pagesift_api::Engine::admitted_concurrency(scraper)
        .unwrap_or(1)
        .max(1) as f64
}

/// 一个工人进程里各任务共用的东西。
struct Ctx {
    scraper: Arc<Scraper>,
    sites: Arc<BackoffStates>,
    stats: Arc<Stats>,
    id: Arc<str>,
}

/// 一条连接上的工作循环，连接断开时返回（在执行的任务随 `JoinSet` 一起放弃）。
async fn work(ctx: &Arc<Ctx>, rc: &Arc<Remote>, idle: &mut Idle) {
    let scraper = &ctx.scraper;
    let mut running: JoinSet<Option<Duration>> = JoinSet::new();
    loop {
        while running.len() < scraper.concurrency().max(1) {
            // 准入的当前值随每次拉取报给网关（拉取就是工人的心跳：空闲时按退避间隔、忙时每完成一页）
            rc.set_capacity(admitted(scraper));
            let t0 = Instant::now();
            let pulled = rc.pull(Resources::default()).await;
            idle.observe_rtt(t0.elapsed());
            let Some(lease) = pulled else {
                break;
            };
            idle.reset();
            running.spawn(execute(ctx.clone(), rc.clone(), lease));
        }
        if rc.is_closed() {
            return;
        }
        let full = running.len() >= scraper.concurrency().max(1);
        // 满了只等有任务结束；没满说明刚才拉不到，按空闲退避再拉
        let wait = if full {
            Duration::ZERO
        } else {
            let floor = idle.rtt_hi();
            idle.wait(floor)
        };
        tokio::select! {
            Some(done) = running.join_next(), if !running.is_empty() => {
                if let Ok(Some(ran)) = done {
                    idle.observe_run(ran);
                }
            }
            () = tokio::time::sleep(wait), if !full => {}
        }
    }
}

/// 执行一个租约：抓取与续约并行，抓完提交。返回执行耗时（放弃时为 `None`）。
async fn execute(ctx: Arc<Ctx>, rc: Arc<Remote>, lease: Lease) -> Option<Duration> {
    let started = Instant::now();
    let scraper = &ctx.scraper;
    let (nonce, result, audit) = match decode_task(&lease.work.body) {
        Ok((nonce, task)) => {
            ctx.sites.note(&task.url);
            // 这个路径族第一次在本工人上出现：先向网关要先验（别的工人学到的）
            ctx.stats.prime(scraper, &rc, &task.url).await;
            // robots 模式是网关判定后放进任务的，工人照做，不看本机的 robots 设置
            let scrape = crate::audit::COLLECT.scope(std::cell::RefCell::new(Vec::new()), async {
                let r = scraper
                    .scrape_audited(task.audit_kind(), &task.url, &task.opts, task.mode())
                    .await;
                (r, crate::audit::COLLECT.with(|c| c.take()))
            });
            let keep = renewals(&rc, &lease);
            tokio::select! {
                (r, audit) = scrape => (nonce, r, audit),
                () = keep => return None,
            }
        }
        Err(e) => {
            // 任务体解不开（版本不一致）：如实报告，不让网关一直重派
            let nonce = super::task::nonce_of(&lease.work.body).unwrap_or_default();
            let err = pagesift_api::ApiError::new(
                pagesift_api::ErrorCode::Internal,
                format!("worker cannot decode the task: {e}"),
            );
            (nonce, Err(err), Vec::new())
        }
    };
    let ran = started.elapsed();
    match rc
        .commit(&lease, encode_outcome(nonce, &ctx.id, audit, &result))
        .await
    {
        Ok(()) | Err(ClusterError::Fenced { .. } | ClusterError::Cancelled) => {}
        Err(e) => tracing::debug!("commit of task {} failed: {e}", lease.task.0),
    }
    Some(ran)
}

/// 按租约续约，直到任务被取消、被栅栏拒绝或连接断开（这时返回，调用方停下这一页）。
///
/// 续约时刻沿用协调器的推导（`renew_after(ttl, rtt_hi)`）；`rtt_hi` 是协调器发租约时用的往返上界，
/// 由 `renew_after = ttl − 2·rtt_hi` 反解（进程内 `rtt_hi = 0` 时 `renew_after = ttl/2`，反解得 `ttl/4`，
/// 续约只会更早，不会过期）。
async fn renewals(rc: &Remote, lease: &Lease) {
    let rtt_hi = lease.ttl.saturating_sub(lease.renew_after) / 2;
    let mut wait = lease.renew_after;
    loop {
        tokio::time::sleep(wait).await;
        match rc.renew(lease).await {
            Ok(Renewal::Extended { ttl, .. }) => wait = renew_after(ttl, rtt_hi),
            Ok(Renewal::Cancel) | Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 冷启动上限 = δ × 先验；从往返下限起翻倍、封顶；拉到任务回到下限；耗时样本够了以后上限跟着实测走。
    #[test]
    fn idle_backoff_doubles_from_rtt_and_caps_at_delta_of_run_time() {
        let mut i = Idle::new(Duration::from_secs(20));
        let d = Delta::DEFAULT.get();
        assert_eq!(i.ceiling(), Duration::from_secs(20).mul_f64(d));
        let floor = Duration::from_millis(10);
        let seq: Vec<_> = (0..10).map(|_| i.wait(floor)).collect();
        assert_eq!(seq[0], floor);
        assert_eq!(seq[1], floor * 2);
        assert_eq!(*seq.last().unwrap(), i.ceiling());
        i.reset();
        assert_eq!(i.wait(floor), floor);
        // 没有连接时从上限开始
        i.reset();
        assert_eq!(i.wait(Duration::ZERO), i.ceiling());
        for _ in 0..500 {
            i.observe_run(Duration::from_millis(200));
        }
        let c = i.ceiling();
        assert!(
            c < Duration::from_millis(20) && c >= Duration::from_millis(9),
            "{c:?}"
        );
    }
}
