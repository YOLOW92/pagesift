//! 渲染和浏览器池里的等待上限，从观测到的完成时间学出来。
//!
//! 每个原来写死的等待上限（网络静默最多等多久、壳页面宽限、截图等 `load`、iframe 抓取、
//! 启动浏览器、空闲浏览器保留多久……）变成一个 [`WaitCap`]：
//!
//! - **先验**：0.81 的现值（先验包 v0）。没有数据、或数据还不显著时，上限就是它，行为与 0.81 相同。
//! - **缩短**：等待按"占本次先验上限的比例"记进 [`Remaining`]（完成 = 事件，到上限或被别的条件结束 = 截尾）。
//!   已等到 `t` 仍在等的里面，先验上限前还能完成的比例的 Wilson 上界 `< δ` 时，把上限收到 `t`
//!   （[`Remaining::give_up_at`]）。受影响的等待不超过全部的 `δ·Ŝ(t)`。
//! - **放回**：学到的上限只在数据显著时变化；当前上限被数据显著否定（[`Remaining::too_early`]）时放回到
//!   新的放弃时刻（没有就回到先验）。
//! - **探索**：每 `⌈1/δ⌉` 次等待有一次用先验上限，这样学到的上限之后的完成时间仍然有样本，
//!   上限才放得回来（截尾在学到的上限上的数据本身看不出"等久一点有没有用"）。
//! - **不超过先验**：先验之后的完成时间从来没被观测过（0.81 就在那里截断），无法识别；
//!   这也保证速度不比 0.81 慢。
//!
//! 窗口：样本累进当前窗口；发布一个新上限后开新窗口，旧窗口不再拖住估计（同
//! `pagesift_stat::window` 的约定）。重新计算按 `√n` 的节奏：新增样本数到 `√(n)` 量级时估计才可能
//! 移动一个标准误（`SE ∝ 1/√n`），更频繁地算没有新信息。
//!
//! 并发：样本按线程分片记（每片一把锁，分片数随核数，按 128 字节对齐独占缓存行）；
//! 记样本用 `try_lock`，抢不到就丢掉这一个样本——丢弃与样本取值无关，不引入偏差，热路径从不等锁。
//! 读上限是一次原子读。

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use pagesift_stat::Delta;
use pagesift_stat::hazard::Remaining;

/// 独占缓存行，免得相邻分片的锁在多 CCD 上互相颠簸（0.70 在 192 vCPU 上的实测结论）。
#[repr(align(128))]
#[derive(Default)]
struct Pad<T>(T);

/// 按线程分片的统计。分片数取不小于核数的 2 的幂。
pub struct Sharded<T> {
    shards: Box<[Pad<Mutex<T>>]>,
}

fn shard_count() -> usize {
    std::thread::available_parallelism()
        .map_or(1, usize::from)
        .next_power_of_two()
}

/// 当前线程的分片号：线程第一次用时从全局计数器领一个，之后是线程本地读。
fn thread_slot() -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    thread_local! {
        static SLOT: usize = NEXT.fetch_add(1, Ordering::Relaxed);
    }
    SLOT.with(|s| *s)
}

impl<T: Default> Default for Sharded<T> {
    fn default() -> Self {
        Self {
            shards: (0..shard_count()).map(|_| Pad::default()).collect(),
        }
    }
}

impl<T> Sharded<T> {
    pub fn len(&self) -> usize {
        self.shards.len()
    }

    pub fn is_empty(&self) -> bool {
        self.shards.is_empty()
    }

    /// 在当前线程的分片上做 `f`；分片正被别人用时返回 `None`（调用方丢掉这个样本）。
    pub fn try_with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let s = &self.shards[thread_slot() & (self.shards.len() - 1)].0;
        let mut g = s.try_lock().ok()?;
        Some(f(&mut g))
    }

    /// 逐个分片加锁访问（一次只持有一把锁，不会死锁）。只在低频的汇总里用。
    pub fn for_each(&self, mut f: impl FnMut(&mut T)) {
        for s in self.shards.iter() {
            let mut g: MutexGuard<'_, T> = s.0.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut g);
        }
    }
}

