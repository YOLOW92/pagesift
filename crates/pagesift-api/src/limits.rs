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

    fn load_usage(&self, account: &Arc<str>, day: u64) -> Usage {
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
        stored.unwrap_or(Usage { day, used: 0 })
    }

    fn usage_entry<'a>(
        &self,
        map: &'a mut HashMap<Arc<str>, Usage>,
        account: &Arc<str>,
        day: u64,
    ) -> &'a mut Usage {
        if map.get(account).is_none_or(|u| u.day != day) {
            let u = self.load_usage(account, day);
            map.insert(account.clone(), u);
        }
        map.get_mut(account).expect("inserted above")
    }

    fn usage(&self, account: &Arc<str>) -> Usage {
        let day = today();
        let mut map = self.usage.lock().unwrap_or_else(|e| e.into_inner());
        *self.usage_entry(&mut map, account, day)
    }

    /// 当天还剩多少页；没设配额时是 `None`。已预留但还没结算的页也算在用量里。
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

    /// 可计费操作发出之前原子地预留一页。返回的 [`PageHold`] 在成功时 `commit`，
    /// 失败/取消时直接丢弃即自动退还（[`Drop`] 里完成），不会出现 check-then-act 的并发窗口。
    ///
    /// 账本模式下，进程在预留尚未结算时崩溃，冻结会保留到当天结束（UTC）：最多是崩溃瞬间
    /// 在途的页数，不会双重扣费；单机模式的内存预留则随进程消失。
    pub(crate) async fn reserve_page(self: &Arc<Self>, c: &Caller) -> Result<PageHold, ApiError> {
        let day = today();
        let amount = 1;
        let Some(_) = c.limits.pages_per_day else {
            return Ok(PageHold {
                limiter: self.clone(),
                account: c.account.clone(),
                day,
                amount,
                ledger: None,
                tracked: false,
                settled: false,
            });
        };
        if let Some(ledger) = &self.ledger {
            let hold = ledger
                .reserve(&c.account, amount)
                .map_err(|err| match err {
                    pagesift_cluster::Refusal::Error(err) => ledger_error(err),
                    _ => quota_error(c),
                })?;
            return Ok(PageHold {
                limiter: self.clone(),
                account: c.account.clone(),
                day,
                amount,
                ledger: Some(hold),
                tracked: true,
                settled: false,
            });
        }
        let max = c.limits.pages_per_day.unwrap_or(0);
        {
            let mut map = self.usage.lock().unwrap_or_else(|e| e.into_inner());
            let u = self.usage_entry(&mut map, &c.account, day);
            if u.used.saturating_add(amount) > max {
                return Err(quota_error(c));
            }
            u.used += amount;
        }
        Ok(PageHold {
            limiter: self.clone(),
            account: c.account.clone(),
            day,
            amount,
            ledger: None,
            tracked: true,
            settled: false,
        })
    }

    /// 退还预留（没有结算就丢弃）。只退当天的记录，跨天时旧记录已经作废。
    fn refund(&self, account: &Arc<str>, day: u64, amount: u64) {
        let mut map = self.usage.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(u) = map.get_mut(account)
            && u.day == day
        {
            u.used = u.used.saturating_sub(amount);
        }
    }

    async fn persist_usage(&self, account: &Arc<str>) {
        let Some(store) = &self.store else {
            return;
        };
        let u = {
            let map = self.usage.lock().unwrap_or_else(|e| e.into_inner());
            match map.get(account) {
                Some(u) => *u,
                None => return,
            }
        };
        let key = usage_key(account);
        let value = format!("{} {}", u.day, u.used);
        let r = store
            .blocking(move |s| s.tasks().kv_put(key, value.as_bytes()))
            .await;
        if let Err(e) = r {
            tracing::warn!(error = %e, "saving page quota usage failed");
        }
    }
}

/// 一页的预留。`commit` 结算；未结算就丢弃时自动退还（失败、取消、panic 都走同一条路）。
pub(crate) struct PageHold {
    limiter: Arc<Limiter>,
    account: Arc<str>,
    day: u64,
    amount: u64,
    ledger: Option<pagesift_cluster::Hold>,
    tracked: bool,
    settled: bool,
}

impl PageHold {
    pub(crate) async fn commit(mut self) {
        self.settled = true;
        if !self.tracked {
            return;
        }
        if let Some(ledger) = &self.limiter.ledger {
            if let Some(hold) = self.ledger.take()
                && let Err(e) = ledger.settle(&hold, self.amount)
            {
                tracing::warn!(error = %e, "settling page quota failed");
            }
            return;
        }
        self.limiter.persist_usage(&self.account).await;
    }
}

