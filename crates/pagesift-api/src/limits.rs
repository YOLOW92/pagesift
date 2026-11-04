//! 按 API key 的限流和配额：每分钟请求数（令牌桶）、同时进行的任务数、每天（UTC）的页面数。
//! 超限时返回 Firecrawl 兼容的 429 错误体。没配置任何限额时什么都不检查。
//!
//! 当天已用的页数写进存储（有的话），重启不会把配额清零。
//!
//! 多服务器网关上（[`Limiter::with_ledger`]）每分钟请求数和每日页数改记在 `pagesift-cluster` 的账本里：
//! 用量是追加的冻结 / 结算记录，崩溃重放、温备接管后一致；令牌桶在账本里，接管时由最近一分钟的
//! 请求记录重建。单主机不经过账本，行为不变。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use pagesift_store::Store;

use crate::error::{ApiError, ErrorCode};

/// 一个 API key 的限额，各项不设就是不限。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyLimits {
    /// 日志、监控和配额记录里用的名字；不设时用 key 的指纹，key 本身不会写进日志或磁盘。
    pub name: Option<String>,
    pub requests_per_minute: Option<u32>,
    pub max_concurrent_jobs: Option<usize>,
    pub pages_per_day: Option<u64>,
}

/// 发起请求的调用方：鉴权中间件放进请求扩展里。
#[derive(Debug, Clone)]
pub(crate) struct Caller {
    pub account: Arc<str>,
    pub limits: Arc<KeyLimits>,
}

impl Caller {
    pub(crate) fn new(key: Option<&str>, limits: Arc<KeyLimits>) -> Self {
        let account = match (&limits.name, key) {
            (Some(n), _) => n.clone(),
            (None, Some(k)) => fingerprint(k),
            (None, None) => "anonymous".into(),
        };
        Self {
            account: account.into(),
            limits,
        }
    }
}

impl Caller {
    /// 恢复任务时按保存的名字找回调用方。
    pub(crate) fn named(account: &str, limits: Arc<KeyLimits>) -> Self {
        Self {
            account: account.into(),
            limits,
        }
    }
}

/// key 的指纹（FNV-1a 64 位），稳定、不可逆到能用的程度，用来区分调用方。
fn fingerprint(key: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("key-{h:016x}")
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

#[derive(Clone, Copy)]
struct Usage {
    day: u64,
    used: u64,
}

#[derive(Default)]
pub(crate) struct Limiter {
    buckets: Mutex<HashMap<Arc<str>, Bucket>>,
    usage: Mutex<HashMap<Arc<str>, Usage>>,
    store: Option<Store>,
    ledger: Option<Arc<pagesift_cluster::Ledger>>,
}

fn rate_error(rpm: u32, wait: u64) -> ApiError {
    ApiError::new(
        ErrorCode::RateLimitExceeded,
        format!(
            "rate limit exceeded: {rpm} requests per minute for this API key; retry after {wait}s"
        ),
    )
    .with_retry_after(wait)
}

fn ledger_error(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::Internal, format!("quota ledger: {e}"))
}

fn today() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400)
}

fn secs_to_midnight() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    86_400 - now % 86_400
}

fn usage_key(account: &str) -> String {
    format!("quota/{account}")
}

impl Limiter {
    pub(crate) fn new(store: Option<Store>) -> Self {
        Self {
            store,
            ..Default::default()
        }
    }

    /// 网关：限额记在账本里。`accounts` 是全部账户（名字 → 限额），事先登记。
    pub(crate) fn with_ledger(
        ledger: Arc<pagesift_cluster::Ledger>,
        accounts: &HashMap<String, Arc<KeyLimits>>,
    ) -> Self {
        for (name, l) in accounts {
            ledger.set_limits(
                name,
                pagesift_cluster::AccountLimits {
                    pages_per_day: l.pages_per_day,
                    requests_per_minute: l.requests_per_minute,
                },
            );
        }
        Self {
            ledger: Some(ledger),
            ..Default::default()
        }
    }

