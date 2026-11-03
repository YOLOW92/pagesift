//! 渲染的在线统计：自动升级的导航耐心、早停时机、渲染预算，以及内核启动失败后的重试间隔、
//! 一次渲染尝试至少要多少时间、没给 `timeout` 时的渲染时限。
//!
//! 上界都取对应耗时分布的 `q(1−δ)`，直接来自 DDSketch，并取它的 `1−δ` 无分布
//! 置信上界（Conover 1999 §3.2 的次序统计量界，`pagesift_stat::hazard::quantile_bounds`）：
//! - 导航耐心 = 成功导航耗时的 `q(1−δ)` 上界；
//! - 早停时机 = 页面第一次出现"比 HTTP 更多正文"的耗时的 `q(1−δ)` 上界；
//! - 渲染预算 = 真正带来增益的渲染总耗时的 `q(1−δ)` 上界。
//!
//! 取代 0.81 的"均值 + 3σ、均值标准误 ≤ 10% 才启用"（`SIGMA`、`MAX_RELATIVE_ERROR`）：
//! - 样本够不够不再看固定的 10%：分位数的置信上界在样本里存在、且窗口内分位数的相对标准误 `< δ`
//!   时才发布（`pagesift_stat::hazard::WindowQuantile`，与 `window::WindowMean` 同一约定）；
//!   之前返回 `None`，按原有流程跑（冷启动与 0.81 相同：不设上限）。
//! - 不用累计均值：每个窗口发布后开新窗口，只用最近一个完整窗口；新值与当前值的差超过
//!   `z(1−δ)·SE` 才替换。
//! - 不假设正态：渲染耗时重尾，均值 + 3σ 在重尾下既不对应固定的分位数，也被少数极慢页面拉高。
//!
//! 并发：样本按线程分片记（`pagesift_cdp::learn::Sharded`，`try_lock` 抢不到就丢这一个样本），
//! 读上界是一次原子读；汇总按 `√n` 的节奏、一次只有一个线程做。

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use pagesift_cdp::EngineKind;
use pagesift_cdp::learn::{Sharded, WaitCap};
use pagesift_flow::{Admission, TabPermit};
use pagesift_stat::Delta;
use pagesift_stat::hazard::WindowQuantile;
use pagesift_stat::shrink::significant;
use pagesift_stat::survival::Survival;
use pagesift_stat::window::WindowRatio;

/// 内核启动失败后多久再试的先验（先验包 v0：0.81 的现值）。
/// [策略] 先验包 v0：0.81 的现值
pub(super) const LAUNCH_RETRY: Duration = Duration::from_secs(600);
/// 没给 `timeout` 时一次渲染（含换内核）时限的先验和上限（先验包 v0：0.81 的现值）。
/// [策略] 先验包 v0：0.81 的现值
pub(super) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// 剩余时间少于这么多就不再开始新的渲染尝试：先验（先验包 v0：0.81 的现值）。
/// [策略] 先验包 v0：0.81 的现值
pub(super) const MIN_ATTEMPT: Duration = Duration::from_millis(500);

#[derive(Clone)]
struct Shard {
    window: WindowQuantile,
    n: u64,
    next: u64,
}

/// 按线程分片、按精度截窗的分位数；发布值放在原子量里。
struct Quantile {
    p: f64,
    /// 用发布窗口的上界（`true`）还是下界。
    upper: bool,
    shards: Sharded<Option<Shard>>,
    /// 当前发布值（秒，`f64` 的位）；NaN 表示还没有。
    value: AtomicU64,
    busy: AtomicBool,
}

impl Quantile {
    fn new(p: f64, upper: bool) -> Self {
        Self {
            p,
            upper,
            shards: Sharded::default(),
            value: AtomicU64::new(f64::NAN.to_bits()),
            busy: AtomicBool::new(false),
        }
    }

    fn get(&self) -> Option<Duration> {
        let v = f64::from_bits(self.value.load(Ordering::Relaxed));
        v.is_finite().then(|| Duration::from_secs_f64(v.max(0.0)))
    }

    fn push(&self, x: Duration) {
        let (p, shards) = (self.p, self.shards.len() as f64);
        let due = self.shards.try_with(|s| {
            let s = s.get_or_insert_with(|| Shard {
                window: WindowQuantile::new(p),
                n: 0,
                next: 1,
            });
            s.window.add(x.as_secs_f64());
            s.n += 1;
            if s.n >= s.next {
                // 全部分片合计约 n·分片数 个样本；再攒 √(n·分片数) 个，估计才可能移动一个标准误
                s.next = s.n + ((s.n as f64 * shards).sqrt().ceil() as u64).max(1);
                true
            } else {
                false
            }
        });
        if due == Some(true) {
            self.recompute();
        }
    }

