//! 自适应退避：连续失败后冷却，429/503 遵守 Retry-After（RFC 9110 §10.2.3，协议优先）。
//!
//! 冷却时长：主机故障持续时长的 Kaplan–Meier 估计上，已持续 `e` 的故障
//! 有把握恢复概率达到 `1−δ` 的时刻（[`Outages`]）；限流（没给 Retry-After 的 429）单独一张分布。
//! 0.81 的指数退避 `base·2^k` 是礼貌下限：礼貌开关开着时冷却取两者中较长的，学到的分布只能让
//! 冷却更长、不能让对正在故障的站重试更勤；礼貌关掉时完全按分布（样本不够时仍用指数退避）。
//! 都不超过 `max_cooldown`（用户策略的上限）。
//!
//! 主机表按主机哈希分片（分片数随核数）：`check`/`on_success` 每个请求都调，不能走一把全局锁
//! （192 vCPU 实测经验）。

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use tokio::time::Instant;

use crate::outage::Outages;

#[derive(Debug, Clone)]
pub struct BackoffConfig {
    /// 连续失败多少次后进入冷却。
    pub failure_threshold: u32,
    /// 冷却时长的礼貌下限底数：礼貌开关开着时冷却不短于 `base·2^k`；
    /// 也是故障时长分布还没有把握时的先验。
    pub base_cooldown: Duration,
    /// 冷却上限（用户策略）。
    pub max_cooldown: Duration,
    /// 管理员开关：关掉后不按站点冷却，也不因限流加大请求间隔（429、Retry-After 都不等）。默认开。
    pub enabled: bool,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            // [策略] 先验包 v0：0.81 的现值。连续失败 3 次才算主机故障：健康主机的偶发失败率
            // 即使高到 1/3，连续 3 次的概率也只有 1/27 < δ
            failure_threshold: 3,
            // [策略] 礼貌下限（0.81 的现值）：对正在故障的站，第一次冷却至少 10 s，之后逐次加倍
            base_cooldown: Duration::from_secs(10),
            // [策略] 用户策略：单次冷却的上限（0.81 的现值）
            max_cooldown: Duration::from_secs(600),
            enabled: true,
        }
    }
}

#[derive(Debug, Default)]
struct Health {
    /// 连续的 5xx、网络错误、超时
    failures: u32,
    /// 本轮已失败的**不同 URL**（同一 URL 的重试不再累加）。
    /// 单个永久失败的 URL 不该让整个站点进入指数冷却。
    /// 达到 `failure_threshold` 就清空，所以长度不超过阈值，不需要另设上限。
    failed_urls: Vec<String>,
    /// 连续的限流（429、带 Retry-After 的 503）。和 `failures` 分开计：几次限流不该让一次偶发的 5xx
    /// 直接跳到很长的冷却
    throttles: u32,
    until: Option<Instant>,
    /// 这一轮故障 / 限流从什么时候开始（故障时长分布的起点）。
    down_since: Option<Instant>,
    throttled_since: Option<Instant>,
}

/// 独占缓存行的分片（[推导] 128 字节：相邻缓存行成对预取，按两行对齐）。
#[repr(align(128))]
struct Shard(Mutex<HostMap>);

struct HostMap {
    hosts: HashMap<String, Health>,
    /// 下次清理的门槛。
    prune_at: usize,
}

pub struct Backoff {
    config: BackoffConfig,
    shards: Box<[Shard]>,
    hasher: std::collections::hash_map::RandomState,
    /// 不在冷却中的条目第一次到这么多时清理（每分片）。
    prune_floor: usize,
    outages: Outages,
    throttling: Outages,
    /// 礼貌开关（限速配置的 `politeness`）：开着时指数退避是冷却的下限。
    polite: bool,
}

/// [策略] 先验包 v0：0.81 的现值（全表 4096 项时清理），按分片数均分；
/// 之后每个分片的门槛是清理后存活数的两倍（摊还 O(1)，动态表，CLRS §17.4）
const PRUNE_AT: usize = 4096;