    /// 每分钟请求数：容量等于每分钟请求数的令牌桶，匀速补充，允许短时突发。
    pub(crate) fn check_rate(&self, c: &Caller) -> Result<(), ApiError> {
        let Some(rpm) = c.limits.requests_per_minute else {
            return Ok(());
        };
        if let Some(l) = &self.ledger {
            use pagesift_cluster::Refusal;
            return match l.check_rate(&c.account) {
                Ok(()) => Ok(()),
                Err(Refusal::Rate { retry_after, .. }) => Err(rate_error(
                    rpm,
                    retry_after.as_secs_f64().ceil().max(1.0) as u64,
                )),
                Err(Refusal::Quota { .. }) => Err(quota_error(c)),
                Err(Refusal::Error(e)) => Err(ledger_error(e)),
            };
        }
        let cap = f64::from(rpm.max(1));
        let per_sec = cap / 60.0;
        let now = Instant::now();
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let b = buckets.entry(c.account.clone()).or_insert(Bucket {
            tokens: cap,
            last: now,
        });
        b.tokens = (b.tokens + now.duration_since(b.last).as_secs_f64() * per_sec).min(cap);
        b.last = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            return Ok(());
        }
        let wait = ((1.0 - b.tokens) / per_sec).ceil().max(1.0) as u64;
        Err(rate_error(rpm, wait))
    }

    /// 同时进行的任务数是否还能再加一个。
    pub(crate) fn check_jobs(&self, c: &Caller, active: usize) -> Result<(), ApiError> {
        match c.limits.max_concurrent_jobs {
            Some(max) if active >= max => Err(ApiError::new(
                ErrorCode::ConcurrencyLimitExceeded,
                format!("concurrency limit exceeded: at most {max} crawl or batch jobs may run at once for this API key"),
            )
            .with_retry_after(5)),
            _ => Ok(()),
        }
    }

    fn usage(&self, account: &Arc<str>) -> Usage {
        let day = today();
        let mut map = self.usage.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(u) = map.get(account).filter(|u| u.day == day) {
            return *u;
        }
        // 第一次用到这个调用方（或者跨天了）：从存储里读当天的记录
        let stored = self
            .store
            .as_ref()
            .and_then(|s| s.tasks().kv_get(usage_key(account)).ok().flatten())
            .and_then(|v| {
                let text = String::from_utf8(v).ok()?;
                let (d, n) = text.split_once(' ')?;
                Some(Usage {
                    day: d.parse().ok()?,
                    used: n.parse().ok()?,
                })
            })
            .filter(|u| u.day == day);
        let u = stored.unwrap_or(Usage { day, used: 0 });
        map.insert(account.clone(), u);
        u
    }

    /// 当天还剩多少页；没设配额时是 `None`。
    pub(crate) fn remaining_pages(&self, c: &Caller) -> Option<u64> {
        let max = c.limits.pages_per_day?;
        if let Some(l) = &self.ledger {
            return l.balance(&c.account);
        }
        Some(max.saturating_sub(self.usage(&c.account).used))
    }

    pub(crate) fn check_quota(&self, c: &Caller) -> Result<(), ApiError> {
        if self.remaining_pages(c) == Some(0) {
            return Err(quota_error(c));
        }
        Ok(())
    }

    /// 记入当天用掉的页数。只有设了配额的调用方才计数和落盘。
    pub(crate) async fn consume_pages(&self, c: &Caller, n: u64) {
        if c.limits.pages_per_day.is_none() || n == 0 {
            return;
        }
        if let Some(l) = &self.ledger {
            // 冻结后立即按实际用量结算：页面已经抓完。余额不够冻结时（并发请求都通过了检查）
            // 当天已经用满，不再记账，余额照样是 0
            match l.reserve(&c.account, n) {
                Ok(hold) => {
                    if let Err(e) = l.settle(&hold, n) {
                        tracing::warn!(error = %e, "settling page quota failed");
                    }
                }
                Err(pagesift_cluster::Refusal::Error(e)) => {
                    tracing::warn!(error = %e, "reserving page quota failed");
                }
                Err(_) => {}
            }
            return;
        }
        let u = self.usage(&c.account);
        let u = {
            let mut map = self.usage.lock().unwrap_or_else(|e| e.into_inner());
            let e = map.entry(c.account.clone()).or_insert(u);
            if e.day != u.day {
                *e = u;
            }
            e.used += n;
            *e
        };
        if let Some(store) = &self.store {
            let key = usage_key(&c.account);
            let value = format!("{} {}", u.day, u.used);
            let r = store
                .blocking(move |s| s.tasks().kv_put(key, value.as_bytes()))
                .await;
            if let Err(e) = r {
                tracing::warn!(error = %e, "saving page quota usage failed");
            }
        }
    }
}

