//! 自适应退避：连续失败后指数退避，429/503 遵守 Retry-After。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use tokio::time::Instant;

#[derive(Debug, Clone)]
pub struct BackoffConfig {
    /// 连续失败多少次后进入冷却。
    pub failure_threshold: u32,
    pub base_cooldown: Duration,
    pub max_cooldown: Duration,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 3,
            base_cooldown: Duration::from_secs(10),
            max_cooldown: Duration::from_secs(600),
        }
    }
}

#[derive(Debug, Default)]
struct Health {
    /// 连续的 5xx、网络错误、超时
    failures: u32,
    /// 本轮已失败的**不同 URL**（同一 URL 的重试不再累加）。
    /// 单个永久失败的 URL 不该让整个站点进入指数冷却。
    failed_urls: Vec<String>,
    /// 连续的限流（429、带 Retry-After 的 503）。和 `failures` 分开计：几次限流不该让一次偶发的 5xx
    /// 直接跳到很长的冷却
    throttles: u32,
    until: Option<Instant>,
}

pub struct Backoff {
    config: BackoffConfig,
    hosts: Mutex<HashMap<String, Health>>,
}

const PRUNE_AT: usize = 4096;
/// 单个主机最多记住多少个失败 URL，防止内存随坏链增长。
const MAX_TRACKED_FAILED_URLS: usize = 64;

impl Backoff {
    pub fn new(config: BackoffConfig) -> Self {
        Self {
            config,
            hosts: Mutex::new(HashMap::new()),
        }
    }

    /// 主机在冷却期内时返回剩余时间。
    pub fn check(&self, host: &str) -> Result<(), Duration> {
        let hosts = self.hosts.lock().expect("lock poisoned");
        let now = Instant::now();
        match hosts.get(&host.to_ascii_lowercase()).and_then(|h| h.until) {
            Some(until) if until > now => Err(until - now),
            _ => Ok(()),
        }
    }

    pub fn on_success(&self, host: &str) {
        self.hosts
            .lock()
            .expect("lock poisoned")
            .remove(&host.to_ascii_lowercase());
    }

    /// 5xx、网络错误、超时。
    pub fn on_failure(&self, host: &str) {
        let cfg = &self.config;
        self.update(host, |h| {
            h.failures = h.failures.saturating_add(1);
            (h.failures >= cfg.failure_threshold)
                .then(|| exp_backoff(cfg, h.failures - cfg.failure_threshold))
        });
    }

    /// 429 或 503。有 Retry-After 时按它冷却，否则立即按指数退避冷却。
    pub fn on_throttled(&self, host: &str, retry_after: Option<Duration>) {
        let cfg = &self.config;
        self.update(host, |h| {
            h.throttles = h.throttles.saturating_add(1);
            Some(retry_after.unwrap_or_else(|| exp_backoff(cfg, h.throttles - 1)))
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
        self.update(host, |h| {
            if h.failed_urls.iter().any(|u| u == url_key) {
                return None;
            }
            if h.failed_urls.len() >= MAX_TRACKED_FAILED_URLS {
                h.failed_urls.clear();
            }
            h.failed_urls.push(url_key.to_string());
            (h.failed_urls.len() as u32 >= cfg.failure_threshold).then(|| {
                h.failed_urls.clear();
                cfg.base_cooldown
            })
        });
    }

    fn update(&self, host: &str, f: impl FnOnce(&mut Health) -> Option<Duration>) {
        let mut hosts = self.hosts.lock().expect("lock poisoned");
        let now = Instant::now();
        if hosts.len() >= PRUNE_AT {
            hosts.retain(|_, h| h.until.is_some_and(|u| u > now));
        }
        let h = hosts.entry(host.to_ascii_lowercase()).or_default();
        if let Some(d) = f(h) {
            let until = now + d;
            // 只延长，不缩短已有的冷却。
            h.until = Some(h.until.map_or(until, |u| u.max(until)));
        }
    }
}

fn exp_backoff(cfg: &BackoffConfig, step: u32) -> Duration {
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
}
