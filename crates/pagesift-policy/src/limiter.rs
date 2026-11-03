//! 按主机限速和并发上限。
//!
//! - 全局在途数：USL 峰值 `N*` 的 `1−δ` 置信下界（[`crate::concurrency`]），冷启动等于
//!   `global_concurrency`，用户写明的 `global_ceiling` 是硬上限。
//! - 每站点并发：按并发档位的延迟梯度控制（[`GradientLimit`]，Netflix Gradient2 / TCP Vegas，
//!），冷启动等于 `per_host_concurrency`（0.81 的 2），只在相邻档的延迟差显著时调整，
//!   延迟到没有排队时的 1.5 倍（0.81 的阈值）直接退一档。上限是礼貌策略（`per_host_concurrency` 本身）
//!   和 Crawl-delay：间隔 `I` 下同时在途超过 `⌈RTT 的 1−δ 预测上界 / I⌉` 不会让请求更早开始（Little 定律）。
//!   429 只加大请求间隔（同 0.81）。礼貌开关关闭时不设每站点上限。
//! - 名额都是先进先出的（[`Slots`]），调整上限不打断在途请求。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pagesift_stat::Delta;
use pagesift_stat::limit::GradientLimit;
use pagesift_stat::usl::UslFit;
use tokio::time::Instant;

use crate::concurrency::GlobalCtl;
use crate::slots::{Slot, Slots};

#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// 同一主机两次请求开始之间的最小间隔；robots.txt 有 Crawl-delay 时以它为准。
    pub default_interval: Duration,
    /// 全局在途数的冷启动值；之后由 USL 控制器调整（见模块说明）。
    pub global_concurrency: usize,
    /// 用户写明的全局在途数上限（硬上限）。`None` 时只受内存预算和文件描述符余量约束。
    pub global_ceiling: Option<usize>,
    /// 每站点并发的冷启动值，也是礼貌策略的上限：延迟梯度控制只在它以下调整。
    pub per_host_concurrency: usize,
    /// 管理员开关：关掉后不按站点限速（没有请求间隔、每站点并发上限和 Crawl-delay），只受全局并发约束。
    /// 限流（429）学到的额外间隔仍按退避开关处理。默认开。
    pub politeness: bool,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            // [策略] 用户礼貌策略的下限：同一主机每秒至多开始一个请求，即常见爬虫
            // 礼貌约定的"每站点每秒一次"，0.81 的现值；robots 的 Crawl-delay 优先，下限之上由延迟梯度控制
            default_interval: Duration::from_secs(1),
            // [策略] 先验包 v0：0.81 的现值；冷启动的全局在途数，之后由 USL 控制器调整
            global_concurrency: 64,
            global_ceiling: None,
            // [策略] 先验包 v0：0.81 的现值；每站点并发的冷启动值和礼貌上限
            per_host_concurrency: 2,
            politeness: true,
        }
    }
}

/// 限流学到的额外间隔：Retry-After 优先（协议，由 Policy 处理，有它时不进这里）；
/// 没有时按"在哪个间隔被拒、在哪个间隔通过"估计站点限额对应的最小间隔 `S`。
///
/// 每次 429 只知道 `S` 大于当时的间隔，每次成功只知道 `S` 不大于当时的间隔（current-status 数据）：
/// 区间 `(lo, hi]` 是 `S` 的全部信息。没有上界时按指数搜索加倍（同 0.81），有了上界就在区间里按
/// 几何中点二分，收敛到站点的真实限额，而不是在"翻倍 ↔ 减半"之间来回撞 429。区间窄到计时器分辨率
/// 以下时忘掉下界，重新向下试探（站点可能放宽了限额），代价是偶尔一次 429。
/// 不做"429 间隔的分布估计"：这里没有分布可估——观测的是阈值的上下界，二分是这类
/// 数据下的阈值估计（Bisection / exponential search）。
///
/// [策略] 先验包 v0：0.81 的现值，第一次 429 时的间隔
const PENALTY_START: Duration = Duration::from_millis(250);
/// [策略] 用户礼貌策略的安全上限（0.81 的现值）：再慢的限额也不让一个站点的间隔超过 30 s，
/// 更长的冷却由主机退避（Backoff）处理
const PENALTY_MAX: Duration = Duration::from_secs(30);
/// [协议] tokio 计时器的分辨率是 1 ms：间隔差别小于它时无法区分，也无法执行
const TIMER_RESOLUTION: Duration = Duration::from_millis(1);