pub(crate) fn quota_error(c: &Caller) -> ApiError {
    ApiError::new(
        ErrorCode::QuotaExceeded,
        format!(
            "daily page quota exceeded: {} pages per day for this API key",
            c.limits.pages_per_day.unwrap_or(0)
        ),
    )
    .with_retry_after(secs_to_midnight())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller(limits: KeyLimits) -> Caller {
        Caller::new(Some("secret-key"), Arc::new(limits))
    }

    #[test]
    fn token_bucket_allows_a_burst_then_429s() {
        let l = Limiter::default();
        let c = caller(KeyLimits {
            requests_per_minute: Some(3),
            ..Default::default()
        });
        for _ in 0..3 {
            l.check_rate(&c).unwrap();
        }
        let e = l.check_rate(&c).unwrap_err();
        assert_eq!(e.code, ErrorCode::RateLimitExceeded);
        assert!(e.retry_after.unwrap() >= 1);
        // 别的调用方有自己的桶
        let other = Caller::new(Some("other"), c.limits.clone());
        l.check_rate(&other).unwrap();
        // 不限速的调用方不受影响
        l.check_rate(&caller(KeyLimits::default())).unwrap();
    }

    #[test]
    fn accounts_use_names_or_fingerprints() {
        let named = Caller::new(
            Some("k"),
            Arc::new(KeyLimits {
                name: Some("team-a".into()),
                ..Default::default()
            }),
        );
        assert_eq!(&*named.account, "team-a");
        let anon = caller(KeyLimits::default());
        assert!(anon.account.starts_with("key-") && !anon.account.contains("secret"));
        assert_eq!(&*Caller::new(None, Arc::default()).account, "anonymous");
    }

    /// 网关：限额记在账本里，行为与单主机一致（桶先突发后 429，配额用完 QUOTA_EXCEEDED）。
    #[tokio::test]
    async fn ledger_backed_limits_match_the_local_ones() {
        let ledger = Arc::new(
            pagesift_cluster::Ledger::open(pagesift_cluster::LedgerConfig::new(
                pagesift_cluster::MonoClock::new(),
                false,
            ))
            .unwrap(),
        );
        let limits = Arc::new(KeyLimits {
            requests_per_minute: Some(2),
            pages_per_day: Some(2),
            ..Default::default()
        });
        let c = Caller::new(Some("k"), limits.clone());
        let accounts = HashMap::from([(c.account.to_string(), limits)]);
        let l = Limiter::with_ledger(ledger, &accounts);
        l.check_rate(&c).unwrap();
        l.check_rate(&c).unwrap();
        let e = l.check_rate(&c).unwrap_err();
        assert_eq!(e.code, ErrorCode::RateLimitExceeded);
        assert!(e.retry_after.unwrap() >= 1);
        assert_eq!(l.remaining_pages(&c), Some(2));
        l.consume_pages(&c, 1).await;
        assert!(l.check_quota(&c).is_ok());
        l.consume_pages(&c, 1).await;
        assert_eq!(l.remaining_pages(&c), Some(0));
        assert_eq!(
            l.check_quota(&c).unwrap_err().code,
            ErrorCode::QuotaExceeded
        );
        // 不限额的调用方不受影响
        l.check_rate(&caller(KeyLimits::default())).unwrap();
    }

    #[tokio::test]
    async fn page_quota_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let c = caller(KeyLimits {
            pages_per_day: Some(5),
            ..Default::default()
        });
        {
            let store = Store::open_dir(dir.path()).unwrap();
            let l = Limiter::new(Some(store));
            assert_eq!(l.remaining_pages(&c), Some(5));
            l.consume_pages(&c, 4).await;
            assert!(l.check_quota(&c).is_ok());
            l.consume_pages(&c, 1).await;
            assert_eq!(
                l.check_quota(&c).unwrap_err().code,
                ErrorCode::QuotaExceeded
            );
        }
        let l = Limiter::new(Some(Store::open_dir(dir.path()).unwrap()));
        assert_eq!(l.remaining_pages(&c), Some(0));
        let jobs = caller(KeyLimits {
            max_concurrent_jobs: Some(1),
            ..Default::default()
        });
        assert!(l.check_jobs(&jobs, 0).is_ok());
        assert_eq!(
            l.check_jobs(&jobs, 1).unwrap_err().code,
            ErrorCode::ConcurrencyLimitExceeded
        );
    }
}
