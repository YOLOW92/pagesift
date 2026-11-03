//! 每（主机，阶段）的完成时间分布与等待上限。
//!
//! 每个阶段（DNS、建连、TLS、首字节、正文空闲、总时长）按主机维护一个 Kaplan–Meier
//! 完成时间分布（[`Survival`]），超时作为右截尾样本进入估计。等待上限 =
//! min(用户/策略上限, 放弃时刻)。放弃时刻只在有负载（等待价格 > 0）时存在；
//! 价格为 0 或样本不足时上限就是配置值，行为与 0.81 完全相同（冷启动等于现值）。
//!
//! 并发（192 vCPU 实测经验）：表按主机哈希分片，分片数随核数；读上限只读原子量，
//! 不拿锁；记一个样本只锁该主机该阶段自己的小锁。共享原子量 128 字节对齐。

mod rule;
mod shared;

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pagesift_stat::Delta;
use pagesift_stat::sketch::matched_alpha;
use pagesift_stat::survival::Survival;

pub(crate) use rule::next_sweep;
pub(crate) use shared::NetStats;

/// 等待阶段。前六个对应 [`crate::TimeoutPhase`]；`PoolGap` 是连接池里空闲连接等到下一次复用的间隔。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Dns,
    Connect,
    Tls,
    FirstByte,
    BodyIdle,
    Total,
    PoolGap,
}

/// [推导] 阶段数，等于 [`Phase`] 的变体数
const PHASES: usize = 7;

/// 独占一条缓存行的包装（192 vCPU 实测：共享原子量与别的数据同行会在多 CCD 间来回颠簸）。
/// [协议] 128 字节：x86 相邻缓存行预取成对拉取（Intel 优化手册 §3.7.3），按两行对齐。
#[repr(align(128))]
#[derive(Debug, Default)]
pub(crate) struct Padded<T>(pub T);

/// 等待的影子价格 `λ·占用/价值`：占用一个连接名额一秒的代价，以"一页的价值"为单位（每秒）。
///
/// 由统一准入按负载发布：空闲时为 0（等待免费，只受上限约束），
/// 饱和时增大，继续等待规则据此给出放弃时刻。默认 0：行为与固定超时相同。
#[derive(Debug, Default)]
pub struct WaitPrice(Padded<AtomicU64>);

impl WaitPrice {
    pub fn new() -> Self {
        Self::default()
    }

    /// 设价格；负数和 NaN 按 0 处理。
    pub fn set(&self, price: f64) {
        let p = if price.is_finite() && price > 0.0 {
            price
        } else {
            0.0
        };
        self.0.0.store(p.to_bits(), Ordering::Relaxed);
    }

    pub fn get(&self) -> f64 {
        f64::from_bits(self.0.0.load(Ordering::Relaxed))
    }
}

/// 一张表共用的参数。
struct Params {
    price: Arc<WaitPrice>,
    delta: Delta,
    alpha: f64,
    /// 草图的相对分辨率 `γ = (1+α)/(1−α)`：价格变化小于这个倍数时放弃时刻也在同一个桶里，不必重算。
    gamma: f64,
}

/// 每（主机，阶段）的等待统计。
pub(crate) struct HostWait {
    slots: [Slot; PHASES],
    /// 样本总数：淘汰时衡量信息量。只有这个主机自己的请求会写。
    samples: AtomicU64,
    params: Arc<Params>,
}

struct Slot {
    /// 当前放弃时刻（纳秒）；`u64::MAX` 表示不放弃，只按上限。读上限时只读它。
    limit_ns: AtomicU64,
    /// 算出 `limit_ns` 时的价格（f64 位）。
    fit_price: AtomicU64,
    stat: Mutex<SlotStat>,
}

struct SlotStat {
    done: Survival,
    next_fit: u64,
    /// 放弃时刻生效以来的 CUSUM 累计和（变化检测）。
    cusum: f64,
}

impl Slot {
    fn new(alpha: f64) -> Self {
        Self {
            limit_ns: AtomicU64::new(u64::MAX),
            fit_price: AtomicU64::new(0),
            stat: Mutex::new(SlotStat {
                done: Survival::with_alpha(alpha),
                next_fit: 0,
                cusum: 0.0,
            }),
        }
    }
}