impl Drop for PageHold {
    fn drop(&mut self) {
        if self.settled || !self.tracked {
            return;
        }
        if let Some(ledger) = &self.limiter.ledger {
            if let Some(hold) = self.ledger.take()
                && let Err(e) = ledger.release(&hold)
            {
                tracing::warn!(error = %e, "releasing page quota failed");
            }
            return;
        }
        self.limiter.refund(&self.account, self.day, self.amount);
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
        let l = Arc::new(Limiter::with_ledger(ledger, &accounts));
        l.check_rate(&c).unwrap();
        l.check_rate(&c).unwrap();
        let e = l.check_rate(&c).unwrap_err();
        assert_eq!(e.code, ErrorCode::RateLimitExceeded);
        assert!(e.retry_after.unwrap() >= 1);
        assert_eq!(l.remaining_pages(&c), Some(2));
        l.reserve_page(&c).await.unwrap().commit().await;
        assert!(l.check_quota(&c).is_ok());
        l.reserve_page(&c).await.unwrap().commit().await;
        assert_eq!(l.remaining_pages(&c), Some(0));
        assert_eq!(
            l.check_quota(&c).unwrap_err().code,
            ErrorCode::QuotaExceeded
        );
        // 不限额的调用方不受影响
        l.check_rate(&caller(KeyLimits::default())).unwrap();
    }

    #[tokio::test]
    async fn concurrent_page_reservations_stop_at_quota() {
        let c = caller(KeyLimits {
            pages_per_day: Some(1),
            ..Default::default()
        });
        let limiter = Arc::new(Limiter::new(None));
        let (a, b) = tokio::join!(limiter.reserve_page(&c), limiter.reserve_page(&c));
        assert_ne!(a.is_ok(), b.is_ok());
        assert_eq!(limiter.remaining_pages(&c), Some(0));
        // 丢掉未结算的预留：额度回来
        drop(a);
        drop(b);
        assert_eq!(limiter.remaining_pages(&c), Some(1));
    }

    #[tokio::test]
    async fn twenty_concurrent_reservations_allow_exactly_one() {
        let c = Arc::new(caller(KeyLimits {
            pages_per_day: Some(1),
            ..Default::default()
        }));
        let limiter = Arc::new(Limiter::new(None));
        let mut tasks = Vec::new();
        for _ in 0..20 {
            let (limiter, c) = (limiter.clone(), c.clone());
            tasks.push(tokio::spawn(async move { limiter.reserve_page(&c).await }));
        }
        let mut granted = 0;
        let mut holds = Vec::new();
        for task in tasks {
            match task.await.unwrap() {
                Ok(hold) => {
                    granted += 1;
                    holds.push(hold);
                }
                Err(e) => assert_eq!(e.code, ErrorCode::QuotaExceeded),
            }
        }
        assert_eq!(granted, 1);
        assert_eq!(limiter.remaining_pages(&c), Some(0));
        drop(holds);
        assert_eq!(limiter.remaining_pages(&c), Some(1));
    }

    #[tokio::test]
    async fn committed_pages_survive_restart_and_holds_refund() {
        let dir = tempfile::tempdir().unwrap();
        let c = caller(KeyLimits {
            pages_per_day: Some(2),
            ..Default::default()
        });
        let limiter = Arc::new(Limiter::new(Some(Store::open_dir(dir.path()).unwrap())));
        let hold = limiter.reserve_page(&c).await.unwrap();
        assert_eq!(limiter.remaining_pages(&c), Some(1));
        drop(hold);
        assert_eq!(limiter.remaining_pages(&c), Some(2));
        let hold = limiter.reserve_page(&c).await.unwrap();
        hold.commit().await;
        assert_eq!(limiter.remaining_pages(&c), Some(1));
        drop(limiter);
        // 重启：已结算的 1 页还在，未结算的预留不落盘
        let limiter = Limiter::new(Some(Store::open_dir(dir.path()).unwrap()));
        assert_eq!(limiter.remaining_pages(&c), Some(1));
    }

    #[tokio::test]
    async fn ledger_reservations_are_atomic_across_limiter_instances() {
        let ledger = Arc::new(
            pagesift_cluster::Ledger::open(pagesift_cluster::LedgerConfig::new(
                pagesift_cluster::MonoClock::new(),
                false,
            ))
            .unwrap(),
        );
        let limits = Arc::new(KeyLimits {
            pages_per_day: Some(1),
            ..Default::default()
        });
        let c = Arc::new(Caller::new(Some("k"), limits.clone()));
        let accounts = HashMap::from([(c.account.to_string(), limits)]);
        let a = Arc::new(Limiter::with_ledger(ledger.clone(), &accounts));
        let b = Arc::new(Limiter::with_ledger(ledger.clone(), &accounts));
        let mut tasks = Vec::new();
        for i in 0..20 {
            let (l, c) = (if i % 2 == 0 { a.clone() } else { b.clone() }, c.clone());
            tasks.push(tokio::spawn(async move { l.reserve_page(&c).await }));
        }
        let mut holds = Vec::new();
        let mut granted = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(hold) => {
                    granted += 1;
                    holds.push(hold);
                }
                Err(e) => assert_eq!(e.code, ErrorCode::QuotaExceeded),
            }
        }
        assert_eq!(granted, 1);
        assert_eq!(a.remaining_pages(&c), Some(0));
        drop(holds);
        assert_eq!(a.remaining_pages(&c), Some(1));
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
            let l = Arc::new(Limiter::new(Some(store)));
            assert_eq!(l.remaining_pages(&c), Some(5));
            for _ in 0..4 {
                l.reserve_page(&c).await.unwrap().commit().await;
            }
            assert!(l.check_quota(&c).is_ok());
            l.reserve_page(&c).await.unwrap().commit().await;
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
