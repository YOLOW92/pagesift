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

/// 被限流（429）后，主机的请求间隔至少是这么多，再按次加倍。
const PENALTY_START: Duration = Duration::from_millis(250);
const PENALTY_MAX: Duration = Duration::from_secs(30);
/// 每成功一次，额外间隔减掉它的 1/PENALTY_DECAY；小于 PENALTY_FLOOR 就清零。
const PENALTY_DECAY: u32 = 16;
const PENALTY_FLOOR: Duration = Duration::from_millis(10);

#[derive(Default)]
struct Penalty {
    value: Duration,
    /// 上次加倍的时间：同一批在途请求一起收到 429 只算一次，不连着翻好几倍
    raised_at: Option<Instant>,
}

struct HostState {
    slots: Arc<Semaphore>,
    /// 限流学到的额外间隔：429 时加倍（乘性增），成功时慢慢缩短。和 TCP 拥塞控制的 AIMD 同一个思路，
    /// 让请求速度停在站点限额之下，而不是冷却结束就全速冲上去、再被限流。
    penalty: Arc<Mutex<Penalty>>,
    /// 排队等待发出请求的门：同一时刻只有一个请求在等间隔，保证间隔按实际开始时间算。
    gate: Arc<tokio::sync::Mutex<Option<Instant>>>,
}

/// 持有期间占用一个全局并发名额和一个主机并发名额。
#[derive(Debug)]
pub struct Permit {
    _global: OwnedSemaphorePermit,
    _host: OwnedSemaphorePermit,
    waited: Duration,
    interval: Duration,
    robots_crawl_delay: bool,
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
        let started = Instant::now();
        let (slots, gate, penalty) = self.host(host);
        let interval = crawl_delay
            .unwrap_or(self.config.default_interval)
            .max(penalty.lock().expect("lock poisoned").value);
        let host_permit = slots.acquire_owned().await.expect("semaphore never closed");
        let mut last = gate.lock().await;
        // 截止时间已过就直接放行：进定时器至少要等到下一个 1 ms 刻度，
        // 同一主机的请求排在这道门后面，会被串成每毫秒一个
        if let Some(prev) = *last
            && prev + interval > Instant::now()
        {
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
            waited: started.elapsed(),
            interval,
            robots_crawl_delay: crawl_delay.is_some(),
        }
    }

    /// 主机返回了 429（或带 Retry-After 的 503）：加大它的请求间隔。
    pub fn throttled(&self, host: &str) {
        let (_, _, penalty) = self.host(host);
        let mut p = penalty.lock().expect("lock poisoned");
        let now = Instant::now();
        if p.raised_at.is_some_and(|t| now < t + p.value) {
            return;
        }
        p.value = (p.value * 2).clamp(PENALTY_START, PENALTY_MAX);
        p.raised_at = Some(now);
    }

    /// 主机正常响应：限流学到的额外间隔慢慢缩短。
    pub fn succeeded(&self, host: &str) {
        let hosts = self.hosts.lock().expect("lock poisoned");
        if let Some(s) = hosts.get(&host.to_ascii_lowercase()) {
            let mut p = s.penalty.lock().expect("lock poisoned");
            if !p.value.is_zero() {
                let cut = p.value / PENALTY_DECAY;
                p.value -= cut;
                if p.value < PENALTY_FLOOR {
                    p.value = Duration::ZERO;
                }
            }
        }
    }

    #[cfg(test)]
    fn penalty(&self, host: &str) -> Duration {
        self.host(host).2.lock().unwrap().value
    }

    #[allow(clippy::type_complexity)]
    fn host(
        &self,
        host: &str,
    ) -> (
        Arc<Semaphore>,
        Arc<tokio::sync::Mutex<Option<Instant>>>,
        Arc<Mutex<Penalty>>,
    ) {
        let key = host.to_ascii_lowercase();
        let mut hosts = self.hosts.lock().expect("lock poisoned");
        if hosts.len() >= PRUNE_AT && !hosts.contains_key(&key) {
            let max_interval = self.config.default_interval.max(Duration::from_secs(60));
            let now = Instant::now();
            hosts.retain(|_, s| {
                let busy = Arc::strong_count(&s.slots) > 1
                    || Arc::strong_count(&s.gate) > 1
                    || !s.penalty.lock().map_or(true, |p| p.value.is_zero());
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
            penalty: Arc::new(Mutex::new(Penalty::default())),
        });
        (s.slots.clone(), s.gate.clone(), s.penalty.clone())
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
}
