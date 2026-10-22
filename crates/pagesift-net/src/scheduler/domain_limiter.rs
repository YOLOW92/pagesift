//! 域名级自适应 Token-Bucket 速率池与抓取礼仪限速器。
//! 严格控制每域名并发上限（Inflight）与请求间隔，支持针对 429/503 的自适应退避惩罚。

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct Bucket {
    tokens: f64,
    last_update: Instant,
    fill_rate: f64, // 每秒补充 token 数
    capacity: f64,  // 突发桶容量
    inflight: usize,
    max_inflight: usize,
    penalty_until: Option<Instant>,
    consecutive_penalties: u32,
}

impl Bucket {
    fn new(fill_rate: f64, max_inflight: usize) -> Self {
        let cap = (fill_rate * 2.0).max(1.0);
        Self {
            tokens: cap,
            last_update: Instant::now(),
            fill_rate,
            capacity: cap,
            inflight: 0,
            max_inflight: max_inflight.max(1),
            penalty_until: None,
            consecutive_penalties: 0,
        }
    }

    fn refresh(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last_update).as_secs_f64();
        self.last_update = now;
        if self.fill_rate > 0.0 {
            self.tokens = (self.tokens + elapsed * self.fill_rate).min(self.capacity);
        } else {
            // fill_rate == 0 表示不限速
            self.tokens = self.capacity;
        }
    }

    fn can_acquire(&mut self, now: Instant) -> bool {
        if let Some(until) = self.penalty_until {
            if now < until {
                return false;
            }
            self.penalty_until = None;
        }
        if self.inflight >= self.max_inflight {
            return false;
        }
        self.refresh(now);
        self.tokens >= 1.0
    }

    fn acquire(&mut self, now: Instant) -> bool {
        if !self.can_acquire(now) {
            return false;
        }
        self.tokens -= 1.0;
        self.inflight += 1;
        true
    }

    fn release(&mut self, status_code: Option<u16>, now: Instant) {
        self.inflight = self.inflight.saturating_sub(1);
        if let Some(code) = status_code {
            if code == 429 || code == 503 {
                self.consecutive_penalties = (self.consecutive_penalties + 1).min(8);
                let backoff_secs = 2f64.powi(self.consecutive_penalties as i32);
                self.penalty_until = Some(now + Duration::from_secs_f64(backoff_secs));
                self.tokens = 0.0;
            } else if code < 400 {
                if self.consecutive_penalties > 0 {
                    self.consecutive_penalties -= 1;
                }
            }
        }
    }
}

/// 域名级并发与速率限制器。
#[derive(Debug)]
pub struct DomainLimiter {
    buckets: RwLock<HashMap<String, Bucket>>,
    default_rate: f64,
    default_max_inflight: usize,
}

impl DomainLimiter {
    /// 创建限速器：default_rate 为默认每秒请求数（0 表示不限速），default_max_inflight 为单域名并发上限。
    pub fn new(default_rate: f64, default_max_inflight: usize) -> Self {
        Self {
            buckets: RwLock::new(HashMap::new()),
            default_rate,
            default_max_inflight,
        }
    }

    /// 设置特定域名的抓取礼仪策略（如从 robots.txt 获得的 Crawl-Delay）。
    pub fn set_policy(&self, domain: &str, rate_per_sec: f64, max_inflight: usize) {
        let mut map = self.buckets.write().expect("lock poisoned");
        let b = map
            .entry(domain.to_string())
            .or_insert_with(|| Bucket::new(rate_per_sec, max_inflight));
        b.fill_rate = rate_per_sec;
        b.capacity = (rate_per_sec * 2.0).max(1.0);
        b.max_inflight = max_inflight.max(1);
    }

    /// 尝试获取派发许可：扣减一个 Token 并增加 inflight 计数。
    pub fn acquire(&self, domain: &str) -> bool {
        let now = Instant::now();
        let mut map = self.buckets.write().expect("lock poisoned");
        let def_rate = self.default_rate;
        let def_inflight = self.default_max_inflight;
        let bucket = map
            .entry(domain.to_string())
            .or_insert_with(|| Bucket::new(def_rate, def_inflight));
        bucket.acquire(now)
    }

    /// 检查指定域名当前是否允许派发新请求（不消耗 Token）。
    pub fn can_acquire(&self, domain: &str) -> bool {
        let now = Instant::now();
        let mut map = self.buckets.write().expect("lock poisoned");
        let def_rate = self.default_rate;
        let def_inflight = self.default_max_inflight;
        let bucket = map
            .entry(domain.to_string())
            .or_insert_with(|| Bucket::new(def_rate, def_inflight));
        bucket.can_acquire(now)
    }

    /// 请求完成后释放并发占位，并依据响应状态码决定是否实施自适应退避。
    pub fn release(&self, domain: &str, status_code: Option<u16>) {
        let now = Instant::now();
        let mut map = self.buckets.write().expect("lock poisoned");
        if let Some(bucket) = map.get_mut(domain) {
            bucket.release(status_code, now);
        }
    }

    /// 当前被追踪的独立域名总数。
    pub fn tracked_domains(&self) -> usize {
        self.buckets.read().expect("lock poisoned").len()
    }
}