#[derive(Debug, Default)]
struct Penalty {
    value: Duration,
    /// 上次加倍的时间：同一批在途请求一起收到 429 只算一次，不连着翻好几倍
    raised_at: Option<Instant>,
    /// 已知会被拒的最大间隔（`S > lo`）。
    lo: Duration,
    /// 限流之后已知能通过的最小间隔（`S ≤ hi`）。
    hi: Option<Duration>,
}

impl Penalty {
    /// 在当前间隔下被拒（429，没有 Retry-After）。
    fn throttled(&mut self, now: Instant) {
        if self.raised_at.is_some_and(|t| now < t + self.value) {
            return;
        }
        self.lo = self.lo.max(self.value);
        if self.hi.is_some_and(|h| h <= self.lo) {
            self.hi = None;
        }
        self.value = match self.hi {
            Some(hi) => geometric_mid(self.lo, hi),
            None => (self.lo * 2).max(PENALTY_START),
        }
        .min(PENALTY_MAX);
        self.raised_at = Some(now);
    }

    /// 在当前间隔下通过。
    fn succeeded(&mut self) {
        if self.value.is_zero() {
            return;
        }
        let hi = self.hi.map_or(self.value, |h| h.min(self.value));
        self.hi = Some(hi);
        if !self.lo.is_zero() && hi.saturating_sub(self.lo) < TIMER_RESOLUTION {
            // 区间已收敛到计时器分辨率以下：忘掉下界，重新向下试探
            self.lo = Duration::ZERO;
        }
        self.value = if self.lo.is_zero() {
            hi / 2
        } else {
            geometric_mid(self.lo, hi)
        };
        if self.value < TIMER_RESOLUTION {
            *self = Self::default();
        }
    }
}

/// 两个间隔的几何中点 `√(a·b)`（间隔按比例搜索）。
fn geometric_mid(a: Duration, b: Duration) -> Duration {
    Duration::from_secs_f64((a.as_secs_f64() * b.as_secs_f64()).sqrt())
}

/// 一个主机的控制状态：限流学到的额外间隔、最近一次的请求间隔、完成数。
#[derive(Debug)]
struct HostCtl {
    /// 限流学到的额外间隔（见 [`Penalty`]），让请求速度停在站点限额之下，
    /// 而不是冷却结束就全速冲上去、再被限流。
    penalty: Penalty,
    gradient: GradientLimit,
    interval: Duration,
    /// 完成的请求数：主机表满了按它衡量信息量，先淘汰少的。
    samples: u64,
}

impl HostCtl {
    /// 一个请求结束：占用了 `held`。返回新的整数名额（变了时）。
    fn complete(&mut self, held: Duration, saturated: bool, polite_cap: usize) -> Option<usize> {
        self.samples += 1;
        let mut cap = polite_cap as f64;
        if !self.interval.is_zero()
            && let Some(rtt_hi) = self.gradient.slow_after()
        {
            // Crawl-delay 上限：⌈RTT 的 1−δ 预测上界 / 间隔⌉，见模块说明
            cap = cap.min((rtt_hi / self.interval.as_secs_f64()).ceil().max(1.0));
        }
        self.gradient.complete(held.as_secs_f64(), saturated, cap)
    }
}

struct HostState {
    slots: Slots,
    ctl: Arc<Mutex<HostCtl>>,
    /// 排队等待发出请求的门：同一时刻只有一个请求在等间隔，保证间隔按实际开始时间算。
    gate: Arc<tokio::sync::Mutex<Option<Instant>>>,
}