    fn recompute(&self) {
        if self
            .busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let delta = Delta::DEFAULT;
        let mut all = WindowQuantile::new(self.p);
        self.shards.for_each(|s| {
            if let Some(s) = s {
                all.merge(&s.window);
            }
        });
        if let Some(p) = all.ready(delta) {
            let new = if self.upper { p.upper } else { p.lower };
            let cur = f64::from_bits(self.value.load(Ordering::Relaxed));
            // 置信界宽度换算成标准误：(上界 − 下界) / 2z
            let se = (p.upper - p.lower) / (2.0 * delta.z());
            if !cur.is_finite() || significant(cur, new, se, delta) {
                self.value.store(new.to_bits(), Ordering::Relaxed);
            }
            // 开新窗口
            self.shards.for_each(|s| {
                if let Some(s) = s {
                    s.window = WindowQuantile::new(self.p);
                }
            });
        }
        self.busy.store(false, Ordering::Release);
    }
}

/// 内核启动故障的持续时间（Kaplan–Meier，含仍在持续的故障作截尾），决定多久再试一次
/// （恢复概率达到 `1−δ` 的时刻，`pagesift_stat::decide::retry_after`）。
#[derive(Default)]
struct Outages {
    /// 已结束的故障：开始到恢复的时长。
    ended: Survival,
    /// 每个内核（按内核链下标）正在持续的故障从何时开始。
    open: Vec<Option<Instant>>,
}

/// 所有抓取共享的一份渲染统计。
pub struct RenderStats {
    /// 成功完成导航的耗时（没等到 DOMContentLoaded 的页面不进这里）。
    navigation: Quantile,
    /// 真正比 HTTP 多出正文的渲染总耗时。
    useful_total: Quantile,
    /// 没有带来增益的渲染总耗时。
    useless_total: Quantile,
    /// 从开始等待就绪到第一次看到比 HTTP 更多正文的耗时。
    first_content: Quantile,
    /// 每种内核拿到文档（排队 + 建页 + 导航）的耗时的 `q(δ)` 下界，按 [`EngineKind`] 顺序。
    /// [推导] 内核种类数，等于 `EngineKind` 的变体数
    first_byte: [Quantile; 3],
    /// 没给 `timeout` 时的渲染时限（先验 [`DEFAULT_TIMEOUT`]，见 `pagesift_cdp::learn::WaitCap`）。
    default_timeout: WaitCap,
    outages: Mutex<Outages>,
    any_outage: AtomicBool,
    /// 当前的启动重试间隔（秒，`f64` 的位）。
    launch_retry: AtomicU64,
    /// 浏览器标签名额；上限 = 各内核池当前的标签容量之和。
    tabs: Admission,
    tab_limit: AtomicUsize,
    /// 提取耗时 / HTML 字节（Cochran 比值估计，按精度截窗，按线程分片、各片独立发布）。
    extract_rate: Sharded<WindowRatio>,
    /// 最近发布的每字节提取秒数（`f64` 的位）；NaN = 还没有。
    extract_per_byte: AtomicU64,
}

impl Default for RenderStats {
    fn default() -> Self {
        let d = Delta::DEFAULT.get();
        Self {
            navigation: Quantile::new(1.0 - d, true),
            useful_total: Quantile::new(1.0 - d, true),
            useless_total: Quantile::new(1.0 - d, true),
            first_content: Quantile::new(1.0 - d, true),
            first_byte: std::array::from_fn(|_| Quantile::new(d, false)),
            default_timeout: WaitCap::new(DEFAULT_TIMEOUT),
            outages: Mutex::new(Outages::default()),
            any_outage: AtomicBool::new(false),
            launch_retry: AtomicU64::new(LAUNCH_RETRY.as_secs_f64().to_bits()),
            tabs: Admission::new(1),
            tab_limit: AtomicUsize::new(1),
            extract_rate: Sharded::default(),
            extract_per_byte: AtomicU64::new(f64::NAN.to_bits()),
        }
    }
}

fn engine_slot(e: EngineKind) -> usize {
    match e {
        EngineKind::Lightpanda => 0,
        EngineKind::Moli => 1,
        EngineKind::Chrome => 2,
    }
}