impl Backoff {
    pub fn new(config: BackoffConfig) -> Self {
        // [推导] 分片数随核数（不小于逻辑核数的 2 的幂），但每片的清理门槛至少 1/δ 个主机：
        // 分片数 ≤ PRUNE_AT·δ（取 2 的幂），哈希稍不均匀时不会过早清掉还在计数的主机
        let delta = pagesift_stat::Delta::DEFAULT.get();
        let by_cores = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .next_power_of_two();
        let by_size = ((PRUNE_AT as f64 * delta) as usize).max(1);
        let n = by_cores.min(1usize << (usize::BITS - 1 - by_size.leading_zeros()));
        let prune_floor = (PRUNE_AT / n).max(1);
        Self {
            config,
            shards: (0..n)
                .map(|_| {
                    Shard(Mutex::new(HostMap {
                        hosts: HashMap::new(),
                        prune_at: prune_floor,
                    }))
                })
                .collect(),
            hasher: Default::default(),
            prune_floor,
            outages: Outages::default(),
            throttling: Outages::default(),
            polite: true,
        }
    }

    /// 礼貌开关（默认开）：关掉后冷却完全按故障时长分布，可以比 0.81 的指数退避短。
    pub fn with_politeness(mut self, polite: bool) -> Self {
        self.polite = polite;
        self
    }

    fn shard(&self, host: &str) -> &Mutex<HostMap> {
        &self.shards[(self.hasher.hash_one(host) as usize) & (self.shards.len() - 1)].0
    }

    /// 退避开关（[`BackoffConfig::enabled`]）。
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// 主机在冷却期内时返回剩余时间。退避关掉时总是放行。
    pub fn check(&self, host: &str) -> Result<(), Duration> {
        if !self.config.enabled {
            return Ok(());
        }
        let host = host.to_ascii_lowercase();
        let map = self.shard(&host).lock().expect("lock poisoned");
        let now = Instant::now();
        match map.hosts.get(&host).and_then(|h| h.until) {
            Some(until) if until > now => Err(until - now),
            _ => Ok(()),
        }
    }

    pub fn on_success(&self, host: &str) {
        let host = host.to_ascii_lowercase();
        let removed = self
            .shard(&host)
            .lock()
            .expect("lock poisoned")
            .hosts
            .remove(&host);
        let Some(h) = removed else {
            return;
        };
        let now = Instant::now();
        if let Some(t) = h.down_since {
            self.outages.recovered(now - t);
        }
        if let Some(t) = h.throttled_since {
            self.throttling.recovered(now - t);
        }
    }

    /// 5xx、网络错误、超时。
    pub fn on_failure(&self, host: &str) {
        let cfg = &self.config;
        let outages = &self.outages;
        let polite = self.polite;
        self.update(host, |h, now| {
            h.failures = h.failures.saturating_add(1);
            let since = *h.down_since.get_or_insert(now);
            (h.failures >= cfg.failure_threshold).then(|| {
                cooldown(
                    cfg,
                    outages,
                    now - since,
                    h.failures - cfg.failure_threshold,
                    polite,
                )
            })
        });
    }

    /// 429 或 503。有 Retry-After 时按它冷却（协议优先），否则按限流时长分布（先验是指数退避）冷却。
    pub fn on_throttled(&self, host: &str, retry_after: Option<Duration>) {
        let cfg = &self.config;
        let throttling = &self.throttling;
        let polite = self.polite;
        self.update(host, |h, now| {
            h.throttles = h.throttles.saturating_add(1);
            let since = *h.throttled_since.get_or_insert(now);
            Some(
                retry_after.unwrap_or_else(|| {
                    cooldown(cfg, throttling, now - since, h.throttles - 1, polite)
                }),
            )
        });
    }

    /// 单个 URL 的失败（超时、网络错误）。按"不同 URL"计数，同一 URL 重试不累加；
    /// 达到阈值后进入**固定**的 base_cooldown，并在触发后清零重计，不指数膨胀。
    ///
    /// 动机：真实站点里常有少量永久慢/永久坏的 URL。按原来"每次失败都累加并指数增长"
    /// 的做法，一个卡住的 URL 重试几次就能让整站进入 10 s→600 s 的全局冷却，
    /// 而冷却期内不可能有成功请求来清除它，形成自我强化的停顿。
    pub fn on_url_failure(&self, host: &str, url_key: &str) {
        let cfg = &self.config;
        self.update(host, |h, _| {
            if h.failed_urls.iter().any(|u| u == url_key) {
                return None;
            }
            h.failed_urls.push(url_key.to_string());
            (h.failed_urls.len() as u32 >= cfg.failure_threshold).then(|| {
                h.failed_urls.clear();
                cfg.base_cooldown
            })
        });
    }

