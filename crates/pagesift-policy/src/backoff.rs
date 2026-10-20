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
    failures: u32,
    until: Option<Instant>,
}

pub struct Backoff {
    config: BackoffConfig,
    hosts: Mutex<HashMap<String, Health>>,
}

const PRUNE_AT: usize = 4096;

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
            h.failures = h.failures.saturating_add(1);
            Some(retry_after.unwrap_or_else(|| exp_backoff(cfg, h.failures - 1)))
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