/// 每 `⌈1/δ⌉` 次里的第一次返回 `true`（线程本地计数，不共享缓存行）。
pub fn explore_now(delta: Delta) -> bool {
    thread_local! {
        static COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    let every = (1.0 / delta.get()).ceil() as u64;
    COUNT.with(|c| {
        let n = c.get();
        c.set(n.wrapping_add(1));
        n % every.max(1) == 0
    })
}

#[derive(Default)]
struct Window {
    data: Remaining,
    n: u64,
    next: u64,
}

/// 一类等待的上限：先验是 0.81 的现值，数据显著时才收紧或放回（见模块说明）。
pub struct WaitCap {
    prior: Duration,
    /// 学到的上限占先验的比例（`f64` 的位），`1.0` = 先验。
    frac: Pad<AtomicU64>,
    busy: Pad<AtomicBool>,
    shards: Sharded<Window>,
    delta: Delta,
}

impl std::fmt::Debug for WaitCap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaitCap")
            .field("prior", &self.prior)
            .field("fraction", &self.fraction())
            .finish()
    }
}

impl WaitCap {
    pub fn new(prior: Duration) -> Self {
        Self {
            prior,
            frac: Pad(AtomicU64::new(1f64.to_bits())),
            busy: Pad(AtomicBool::new(false)),
            shards: Sharded::default(),
            delta: Delta::DEFAULT,
        }
    }

    pub fn prior(&self) -> Duration {
        self.prior
    }

    /// 学到的上限占先验的比例，`(0, 1]`。
    pub fn fraction(&self) -> f64 {
        f64::from_bits(self.frac.0.load(Ordering::Relaxed))
    }

    /// 本次等待用的上限（先验固定时）。
    pub fn cap(&self) -> Duration {
        self.cap_for(self.prior)
    }

    /// 本次等待用的上限，先验随请求变化时（例如按请求时限算的耐心）按同一比例缩放。
    /// 每 `⌈1/δ⌉` 次用一次先验本身（探索）。
    pub fn cap_for(&self, prior: Duration) -> Duration {
        if explore_now(self.delta) {
            return prior;
        }
        prior.mul_f64(self.fraction())
    }

    /// 等了 `waited` 拿到了结果；`prior` 是本次等待的先验上限。
    pub fn done(&self, waited: Duration, prior: Duration) {
        self.observe(waited, prior, true);
    }

    /// 等了 `waited` 没拿到结果就结束了（到上限、被别的条件打断）。
    pub fn cut(&self, waited: Duration, prior: Duration) {
        self.observe(waited, prior, false);
    }

    fn observe(&self, waited: Duration, prior: Duration, done: bool) {
        if prior.is_zero() {
            return;
        }
        let u = (waited.as_secs_f64() / prior.as_secs_f64()).min(1.0);
        let shards = self.shards.len() as f64;
        let due = self.shards.try_with(|w| {
            if done && u < 1.0 {
                w.data.done(u);
            } else {
                w.data.cut(u);
            }
            w.n += 1;
            if w.n >= w.next {
                // 全部分片合计约 n·分片数 个样本；再攒 √(n·分片数) 个才可能移动一个标准误
                w.next = w.n + ((w.n as f64 * shards).sqrt().ceil() as u64).max(1);
                true
            } else {
                false
            }
        });
        if due == Some(true) {
            self.recompute();
        }
    }

    /// 汇总各分片，显著时发布新上限并开新窗口。同一时间只有一个线程在算。
    fn recompute(&self) {
        if self
            .busy
            .0
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let mut all = Remaining::default();
        self.shards.for_each(|w| all.merge(&w.data));
        let cur = self.fraction();
        let candidate = all.give_up_at(1.0, self.delta).unwrap_or(1.0);
        // 收紧：放弃时刻本身就是显著的；放宽：当前上限被数据显著否定才放
        let tighten = candidate < cur;
        let loosen = candidate > cur && all.too_early(cur, 1.0, self.delta);
        let publish = (tighten || loosen).then_some(candidate);
        if let Some(f) = publish {
            tracing::debug!(
                prior_ms = self.prior.as_millis() as u64,
                from = cur,
                to = f,
                samples = all.count(),
                "learned wait cap"
            );
            self.frac.0.store(f.to_bits(), Ordering::Relaxed);
            self.shards.for_each(|w| *w = Window::default());
        }
        self.busy.0.store(false, Ordering::Release);
    }
}