    fn update(&self, host: &str, f: impl FnOnce(&mut Health, Instant) -> Option<Duration>) {
        if !self.config.enabled {
            return;
        }
        let host = host.to_ascii_lowercase();
        let now = Instant::now();
        let mut map = self.shard(&host).lock().expect("lock poisoned");
        if map.hosts.len() >= map.prune_at && !map.hosts.contains_key(&host) {
            let (outages, throttling) = (&self.outages, &self.throttling);
            map.hosts.retain(|_, h| {
                let keep = h.until.is_some_and(|u| u > now);
                if !keep {
                    // 还没恢复就不再跟踪：故障时长右截尾
                    if let Some(t) = h.down_since {
                        outages.abandoned(now - t);
                    }
                    if let Some(t) = h.throttled_since {
                        throttling.abandoned(now - t);
                    }
                }
                keep
            });
            map.prune_at = map.hosts.len().saturating_mul(2).max(self.prune_floor);
        }
        let h = map.hosts.entry(host).or_default();
        if let Some(d) = f(h, now) {
            let until = now + d;
            // 只延长，不缩短已有的冷却。
            h.until = Some(h.until.map_or(until, |u| u.max(until)));
        }
    }
}

/// 第 `step` 次冷却多久：故障已持续 `lasted`，按故障时长分布有把握恢复的时刻；
/// 没把握时用 `base·2^step`。礼貌开着时 `base·2^step` 是下限。都不超过 `max_cooldown`。
fn cooldown(
    cfg: &BackoffConfig,
    outages: &Outages,
    lasted: Duration,
    step: u32,
    polite: bool,
) -> Duration {
    let floor = exp_backoff(cfg, step);
    let learned = outages.retry_in(lasted);
    let d = if polite {
        learned.map_or(floor, |d| d.max(floor))
    } else {
        learned.unwrap_or(floor)
    };
    d.min(cfg.max_cooldown)
}

fn exp_backoff(cfg: &BackoffConfig, step: u32) -> Duration {
    // [待迁移] 指数上限 2^20 只防溢出（0.81 原样），实际被 max_cooldown 截住
    let factor = 2u32.saturating_pow(step.min(20));
    cfg.base_cooldown
        .saturating_mul(factor)
        .min(cfg.max_cooldown)
}