/// 持有期间占用一个全局并发名额和一个主机并发名额；归还时把占用时长交给两级控制器。
#[derive(Debug)]
pub struct Permit {
    global: Slot,
    host: Option<HostHold>,
    held_from: Instant,
    ctl: Arc<GlobalCtl>,
    waited: Duration,
    interval: Duration,
    robots_crawl_delay: bool,
}

#[derive(Debug)]
struct HostHold {
    slot: Slot,
    slots: Slots,
    ctl: Arc<Mutex<HostCtl>>,
    cap: usize,
}

impl Permit {
    /// Time spent waiting for host/global admission and request spacing.
    pub fn waited(&self) -> Duration {
        self.waited
    }

    /// Effective minimum interval between request starts for this host.
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Whether robots.txt supplied a Crawl-delay for this admission.
    pub fn robots_crawl_delay(&self) -> bool {
        self.robots_crawl_delay
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let held = self.held_from.elapsed();
        if let Some(h) = &self.host {
            let next = h
                .ctl
                .lock()
                .ok()
                .and_then(|mut c| c.complete(held, h.slot.saturated(), h.cap));
            if let Some(n) = next {
                h.slots.set_limit(n);
            }
        }
        self.ctl.complete(held, self.global.saturated());
    }
}

pub struct RateLimiter {
    config: RateLimitConfig,
    global: Arc<GlobalCtl>,
    hosts: Mutex<HashMap<String, HostState>>,
    /// 主机表的容量（见 [`table_capacity`]）。
    capacity: usize,
}

/// 主机表的容量（条目数）：内存预算的 `δ` 份 / 单个条目的内存上界。
///
/// 依据同 `pagesift-net` 的等待统计表：`δ` 是全局唯一的风险份额，这里当作"愿意分给
/// 学习状态的内存份额"，不另设比例（表的大小按内存预算决定）。取代 0.81 的固定 4096。
fn table_capacity(delta: Delta) -> usize {
    match pagesift_net::resources::budget() {
        Some(bytes) => ((bytes as f64 * delta.get()) as usize / entry_bytes()).max(1),
        // [策略] 读不到内存（非 Linux）时的回退：先验包 v0，0.81 的现值
        None => 4096,
    }
}