fn nanos(t: f64) -> u64 {
    // 秒 → 纳秒；超出 u64 的按"不放弃"
    let ns = t * 1e9;
    if ns.is_finite() && ns >= 0.0 && ns < u64::MAX as f64 {
        ns as u64
    } else {
        u64::MAX
    }
}

impl HostWait {
    fn new(params: Arc<Params>) -> Self {
        let alpha = params.alpha;
        Self {
            slots: std::array::from_fn(|_| Slot::new(alpha)),
            samples: AtomicU64::new(0),
            params,
        }
    }

    /// 这个阶段本次最多等多久：`min(cap, 放弃时刻)`。`cap` 是用户或策略给的硬上限。
    /// 没有负载时直接返回 `cap`，不读统计。
    pub(crate) fn limit(&self, phase: Phase, cap: Duration) -> Duration {
        if phase == Phase::PoolGap {
            return self.pool_limit(cap);
        }
        let price = self.params.price.get();
        if price <= 0.0 {
            return cap;
        }
        let slot = &self.slots[phase as usize];
        let fitted = f64::from_bits(slot.fit_price.load(Ordering::Relaxed));
        let g = self.params.gamma;
        if !(fitted > 0.0 && price <= fitted * g && price * g >= fitted)
            && let Ok(mut st) = slot.stat.try_lock()
        {
            // 价格变了一个桶以上：重算。锁被占着就用旧值，下次再算（不在读路径上等锁）
            self.refit(slot, &mut st, price);
        }
        match slot.limit_ns.load(Ordering::Relaxed) {
            u64::MAX => cap,
            ns => cap.min(Duration::from_nanos(ns)),
        }
    }

    /// 连接池空闲连接的保留上限：复用间隔分布上有把握覆盖 `1−δ` 的时刻
    /// （即复用间隔分布的 q(1−δ)）；不超过配置值。
    fn pool_limit(&self, cap: Duration) -> Duration {
        match self.slots[Phase::PoolGap as usize]
            .limit_ns
            .load(Ordering::Relaxed)
        {
            u64::MAX => cap,
            ns => cap.min(Duration::from_nanos(ns)),
        }
    }

    /// 阶段在 `elapsed` 时完成（成功或明确失败都算完成：不用再等了）。
    pub(crate) fn done(&self, phase: Phase, elapsed: Duration) {
        self.observe(phase, elapsed, true, false);
    }

    /// 阶段等到 `elapsed` 被放弃（超时、或空闲连接被清掉）。`cap` 是这次的硬上限：
    /// 早于它放弃的，是自适应放弃时刻在起作用，计入变化检测。
    pub(crate) fn gave_up(&self, phase: Phase, elapsed: Duration, cap: Duration) {
        self.observe(phase, elapsed, false, elapsed < cap);
    }

    fn observe(&self, phase: Phase, elapsed: Duration, finished: bool, adaptive: bool) {
        self.samples.fetch_add(1, Ordering::Relaxed);
        let slot = &self.slots[phase as usize];
        let Ok(mut st) = slot.stat.lock() else {
            return;
        };
        let t = elapsed.as_secs_f64();
        if finished {
            st.done.event(t);
        } else {
            st.done.censor(t);
        }
        let delta = self.params.delta;
        if slot.limit_ns.load(Ordering::Relaxed) != u64::MAX && phase != Phase::PoolGap {
            let (sum, alarm) = rule::cusum_step(st.cusum, adaptive, delta);
            st.cusum = sum;
            if alarm {
                // 分布变了（主机变慢）：旧段作废，回到只按上限，从新段重新估计
                st.done = Survival::with_alpha(self.params.alpha);
                st.next_fit = 0;
                slot.limit_ns.store(u64::MAX, Ordering::Relaxed);
                return;
            }
        }
        let n = st.done.count();
        if n < st.next_fit {
            return;
        }
        st.next_fit = rule::next_refit(n, delta);
        if phase == Phase::PoolGap {
            let q = st.done.confident_time_to(1.0 - delta.get(), 0.0, delta);
            slot.limit_ns
                .store(q.map_or(u64::MAX, nanos), Ordering::Relaxed);
        } else {
            let price = self.params.price.get();
            if price > 0.0 {
                self.refit(slot, &mut st, price);
            }
        }
    }