/// 渲染一页时用到的各类等待上限，按内核（池）各一份。
#[derive(Debug)]
pub struct PageWaits {
    /// DOMContentLoaded 之后等网络静默的上限（0.81：`NET_IDLE_CAP` 5 s）。
    pub net_idle: WaitCap,
    /// 网络静默后页面仍几乎没有文字时再等的宽限（0.81：`SHELL_GRACE` 1.5 s）。
    pub shell_grace: WaitCap,
    /// 截图前等 `load` 的上限（0.81：`LOAD_CAP` 10 s）。
    pub load: WaitCap,
    /// 裸跳转页等内容出现的耐心，先验按请求算（0.81：`max(HTTP 耗时 × 200, timeout/2)`）。
    pub redirect: WaitCap,
    /// 单个 iframe 抓取的上限（0.81：`FRAME_TIMEOUT` 5 s）。
    pub frame: WaitCap,
}

/// [策略] 先验包 v0：0.81 的现值
pub(crate) const NET_IDLE_PRIOR: Duration = Duration::from_secs(5);
/// [策略] 先验包 v0：0.81 的现值
pub(crate) const SHELL_GRACE_PRIOR: Duration = Duration::from_millis(1500);
/// [策略] 先验包 v0：0.81 的现值
pub(crate) const LOAD_PRIOR: Duration = Duration::from_secs(10);
/// [策略] 先验包 v0：0.81 的现值
pub(crate) const FRAME_PRIOR: Duration = Duration::from_secs(5);
/// 页面跳转后等新的执行上下文的上限。
/// [策略] 先验包 v0：0.81 的现值（每 100 ms 重试一次、最多 50 次）
pub(crate) const CONTEXT_PRIOR: Duration = Duration::from_secs(5);

impl Default for PageWaits {
    fn default() -> Self {
        Self {
            net_idle: WaitCap::new(NET_IDLE_PRIOR),
            shell_grace: WaitCap::new(SHELL_GRACE_PRIOR),
            load: WaitCap::new(LOAD_PRIOR),
            redirect: WaitCap::new(Duration::ZERO),
            frame: WaitCap::new(FRAME_PRIOR),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_stat::rng::Rng;

    /// 冷启动等于先验；只有显著的数据才收紧；收紧后数据变了能放回去。
    #[test]
    fn cold_start_tightens_only_on_evidence_and_recovers() {
        let prior = Duration::from_secs(5);
        let cap = WaitCap::new(prior);
        assert_eq!(cap.fraction(), 1.0);
        // 少量样本：不显著，不动
        for _ in 0..10 {
            cap.cut(prior, prior);
        }
        assert_eq!(cap.fraction(), 1.0);

        // 九成页面 0.2 s 内就绪，一成永远不就绪（长轮询）
        let mut rng = Rng::seed(11);
        for _ in 0..20_000 {
            let c = cap.cap();
            if rng.uniform() < 0.9 {
                let t = Duration::from_secs_f64(-0.2 * rng.uniform().max(1e-12).ln());
                if t < c {
                    cap.done(t, prior)
                } else {
                    cap.cut(c, prior)
                }
            } else {
                cap.cut(c, prior);
            }
        }
        let tight = cap.fraction();
        assert!(tight < 0.5, "{tight}");

        // 页面变慢：完成时间铺满到先验上限，探索样本应把上限放回去
        for _ in 0..200_000 {
            let c = cap.cap();
            let t = prior.mul_f64(rng.uniform());
            if t < c {
                cap.done(t, prior)
            } else {
                cap.cut(c, prior)
            }
        }
        assert!(
            cap.fraction() > tight * 1.5,
            "{} vs {tight}",
            cap.fraction()
        );
    }

    /// 阶跃负载下不来回振荡：平稳数据上发布次数是对数级的。
    #[test]
    fn stable_data_does_not_oscillate() {
        let prior = Duration::from_secs(1);
        let cap = WaitCap::new(prior);
        let mut rng = Rng::seed(12);
        let mut changes = 0;
        let mut last = cap.fraction();
        for _ in 0..50_000 {
            let c = cap.cap();
            let t = Duration::from_secs_f64(-0.05 * rng.uniform().max(1e-12).ln());
            if t < c {
                cap.done(t, prior)
            } else {
                cap.cut(c, prior)
            }
            if cap.fraction() != last {
                changes += 1;
                last = cap.fraction();
            }
        }
        assert!(changes < 40, "{changes}");
    }

    #[test]
    fn exploration_rate_is_delta() {
        let n = 10_000;
        let hits = (0..n).filter(|_| explore_now(Delta::DEFAULT)).count();
        assert_eq!(hits, n / 20);
    }
}