/// [推导] 单个主机条目的内存上界：各结构体的大小，加上三个 `Arc` 的引用计数头、
/// 主机名（RFC 1035 §2.3.4：域名最长 253 字节）和哈希表每项的开销（键、值各一个指针宽的槽）。
fn entry_bytes() -> usize {
    use std::mem::size_of;
    size_of::<HostState>()
        + size_of::<HostCtl>()
        + size_of::<tokio::sync::Semaphore>()
        + size_of::<tokio::sync::Mutex<Option<Instant>>>()
        + 3 * 2 * size_of::<usize>() // [推导] 三个 Arc，各有强、弱两个计数
        + 253 // [协议] RFC 1035 §2.3.4 域名长度上限
        + 2 * size_of::<usize>()
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            global: Arc::new(GlobalCtl::new(
                config.global_concurrency.max(1),
                config.global_ceiling,
            )),
            config,
            hosts: Mutex::new(HashMap::new()),
            capacity: table_capacity(Delta::DEFAULT),
        }
    }

    pub fn config(&self) -> &RateLimitConfig {
        &self.config
    }

    /// 当前的全局在途数目标。
    pub fn global_limit(&self) -> usize {
        self.global.slots.limit()
    }

    /// 当前全局在途的请求数。
    pub fn global_active(&self) -> usize {
        self.global.slots.active()
    }

    /// 把网络层的等待价格交给全局控制器，之后每个观测窗按负载更新。
    pub fn publish_wait_price(&self, price: Arc<pagesift_net::WaitPrice>) {
        self.global.publish_wait_price(price);
    }

    /// 最近一个观测窗里全局名额的平均占用时长；还没有观测窗时为 `None`。
    pub fn global_service_time(&self) -> Option<Duration> {
        self.global.service_time()
    }

    /// 全局 USL 拟合（诊断用）；观测点不够时为 `None`。
    pub fn global_fit(&self) -> Option<UslFit> {
        self.global.fit()
    }

    /// `host` 当前的并发名额（礼貌开关关闭时没有每站点上限，返回 `None`）。
    pub fn host_limit(&self, host: &str) -> Option<usize> {
        if !self.config.politeness {
            return None;
        }
        let hosts = self.hosts.lock().expect("lock poisoned");
        Some(
            hosts
                .get(&host.to_ascii_lowercase())
                .map_or(self.config.per_host_concurrency.max(1), |s| s.slots.limit()),
        )
    }

    /// 等到可以向 `host` 发请求。`crawl_delay` 来自 robots.txt，优先于默认间隔。
    pub async fn acquire(&self, host: &str, crawl_delay: Option<Duration>) -> Permit {
        let started = Instant::now();
        let (slots, gate, ctl) = self.host(host);
        let polite = self.config.politeness;
        let interval = {
            let mut c = ctl.lock().expect("lock poisoned");
            let learned = c.penalty.value;
            let interval = if polite {
                crawl_delay
                    .unwrap_or(self.config.default_interval)
                    .max(learned)
            } else {
                learned
            };
            c.interval = interval;
            interval
        };
        let host_slot = if polite {
            Some(slots.acquire().await)
        } else {
            None
        };
        // 不限速且没有限流学到的间隔时连这道门都不进：同一主机的请求不必排队
        let mut last = if polite || !interval.is_zero() {
            Some(gate.lock().await)
        } else {
            None
        };
        // 截止时间已过就直接放行：进定时器至少要等到下一个 1 ms 刻度，
        // 同一主机的请求排在这道门后面，会被串成每毫秒一个
        if let Some(prev) = last.as_deref().copied().flatten()
            && prev + interval > Instant::now()
        {
            tokio::time::sleep_until(prev + interval).await;
        }
        let global = self.global.slots.acquire().await;
        if let Some(last) = last.as_mut() {
            **last = Some(Instant::now());
        }
        Permit {
            global,
            host: host_slot.map(|slot| HostHold {
                slot,
                slots,
                ctl,
                cap: self.config.per_host_concurrency.max(1),
            }),
            held_from: Instant::now(),
            ctl: self.global.clone(),
            waited: started.elapsed(),
            interval,
            robots_crawl_delay: polite && crawl_delay.is_some(),
        }
    }

    /// 主机返回了 429（或带 Retry-After 的 503）：加大它的请求间隔。
    pub fn throttled(&self, host: &str) {
        let (_, _, ctl) = self.host(host);
        ctl.lock()
            .expect("lock poisoned")
            .penalty
            .throttled(Instant::now());
    }

    /// 主机正常响应：限流学到的额外间隔向站点真实限额收缩（见 [`Penalty`]）。
    pub fn succeeded(&self, host: &str) {
        let hosts = self.hosts.lock().expect("lock poisoned");
        if let Some(s) = hosts.get(&host.to_ascii_lowercase()) {
            s.ctl.lock().expect("lock poisoned").penalty.succeeded();
        }
    }

    #[cfg(test)]
    fn penalty(&self, host: &str) -> Duration {
        self.host(host).2.lock().unwrap().penalty.value
    }

    #[allow(clippy::type_complexity)]
    fn host(
        &self,
        host: &str,
    ) -> (
        Slots,
        Arc<tokio::sync::Mutex<Option<Instant>>>,
        Arc<Mutex<HostCtl>>,
    ) {
        let key = host.to_ascii_lowercase();
        let mut hosts = self.hosts.lock().expect("lock poisoned");
        if hosts.len() >= self.capacity && !hosts.contains_key(&key) {
            prune(&mut hosts, self.capacity);
        }
        let per_host = self.config.per_host_concurrency.max(1);
        let s = hosts.entry(key).or_insert_with(|| HostState {
            slots: Slots::new(per_host),
            gate: Arc::new(tokio::sync::Mutex::new(None)),
            ctl: Arc::new(Mutex::new(HostCtl {
                penalty: Penalty::default(),
                gradient: GradientLimit::new(per_host as f64, Delta::DEFAULT),
                interval: Duration::ZERO,
                samples: 0,
            })),
        });
        (s.slots.clone(), s.gate.clone(), s.ctl.clone())
    }
}