    /// 按当前价格重算放弃时刻。价格明显下降时先回到"不放弃"再算（放弃时刻只在新证据下变小，
    /// 价格降了才允许变大，避免在两个值之间来回跳）。
    fn refit(&self, slot: &Slot, st: &mut SlotStat, price: f64) {
        let current = slot.limit_ns.load(Ordering::Relaxed);
        let fitted = f64::from_bits(slot.fit_price.load(Ordering::Relaxed));
        let base = if price * self.params.gamma < fitted {
            u64::MAX
        } else {
            current
        };
        let found = rule::abandon_time(&st.done.risk_table(), price, self.params.delta);
        let next = found.map_or(base, |t| base.min(nanos(t)));
        if next != current {
            st.cusum = 0.0;
        }
        slot.limit_ns.store(next, Ordering::Relaxed);
        slot.fit_price.store(price.to_bits(), Ordering::Relaxed);
    }
}

struct Shard {
    hosts: HashMap<String, Arc<HostWait>>,
}

/// 按主机分片的等待统计表，大小受内存预算约束。
pub(crate) struct WaitTable {
    shards: Box<[Padded<Mutex<Shard>>]>,
    /// 每个分片最多保留的主机数。
    per_shard: usize,
    hasher: std::collections::hash_map::RandomState,
    params: Arc<Params>,
    pub(crate) net: NetStats,
}

