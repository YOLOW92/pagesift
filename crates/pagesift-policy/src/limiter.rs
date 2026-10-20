//! 按主机限速和并发上限。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// 同一主机两次请求开始之间的最小间隔；robots.txt 有 Crawl-delay 时以它为准。
    pub default_interval: Duration,
    pub global_concurrency: usize,
    pub per_host_concurrency: usize,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            default_interval: Duration::from_secs(1),
            global_concurrency: 64,
            per_host_concurrency: 2,
        }
    }
}

struct HostState {
    slots: Arc<Semaphore>,
    /// 排队等待发出请求的门：同一时刻只有一个请求在等间隔，保证间隔按实际开始时间算。
    gate: Arc<tokio::sync::Mutex<Option<Instant>>>,
}

/// 持有期间占用一个全局并发名额和一个主机并发名额。
#[derive(Debug)]
pub struct Permit {
    _global: OwnedSemaphorePermit,
    _host: OwnedSemaphorePermit,
}

pub struct RateLimiter {
    config: RateLimitConfig,
    global: Arc<Semaphore>,
    hosts: Mutex<HashMap<String, HostState>>,
}

/// 主机表超过这个大小时清理空闲主机，避免长时间运行时内存增长。
const PRUNE_AT: usize = 4096;

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            global: Arc::new(Semaphore::new(config.global_concurrency.max(1))),
            config,
            hosts: Mutex::new(HashMap::new()),
        }
    }

    pub fn config(&self) -> &RateLimitConfig {
        &self.config
    }

    /// 等到可以向 `host` 发请求。`crawl_delay` 来自 robots.txt，优先于默认间隔。
    pub async fn acquire(&self, host: &str, crawl_delay: Option<Duration>) -> Permit {
        let interval = crawl_delay.unwrap_or(self.config.default_interval);
        let (slots, gate) = self.host(host);
        let host_permit = slots.acquire_owned().await.expect("semaphore never closed");
        let mut last = gate.lock().await;
        if let Some(prev) = *last {
            tokio::time::sleep_until(prev + interval).await;
        }
        let global = self
            .global
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore never closed");
        *last = Some(Instant::now());
        Permit {
            _global: global,
            _host: host_permit,
        }
    }

    fn host(&self, host: &str) -> (Arc<Semaphore>, Arc<tokio::sync::Mutex<Option<Instant>>>) {
        let key = host.to_ascii_lowercase();
        let mut hosts = self.hosts.lock().expect("lock poisoned");
        if hosts.len() >= PRUNE_AT && !hosts.contains_key(&key) {
            let max_interval = self.config.default_interval.max(Duration::from_secs(60));
            let now = Instant::now();
            hosts.retain(|_, s| {
                let busy = Arc::strong_count(&s.slots) > 1 || Arc::strong_count(&s.gate) > 1;
                let recent = s
                    .gate
                    .try_lock()
                    .map_or(true, |g| g.is_some_and(|t| now < t + max_interval));
                busy || recent
            });
        }
        let s = hosts.entry(key).or_insert_with(|| HostState {
            slots: Arc::new(Semaphore::new(self.config.per_host_concurrency.max(1))),
            gate: Arc::new(tokio::sync::Mutex::new(None)),
        });
        (s.slots.clone(), s.gate.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(per_host: usize, global: usize) -> Arc<RateLimiter> {
        Arc::new(RateLimiter::new(RateLimitConfig {
            default_interval: Duration::from_secs(1),
            global_concurrency: global,
            per_host_concurrency: per_host,
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
        drop(l.acquire("a.com", Some(Duration::from_secs(5))).await);
        drop(l.acquire("A.COM", Some(Duration::from_secs(5))).await);
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
}