/// 主机表满了：淘汰到容量的 `1−δ`（留 `δ` 的余量，平均每 `δ·容量` 次新主机才清理一次）。
/// 不能淘汰的：有请求在用的、限流学到的间隔还没清零的、请求间隔还在生效的（上次开始 + 间隔 > 现在，
/// 忘了它就会提前发下一个请求，违反礼貌）。其余按信息量（完成数）从少到多淘汰；被淘汰的
/// 主机下次从冷启动开始，和 0.81 的新主机一样。取代 0.81 的"60 s 内用过就留"。
fn prune(hosts: &mut HashMap<String, HostState>, capacity: usize) {
    let now = Instant::now();
    let keep = (capacity as f64 * (1.0 - Delta::DEFAULT.get())) as usize;
    let mut idle: Vec<(u64, String)> = hosts
        .iter()
        .filter_map(|(k, s)| {
            let ctl = s.ctl.try_lock().ok()?;
            let busy = s.slots.active() > 0
                || Arc::strong_count(&s.ctl) > 1
                || Arc::strong_count(&s.gate) > 1
                || !ctl.penalty.value.is_zero();
            let spacing = s
                .gate
                .try_lock()
                .map_or(true, |g| g.is_some_and(|t| now < t + ctl.interval));
            (!busy && !spacing).then(|| (ctl.samples, k.clone()))
        })
        .collect();
    if hosts.len() <= keep {
        return;
    }
    idle.sort_unstable();
    let excess = hosts.len() - keep;
    for (_, k) in idle.into_iter().take(excess) {
        hosts.remove(&k);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(per_host: usize, global: usize) -> Arc<RateLimiter> {
        Arc::new(RateLimiter::new(RateLimitConfig {
            default_interval: Duration::from_secs(1),
            global_concurrency: global,
            global_ceiling: Some(global),
            per_host_concurrency: per_host,
            politeness: true,
        }))
    }

    #[tokio::test(start_paused = true)]
    async fn spaces_requests_per_host() {
        let l = limiter(4, 64);
        let t0 = Instant::now();
        let mut starts = Vec::new();
        for _ in 0..3 {
            let _p = l.acquire("a.com", None).await;
            starts.push(t0.elapsed());
        }
        assert_eq!(starts, [0, 1, 2].map(Duration::from_secs));
        // 其他主机不受影响。
        let _p = l.acquire("b.com", None).await;
        assert_eq!(t0.elapsed(), Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn crawl_delay_overrides_default() {
        let l = limiter(4, 64);
        let t0 = Instant::now();
        let first = l.acquire("a.com", Some(Duration::from_secs(5))).await;
        assert!(first.robots_crawl_delay());
        assert_eq!(first.interval(), Duration::from_secs(5));
        drop(first);
        let second = l.acquire("A.COM", Some(Duration::from_secs(5))).await;
        assert_eq!(second.waited(), Duration::from_secs(5));
        drop(second);
        assert_eq!(t0.elapsed(), Duration::from_secs(5));
        drop(l.acquire("a.com", Some(Duration::from_millis(100))).await);
        assert_eq!(t0.elapsed(), Duration::from_millis(5100));
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_requests_are_spaced() {
        let l = limiter(8, 64);
        let t0 = Instant::now();
        let tasks: Vec<_> = (0..4)
            .map(|_| {
                let l = l.clone();
                tokio::spawn(async move {
                    let _p = l.acquire("a.com", None).await;
                    t0.elapsed()
                })
            })
            .collect();
        let mut starts = Vec::new();
        for t in tasks {
            starts.push(t.await.unwrap());
        }
        starts.sort();
        assert_eq!(starts, [0, 1, 2, 3].map(Duration::from_secs));
    }

    #[tokio::test(start_paused = true)]
    async fn throttling_widens_the_interval_until_requests_succeed() {
        let l = limiter(4, 64);
        let fast = Duration::ZERO;
        l.throttled("a.com");
        assert_eq!(l.penalty("a.com"), PENALTY_START);
        l.throttled("a.com");
        assert_eq!(
            l.penalty("a.com"),
            PENALTY_START,
            "one burst of 429s doubles once"
        );
        tokio::time::advance(PENALTY_START).await;
        l.throttled("a.com");
        assert_eq!(l.penalty("a.com"), PENALTY_START * 2);
        let t0 = Instant::now();
        drop(l.acquire("a.com", Some(fast)).await);
        drop(l.acquire("a.com", Some(fast)).await);
        assert_eq!(
            t0.elapsed(),
            PENALTY_START * 2,
            "the learned interval applies"
        );
        // 其他主机不受影响
        let t1 = Instant::now();
        drop(l.acquire("b.com", Some(fast)).await);
        drop(l.acquire("b.com", Some(fast)).await);
        assert_eq!(t1.elapsed(), Duration::ZERO);
        for _ in 0..200 {
            l.succeeded("a.com");
        }
        assert_eq!(
            l.penalty("a.com"),
            Duration::ZERO,
            "recovers after enough successes"
        );
        for _ in 0..20 {
            l.throttled("a.com");
            tokio::time::advance(PENALTY_MAX).await;
        }
        assert_eq!(l.penalty("a.com"), PENALTY_MAX);
    }

    #[tokio::test(start_paused = true)]
    async fn per_host_and_global_concurrency() {
        let l = limiter(1, 2);
        let p1 = l.acquire("a.com", Some(Duration::ZERO)).await;
        // 主机名额用完：第二个请求要等第一个释放。
        let l2 = l.clone();
        let waiting = tokio::spawn(async move { l2.acquire("a.com", Some(Duration::ZERO)).await });
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert!(!waiting.is_finished());
        drop(p1);
        let p2 = waiting.await.unwrap();

        let _p3 = l.acquire("b.com", None).await;
        // 全局名额（2）用完。
        let l3 = l.clone();
        let blocked = tokio::spawn(async move { l3.acquire("c.com", None).await });
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert!(!blocked.is_finished());
        drop(p2);
        blocked.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn politeness_off_drops_spacing_and_per_host_cap() {
        let l = Arc::new(RateLimiter::new(RateLimitConfig {
            default_interval: Duration::from_secs(1),
            global_concurrency: 8,
            global_ceiling: None,
            per_host_concurrency: 1,
            politeness: false,
        }));
        let t0 = Instant::now();
        // 每站点并发上限是 1：限速开着时第二个会等第一个放手；关掉后同时拿到
        let a = l.acquire("a.com", Some(Duration::from_secs(5))).await;
        let b = l.acquire("a.com", Some(Duration::from_secs(5))).await;
        assert_eq!(t0.elapsed(), Duration::ZERO);
        assert!(!a.robots_crawl_delay() && b.interval().is_zero());
        // 限流学到的间隔仍然生效（由退避开关管）
        l.throttled("a.com");
        let c = l.acquire("a.com", None).await;
        assert!(!c.interval().is_zero());
    }

    async fn hold_round(l: &RateLimiter, host: &str, each: Duration) {
        let n = l.host_limit(host).unwrap();
        let mut held = Vec::new();
        for _ in 0..n {
            held.push(l.acquire(host, Some(Duration::ZERO)).await);
        }
        tokio::time::advance(each).await;
        drop(held);
    }

    #[tokio::test(start_paused = true)]
    async fn cold_start_is_the_policy_and_the_policy_caps_growth() {
        let l = Arc::new(RateLimiter::new(RateLimitConfig::default()));
        assert_eq!(l.global_limit(), 64);
        assert_eq!(l.host_limit("a.com"), Some(2));
        for _ in 0..20 {
            hold_round(&l, "a.com", Duration::from_millis(50)).await;
        }
        assert_eq!(l.host_limit("a.com"), Some(2), "礼貌上限之上不增");
    }

    #[tokio::test(start_paused = true)]
    async fn the_host_table_keeps_hosts_whose_spacing_is_active() {
        let l = limiter(2, 64);
        drop(l.acquire("busy.com", None).await);
        {
            let mut hosts = l.hosts.lock().unwrap();
            for i in 0..10 {
                hosts.insert(
                    format!("h{i}.com"),
                    HostState {
                        slots: Slots::new(2),
                        gate: Arc::new(tokio::sync::Mutex::new(None)),
                        ctl: Arc::new(Mutex::new(HostCtl {
                            penalty: Penalty::default(),
                            gradient: GradientLimit::new(2.0, Delta::DEFAULT),
                            interval: Duration::ZERO,
                            samples: 0,
                        })),
                    },
                );
            }
            prune(&mut hosts, 4);
            // 容量 4 的 1−δ = 3：空闲的 h* 淘汰到只剩 3 项，刚发过请求、间隔（1 s）还在生效的 busy.com 保留
            assert!(hosts.contains_key("busy.com"));
            assert_eq!(hosts.len(), 3);
        }
        assert!(l.capacity >= 1);
    }

    #[test]
    fn penalty_bisects_between_rejected_and_accepted_spacing() {
        let mut p = Penalty::default();
        let mut now = Instant::now();
        p.throttled(now); // 0 被拒 → 250 ms（先验）
        now += PENALTY_MAX;
        p.throttled(now); // 250 被拒 → 500
        assert_eq!(p.value, PENALTY_START * 2);
        p.succeeded(); // 500 通过 → √(250·500) ≈ 354
        let mid = p.value;
        assert!(mid > PENALTY_START && mid < PENALTY_START * 2, "{mid:?}");
        now += PENALTY_MAX;
        p.throttled(now); // 354 被拒 → 区间 (354, 500]
        assert!(
            p.value > mid && p.value < PENALTY_START * 2,
            "{:?}",
            p.value
        );
        // 一直通过：收敛后忘掉下界，逐步回到 0
        for _ in 0..200 {
            p.succeeded();
        }
        assert_eq!(p.value, Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn per_host_concurrency_backs_off_when_the_host_queues() {
        let l = limiter(8, 64);
        for _ in 0..4 {
            hold_round(&l, "a.com", Duration::from_millis(10)).await;
        }
        assert_eq!(l.host_limit("a.com"), Some(8), "平稳时停在礼貌上限");
        // 服务时间涨到 10 倍：超过 1.5 倍的保护，至少退一档；梯度律给 8·0.1 + √8 ≈ 3.6
        hold_round(&l, "a.com", Duration::from_millis(100)).await;
        assert_eq!(l.host_limit("a.com"), Some(3));
        assert_eq!(l.host_limit("b.com"), Some(8), "其他主机不受影响");
    }

    /// 阶跃负载：先是不排队的站点（每个请求 10 ms），之后容量降到 3（在途超过 3 个时服务时间按比例
    /// 变长）。每站点并发收到容量附近，不发散、不一路压到 1、不来回大幅摆动。
    #[tokio::test(start_paused = true)]
    async fn per_host_concurrency_does_not_oscillate_under_a_step_load() {
        let l = limiter(8, 64);
        for _ in 0..10 {
            hold_round(&l, "a.com", Duration::from_millis(10)).await;
        }
        let mut seen = Vec::new();
        for i in 0..60 {
            let n = l.host_limit("a.com").unwrap() as u64;
            hold_round(&l, "a.com", Duration::from_millis(10 * n.max(3) / 3)).await;
            if i >= 20 {
                seen.push(l.host_limit("a.com").unwrap());
            }
        }
        let (lo, hi) = (*seen.iter().min().unwrap(), *seen.iter().max().unwrap());
        assert!(lo >= 2 && hi <= 6 && hi - lo <= 2, "[{lo}, {hi}]");
    }

    #[tokio::test(start_paused = true)]
    async fn crawl_delay_caps_useless_concurrency() {
        // 间隔 1 s、请求 100 ms 就结束：同时在途超过 1 个不会让请求更早开始
        let l = limiter(2, 64);
        for _ in 0..4 {
            let p = l.acquire("a.com", None).await;
            tokio::time::advance(Duration::from_millis(100)).await;
            drop(p);
        }
        assert_eq!(l.host_limit("a.com"), Some(1));
    }
}