impl RenderStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次完成的浏览器渲染。`first_content` 只在页面文字确实超过 HTTP 正文时才有值。
    pub fn note_render(
        &self,
        navigation: Duration,
        wait: Duration,
        total: Duration,
        first_content: Option<Duration>,
        gained: bool,
    ) {
        let _ = wait;
        self.navigation.push(navigation);
        if gained {
            self.useful_total.push(total);
            if let Some(t) = first_content {
                self.first_content.push(t);
            }
        } else {
            self.useless_total.push(total);
        }
    }

    /// 自动升级等主框架导航提交的上限。样本不足时返回 `None`，按完整超时等。
    pub fn nav_patience(&self) -> Option<Duration> {
        self.navigation.get()
    }

    /// 页面文字连续变化时，最晚什么时候抓 DOM 做一次"是否还有增益"的探针。
    /// 样本不足时返回 `None`，只在网络静默时探针。
    pub fn early_stop_after(&self) -> Option<Duration> {
        self.first_content.get()
    }

    /// 自动升级的安全预算：正常增益渲染的耗时上界。样本不足时返回 `None`。
    pub fn budget(&self) -> Option<Duration> {
        self.useful_total.get().or_else(|| self.useless_total.get())
    }

    /// 一个内核拿到文档的耗时（排队 + 建页 + 导航）。
    pub fn note_first_byte(&self, engine: EngineKind, t: Duration) {
        self.first_byte[engine_slot(engine)].push(t);
    }

    /// 剩余时间少于这么多就不再用这个内核开始新的尝试：拿到文档耗时的 `q(δ)` 的置信下界
    /// （至多 `δ` 的尝试能在这么短的时间里拿到文档）；样本不足时用先验 [`MIN_ATTEMPT`]。
    pub fn min_attempt(&self, engine: EngineKind) -> Duration {
        self.first_byte[engine_slot(engine)]
            .get()
            .unwrap_or(MIN_ATTEMPT)
    }

    /// 没给 `timeout` 时这次渲染的时限（学到的，含探索；先验和上限是 [`DEFAULT_TIMEOUT`]）。
    pub fn default_timeout(&self) -> Duration {
        self.default_timeout.cap()
    }

    /// 没给 `timeout` 的渲染结束了：`finished` 为真是拿到了结果，假是到时限放弃。
    pub fn note_default_timeout(&self, elapsed: Duration, finished: bool) {
        if finished {
            self.default_timeout.done(elapsed, DEFAULT_TIMEOUT);
        } else {
            self.default_timeout.cut(elapsed, DEFAULT_TIMEOUT);
        }
    }

    /// 拿一个浏览器标签名额。`capacity` 是各内核池当前的标签容量之和（各池按 USL 和实测内存调），
    /// 变了才改名额上限；冷启动时它就是 0.81 各池"浏览器数 × 标签数"之和。
    pub async fn tab_permit(&self, capacity: usize) -> TabPermit {
        let capacity = capacity.max(1);
        if self.tab_limit.swap(capacity, Ordering::Relaxed) != capacity {
            self.tabs.set_tab_limit(capacity);
        }
        self.tabs.tab().await
    }

    /// 浏览器标签的利用率（统一准入门面的在用 / 上限）。
    pub fn tab_utilization(&self) -> f64 {
        self.tabs.tab_utilization()
    }

    /// 记一次 HTTP 正文提取：HTML 字节数和耗时。
    pub fn note_extract(&self, bytes: usize, t: Duration) {
        if bytes == 0 {
            return;
        }
        let published = self
            .extract_rate
            .try_with(|w| w.push(bytes as f64, t.as_secs_f64(), Delta::DEFAULT))
            .flatten();
        if let Some(r) = published {
            self.extract_per_byte.store(r.to_bits(), Ordering::Relaxed);
        }
    }

    /// 这么大的 HTML 预计要提取多久；样本还不够时返回 `None`。
    pub fn extract_estimate(&self, bytes: usize) -> Option<Duration> {
        let r = f64::from_bits(self.extract_per_byte.load(Ordering::Relaxed));
        (r.is_finite() && r > 0.0).then(|| Duration::from_secs_f64(r * bytes as f64))
    }

    /// 内核启动失败后多久再试。
    pub fn launch_retry(&self) -> Duration {
        Duration::from_secs_f64(f64::from_bits(self.launch_retry.load(Ordering::Relaxed)))
    }

    /// 内核链第 `i` 个内核启动失败：开始（或延续）一段故障。
    pub fn note_launch_failure(&self, i: usize) {
        let mut o = self.outages.lock().unwrap_or_else(|e| e.into_inner());
        if o.open.len() <= i {
            o.open.resize(i + 1, None);
        }
        o.open[i].get_or_insert_with(Instant::now);
        self.any_outage.store(true, Ordering::Relaxed);
    }

    /// 内核链第 `i` 个内核成功渲染了：它若在故障中，记一次恢复并重算重试间隔。
    /// 没有故障时只是一次原子读。
    pub fn note_launch_ok(&self, i: usize) {
        if !self.any_outage.load(Ordering::Relaxed) {
            return;
        }
        let mut o = self.outages.lock().unwrap_or_else(|e| e.into_inner());
        let Some(start) = o.open.get_mut(i).and_then(Option::take) else {
            return;
        };
        o.ended.event(start.elapsed().as_secs_f64());
        self.any_outage
            .store(o.open.iter().any(Option::is_some), Ordering::Relaxed);
        let mut all = o.ended.clone();
        for s in o.open.iter().flatten() {
            all.censor(s.elapsed().as_secs_f64());
        }
        drop(o);
        let delta = Delta::DEFAULT;
        let cur = self.launch_retry().as_secs_f64();
        // 当前间隔处的恢复概率与它的 Greenwood 标准误；与目标 1−δ 的差显著时才换
        let (s, se) = all.survival_with_se(cur);
        if let Some(t) = pagesift_stat::decide::retry_after(&all, delta)
            && significant(1.0 - delta.get(), 1.0 - s, se, delta)
            && t > 0.0
        {
            self.launch_retry.store(t.to_bits(), Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_stat::rng::Rng;

    #[test]
    fn stats_stay_disabled_until_the_quantile_bound_is_precise() {
        let stats = RenderStats::new();
        assert!(stats.nav_patience().is_none());
        assert!(stats.early_stop_after().is_none());
        assert!(stats.budget().is_none());
        // 0.81 在 40 个低方差样本后就启用；q(0.95) 的置信上界此时还不存在于样本内
        for i in 0..40 {
            stats.note_render(
                Duration::from_millis(50 + i),
                Duration::from_millis(100 + i),
                Duration::from_millis(200 + i),
                Some(Duration::from_millis(20 + i)),
                true,
            );
        }
        assert!(stats.nav_patience().is_none());
    }

    /// 合成对数正态耗时：发布的上界落在真实 q(0.95) 附近（不低于它太多），且远低于最大值。
    #[test]
    fn upper_bound_tracks_the_quantile() {
        let stats = RenderStats::new();
        let mut rng = Rng::seed(3);
        let mut xs = Vec::new();
        for _ in 0..50_000 {
            let t = 0.3 * (0.8 * rng.normal()).exp();
            xs.push(t);
            stats.note_render(
                Duration::from_secs_f64(t),
                Duration::ZERO,
                Duration::from_secs_f64(t),
                None,
                true,
            );
        }
        let exact = 0.3 * (0.8 * 1.6449f64).exp();
        let got = stats.nav_patience().expect("enough samples").as_secs_f64();
        assert!(got > exact * 0.9 && got < exact * 1.3, "{got} vs {exact}");
        assert!(stats.budget().is_some());
        assert!(stats.early_stop_after().is_none());
    }

    #[test]
    fn cold_start_equals_the_081_values() {
        let stats = RenderStats::new();
        assert_eq!(stats.launch_retry(), LAUNCH_RETRY);
        assert_eq!(stats.min_attempt(EngineKind::Chrome), MIN_ATTEMPT);
        // 探索之外的时限就是先验
        let t: Vec<_> = (0..40).map(|_| stats.default_timeout()).collect();
        assert!(t.iter().all(|&d| d == DEFAULT_TIMEOUT));
        // 故障只开始、从未恢复：没有恢复的数据，不改间隔
        stats.note_launch_failure(1);
        stats.note_launch_ok(0);
        assert_eq!(stats.launch_retry(), LAUNCH_RETRY);
    }

    /// 很多次故障都在约 1 分钟内恢复：重试间隔收到恢复概率达到 1−δ 的时刻附近。
    #[test]
    fn launch_retry_follows_recovery_times() {
        let stats = RenderStats::new();
        let mut o = stats.outages.lock().unwrap();
        for i in 0..200 {
            o.ended.event(30.0 + (i % 60) as f64);
        }
        drop(o);
        stats.note_launch_failure(0);
        stats.note_launch_ok(0);
        let r = stats.launch_retry().as_secs_f64();
        assert!(r > 60.0 && r < 100.0, "{r}");
    }

    #[test]
    fn first_byte_lower_bound_replaces_the_prior_when_precise() {
        let stats = RenderStats::new();
        let mut rng = Rng::seed(5);
        for _ in 0..50_000 {
            let t = 1.0 + 0.2 * rng.normal().abs();
            stats.note_first_byte(EngineKind::Chrome, Duration::from_secs_f64(t));
        }
        let m = stats.min_attempt(EngineKind::Chrome).as_secs_f64();
        assert!(m > 0.9 && m < 1.05, "{m}");
        assert_eq!(stats.min_attempt(EngineKind::Moli), MIN_ATTEMPT);
    }
}