/// 统计表可用的内存预算：可用内存（`/proc/meminfo` 的 MemAvailable，进程启动时读一次）的 `δ`。
/// 依据：`δ` 是全局唯一的风险份额，这里拿它当"愿意分给学习状态的内存份额"，
/// 不另设比例；按条目最坏大小折算，真实占用远低于预算（条目只为见过的耗时区间分配桶）。
/// 8 GiB 可用时约 400 MiB、约 3.5 万个主机；1 GiB 的小容器约 50 MiB、约 4400 个主机。
fn table_budget(delta: Delta) -> usize {
    let available = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|m| {
            m.lines()
                .find_map(|l| l.strip_prefix("MemAvailable:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        })
        .map(|kib| kib.saturating_mul(1024));
    match available {
        Some(bytes) => (bytes as f64 * delta.get()) as usize,
        // [策略] 读不到 MemAvailable（非 Linux）时的回退：16 MiB，约 1400 个主机的最坏占用
        None => 16 * 1024 * 1024,
    }
}

/// [推导] 单个主机条目的内存上界：每阶段两个草图（事件、截尾），每个草图的桶数是
/// `ln(t_max/t_min)/ln γ`，时间跨度取 1 ms 到 600 s（`ln 6e5 ≈ 13.3`）；每桶 8 字节，
/// 外加每个阶段约 256 字节的结构与锁。
fn entry_bytes(alpha: f64) -> usize {
    let gamma = (1.0 + alpha) / (1.0 - alpha);
    let buckets = (6e5f64.ln() / gamma.ln()).ceil() as usize; // [推导] 1 ms..600 s 的跨度，见上
    PHASES * (2 * buckets * 8 + 256) // [推导] 每桶 8 字节 + 每阶段约 256 字节结构，见上
}

impl WaitTable {
    pub(crate) fn new(price: Arc<WaitPrice>, delta: Delta, he_prior: Duration) -> Self {
        // [推导] 草图误差取与分位数本身统计误差同量级。p = 1−δ，n 取一个主机
        // 统计"信息充分"的量级 10³，指数型分布 f(q)·q = δ·ln(1/δ)；δ = 0.05 时 α ≈ 0.076
        let p = 1.0 - delta.get();
        let alpha =
            matched_alpha(p, 1e3, delta.get() * (1.0 / delta.get()).ln(), delta).clamp(1e-3, 0.5); // [推导] 草图要求 α ∈ (0,1)；超过 ½ 时桶宽已大于一倍
        let gamma = (1.0 + alpha) / (1.0 - alpha);
        let capacity = (table_budget(delta) / entry_bytes(alpha)).max(1);
        let n = shard_count(capacity, delta);
        // 分片只为减少锁争用；淘汰按全局容量均分到各片
        let per_shard = capacity / n;
        Self {
            shards: (0..n)
                .map(|_| {
                    Padded(Mutex::new(Shard {
                        hosts: HashMap::new(),
                    }))
                })
                .collect(),
            per_shard,
            hasher: Default::default(),
            params: Arc::new(Params {
                price,
                delta,
                alpha,
                gamma,
            }),
            net: NetStats::new(n, delta, he_prior),
        }
    }

    fn shard(&self, host: &str) -> usize {
        (self.hasher.hash_one(host) as usize) & (self.shards.len() - 1)
    }

    /// 取（必要时建）主机的等待统计。主机名不分大小写。
    pub(crate) fn host(&self, host: &str) -> Arc<HostWait> {
        let host = host.to_ascii_lowercase();
        let Ok(mut shard) = self.shards[self.shard(&host)].0.lock() else {
            return Arc::new(HostWait::new(self.params.clone()));
        };
        if let Some(w) = shard.hosts.get(&host) {
            return w.clone();
        }
        if shard.hosts.len() >= self.per_shard {
            evict(&mut shard.hosts, self.per_shard / 2);
        }
        let w = Arc::new(HostWait::new(self.params.clone()));
        shard.hosts.insert(host, w.clone());
        w
    }

    /// 只查不建：清理连接池时用。
    pub(crate) fn peek(&self, host: &str) -> Option<Arc<HostWait>> {
        let host = host.to_ascii_lowercase();
        let shard = self.shards[self.shard(&host)].0.lock().ok()?;
        shard.hosts.get(&host).cloned()
    }
}

/// 分片数：随核数（不小于逻辑核数的 2 的幂，同时撞同一分片的概率约 1/2），但每片至少
/// `1/δ` 个主机（分片数 ≤ 容量·δ，取 2 的幂）。每片太小时哈希稍不均匀就会提前淘汰，等于学不到；
/// `1/δ` 个主机时某片多出一个主机的相对波动约 `√δ`。
fn shard_count(capacity: usize, delta: Delta) -> usize {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let by_cores = cores.next_power_of_two();
    let by_size = ((capacity as f64 * delta.get()) as usize).max(1);
    // 不超过 by_size 的最大 2 的幂
    let by_size = 1usize << (usize::BITS - 1 - by_size.leading_zeros());
    by_cores.min(by_size)
}

/// 淘汰到 `keep` 项：先淘汰样本最少（信息量最低）、且没有请求正在用的条目。
/// 被淘汰的主机下次从冷启动开始，行为与 0.81 相同。
fn evict(hosts: &mut HashMap<String, Arc<HostWait>>, keep: usize) {
    let mut idle: Vec<(u64, String)> = hosts
        .iter()
        .filter(|(_, w)| Arc::strong_count(w) == 1)
        .map(|(k, w)| (w.samples.load(Ordering::Relaxed), k.clone()))
        .collect();
    idle.sort_unstable();
    let excess = hosts.len().saturating_sub(keep);
    for (_, k) in idle.into_iter().take(excess) {
        hosts.remove(&k);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(price: f64) -> WaitTable {
        let p = Arc::new(WaitPrice::new());
        p.set(price);
        WaitTable::new(p, Delta::DEFAULT, Duration::from_millis(250))
    }

    /// 冷启动与空闲：上限就是配置值（与 0.81 相同）。
    #[test]
    fn cold_or_idle_limit_is_the_configured_cap() {
        let t = table(0.0);
        let w = t.host("A.example");
        let cap = Duration::from_secs(30);
        assert_eq!(w.limit(Phase::FirstByte, cap), cap);
        for i in 0..1000 {
            w.done(Phase::FirstByte, Duration::from_millis(10 + i % 50));
        }
        assert_eq!(w.limit(Phase::FirstByte, cap), cap, "价格为 0 时不放弃");
        let idle = Duration::from_secs(90);
        assert_eq!(w.limit(Phase::PoolGap, idle), idle);
        assert!(Arc::ptr_eq(&w, &t.host("a.example")));
    }

    /// 一成请求挂到上限 + 有负载：上限收紧到正常请求完成之后；主机整体变慢（按收紧的上限大量放弃）
    /// 时 CUSUM 报警，回到配置值。
    #[test]
    fn loaded_hangs_tighten_then_reset_on_change() {
        let t = table(0.1);
        let w = t.host("hang.example");
        let cap = Duration::from_secs(30);
        let mut rng = pagesift_stat::rng::Rng::seed(5);
        for i in 0..5_000 {
            if i % 10 == 0 {
                w.gave_up(Phase::FirstByte, cap, cap);
            } else {
                let x = 0.005 + 0.05 * rng.uniform();
                w.done(Phase::FirstByte, Duration::from_secs_f64(x));
            }
        }
        let lim = w.limit(Phase::FirstByte, cap);
        assert!(
            lim < Duration::from_secs(1) && lim >= Duration::from_millis(50),
            "{lim:?}"
        );
        for _ in 0..200 {
            let lim = w.limit(Phase::FirstByte, cap);
            w.gave_up(Phase::FirstByte, lim, cap);
            if w.limit(Phase::FirstByte, cap) == cap {
                break;
            }
        }
        assert_eq!(w.limit(Phase::FirstByte, cap), cap);
    }

    /// 回归：大多 5 ms、偶尔 200 ms 的源站，负载高时上限也不收紧。
    #[test]
    fn loaded_bimodal_origin_keeps_the_cap() {
        let t = table(100.0);
        let w = t.host("local.example");
        let cap = Duration::from_secs(30);
        for i in 0..20_000 {
            let ms = if i % 50 == 0 { 200 } else { 5 };
            w.done(Phase::FirstByte, Duration::from_millis(ms));
        }
        assert_eq!(w.limit(Phase::FirstByte, cap), cap);
    }

    /// 连接复用间隔：样本够了以后保留上限收到 q(1−δ) 附近，样本少时保持配置值。
    #[test]
    fn pool_idle_follows_reuse_gaps() {
        let t = table(0.0);
        let w = t.host("pool.example");
        let cap = Duration::from_secs(90);
        for _ in 0..10 {
            w.done(Phase::PoolGap, Duration::from_millis(200));
        }
        assert_eq!(w.limit(Phase::PoolGap, cap), cap);
        for i in 0..2000 {
            w.done(Phase::PoolGap, Duration::from_millis(100 + (i % 10) * 100));
        }
        let lim = w.limit(Phase::PoolGap, cap);
        assert!(
            lim >= Duration::from_millis(900) && lim < Duration::from_secs(2),
            "{lim:?}"
        );
    }

    #[test]
    fn shards_hold_at_least_one_over_delta_hosts() {
        let d = Delta::DEFAULT;
        for capacity in [1, 19, 20, 100, 1_000, 100_000] {
            let n = shard_count(capacity, d);
            assert!(n.is_power_of_two());
            assert!(n == 1 || capacity / n >= 20, "{capacity} → {n}");
        }
        assert!(table_budget(d) > 0);
    }

    #[test]
    fn table_is_bounded_and_evicts_low_information_hosts() {
        let t = table(0.0);
        let keep = t.host("busy.example");
        for _ in 0..100 {
            keep.done(Phase::Dns, Duration::from_millis(5));
        }
        drop(keep);
        let total = t.per_shard * t.shards.len();
        for i in 0..total * 3 {
            let _ = t.host(&format!("h{i}.example"));
        }
        let len: usize = t
            .shards
            .iter()
            .map(|s| s.0.lock().unwrap().hosts.len())
            .sum();
        assert!(len <= total, "{len} > {total}");
        assert!(t.peek("busy.example").is_some(), "样本多的主机留下");
    }
}