/// 解析 `Retry-After`：秒数或 HTTP-date。
pub fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let v = value.trim();
    if let Ok(secs) = v.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let at = httpdate::parse_http_date(v).ok()?;
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backoff() -> Backoff {
        Backoff::new(BackoffConfig {
            failure_threshold: 3,
            base_cooldown: Duration::from_secs(10),
            max_cooldown: Duration::from_secs(60),
            enabled: true,
        })
    }

    #[tokio::test(start_paused = true)]
    async fn exponential_after_threshold() {
        let b = backoff();
        b.on_failure("a.com");
        b.on_failure("a.com");
        assert!(b.check("a.com").is_ok());
        b.on_failure("a.com");
        assert_eq!(b.check("a.com"), Err(Duration::from_secs(10)));
        assert!(b.check("b.com").is_ok());

        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(b.check("a.com").is_ok());
        b.on_failure("A.com");
        assert_eq!(b.check("a.com"), Err(Duration::from_secs(20)));
        tokio::time::advance(Duration::from_secs(20)).await;
        b.on_failure("a.com");
        assert_eq!(b.check("a.com"), Err(Duration::from_secs(40)));
        tokio::time::advance(Duration::from_secs(40)).await;
        b.on_failure("a.com");
        assert_eq!(b.check("a.com"), Err(Duration::from_secs(60)), "capped");

        tokio::time::advance(Duration::from_secs(60)).await;
        b.on_success("a.com");
        b.on_failure("a.com");
        assert!(b.check("a.com").is_ok(), "success resets the streak");
    }

    #[tokio::test(start_paused = true)]
    async fn one_sticky_bad_url_does_not_freeze_the_host() {
        // 同一个 URL 反复失败（例如永久超时）只是这一页的问题，不该让整站进入冷却。
        let b = backoff();
        for _ in 0..10 {
            b.on_url_failure("a.com", "http://a.com/slow");
        }
        assert!(
            b.check("a.com").is_ok(),
            "retries of a single URL must not pause the whole host"
        );

        // 不同 URL 累计到阈值才冷却，且冷却时长固定为 base_cooldown，不指数增长。
        b.on_url_failure("a.com", "http://a.com/x");
        b.on_url_failure("a.com", "http://a.com/y");
        assert_eq!(b.check("a.com"), Err(Duration::from_secs(10)));
        tokio::time::advance(Duration::from_secs(10)).await;
        for u in ["p", "q", "r"] {
            b.on_url_failure("a.com", u);
        }
        assert_eq!(
            b.check("a.com"),
            Err(Duration::from_secs(10)),
            "URL-level cooldown stays flat instead of doubling"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_is_honored() {
        let b = backoff();
        b.on_throttled("a.com", Some(Duration::from_secs(120)));
        assert_eq!(b.check("a.com"), Err(Duration::from_secs(120)));
        tokio::time::advance(Duration::from_secs(119)).await;
        assert_eq!(b.check("a.com"), Err(Duration::from_secs(1)));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(b.check("a.com").is_ok());

        // 没有 Retry-After：立即冷却，并且逐次加倍。
        b.on_throttled("b.com", None);
        assert_eq!(b.check("b.com"), Err(Duration::from_secs(10)));
        b.on_throttled("b.com", None);
        assert_eq!(b.check("b.com"), Err(Duration::from_secs(20)));
        // 较短的 Retry-After 不会缩短已有冷却。
        b.on_throttled("b.com", Some(Duration::from_secs(1)));
        assert_eq!(b.check("b.com"), Err(Duration::from_secs(20)));
    }

    #[tokio::test(start_paused = true)]
    async fn throttling_does_not_inflate_the_failure_streak() {
        let b = backoff();
        for _ in 0..5 {
            b.on_throttled("a.com", Some(Duration::from_secs(1)));
        }
        tokio::time::advance(Duration::from_secs(1)).await;
        b.on_failure("a.com");
        assert!(
            b.check("a.com").is_ok(),
            "one 5xx after throttling is not a failure streak"
        );
    }

    #[test]
    fn parses_retry_after() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_445_412_480);
        assert_eq!(
            parse_retry_after(" 120 ", now),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:28:30 GMT", now),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:00:00 GMT", now),
            Some(Duration::ZERO)
        );
        assert_eq!(parse_retry_after("soon", now), None);
        assert_eq!(parse_retry_after("-5", now), None);
    }

    #[test]
    fn disabled_backoff_never_cools_down() {
        let b = Backoff::new(BackoffConfig {
            enabled: false,
            ..BackoffConfig::default()
        });
        b.on_throttled("a.com", Some(Duration::from_secs(60)));
        for _ in 0..10 {
            b.on_failure("a.com");
        }
        assert!(b.check("a.com").is_ok() && !b.enabled());
    }

    /// 学到的故障时长分布只能让冷却更长（礼貌下限）：所有故障 2 s 内恢复时，礼貌开着仍冷却 10 s，
    /// 关掉才按分布只冷却约 2 s；故障普遍持续 30–50 s 时，礼貌开着也按分布冷却到约 50 s。
    #[tokio::test(start_paused = true)]
    async fn learned_outages_only_lengthen_cooldowns_when_polite() {
        async fn teach(b: &Backoff, base_ms: u64, spread_ms: u64) {
            for i in 0..200 {
                let host = format!("h{i}.com");
                b.on_failure(&host);
                tokio::time::advance(Duration::from_millis(base_ms + (i % 20) * spread_ms)).await;
                b.on_success(&host);
            }
        }
        let polite = Backoff::new(BackoffConfig::default());
        let rude = Backoff::new(BackoffConfig::default()).with_politeness(false);
        // 每个退避教完马上看：教下一个要推进约 190 s，会让前一个的冷却过期
        teach(&polite, 1, 100).await;
        for _ in 0..3 {
            polite.on_failure("a.com");
        }
        assert_eq!(polite.check("a.com"), Err(Duration::from_secs(10)));
        teach(&rude, 1, 100).await;
        for _ in 0..3 {
            rude.on_failure("a.com");
        }
        let left = rude.check("a.com").unwrap_err();
        assert!(
            left <= Duration::from_secs(2) && left > Duration::ZERO,
            "{left:?}"
        );

        let slow = Backoff::new(BackoffConfig::default());
        teach(&slow, 30_000, 1_000).await;
        for _ in 0..3 {
            slow.on_failure("b.com");
        }
        let left = slow.check("b.com").unwrap_err();
        assert!(
            left > Duration::from_secs(45) && left <= Duration::from_secs(50),
            "{left:?}"
        );
    }
}
