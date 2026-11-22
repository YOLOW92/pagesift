//! 按 API key 的限流和配额：每分钟请求数（令牌桶）、同时进行的任务数、每天（UTC）的页面数。
//! 超限时返回 Firecrawl 兼容的 429 错误体。没配置任何限额时什么都不检查。
//!
//! 当天已用的页数写进存储（有的话），重启不会把配额清零。
//!
//! 多服务器网关上（[`Limiter::with_ledger`]）每分钟请求数和每日页数改记在 `pagesift-cluster` 的账本里：
//! 用量是追加的冻结 / 结算记录，崩溃重放、温备接管后一致；令牌桶在账本里，接管时由最近一分钟的
//! 请求记录重建。单主机不经过账本，行为不变。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use pagesift_store::Store;

use crate::error::{ApiError, ErrorCode};
use crate::metrics::Metrics;

/// 一个 API key 的限额，各项不设就是不限。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyLimits {
    /// 日志和监控展示用的显示名（纯标签，不决定租户/配额/会话身份归属）。
    pub name: Option<String>,
    /// 显式逻辑账户标识：仅当多个 API key 显式配置相同的 `account` 时，才作为同一逻辑账户
    /// 共享配额、任务归属、Context/Graph 租户命名空间与 ITNA Cookie 会话池。
    /// 不设时每个 key 独立使用自身的 SHA-256 指纹，互不共享。
    pub account: Option<String>,
    pub requests_per_minute: Option<u32>,
    pub max_concurrent_jobs: Option<usize>,
    pub pages_per_day: Option<u64>,
}

impl KeyLimits {
    /// Create an empty [`KeyLimits`] with no caps (forward-compatible builder alternative to
    /// exhaustive struct literals).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the display/logging label (`name` does not merge tenants or quotas).
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set an explicit logical account ID shared by all keys declaring the same `account`.
    pub fn with_account(mut self, account: impl Into<String>) -> Self {
        self.account = Some(account.into());
        self
    }

    /// Cap requests per minute.
    pub fn with_requests_per_minute(mut self, rpm: u32) -> Self {
        self.requests_per_minute = Some(rpm);
        self
    }

    /// Cap concurrent async jobs.
    pub fn with_max_concurrent_jobs(mut self, jobs: usize) -> Self {
        self.max_concurrent_jobs = Some(jobs);
        self
    }

    /// Cap UTC daily pages.
    pub fn with_pages_per_day(mut self, pages: u64) -> Self {
        self.pages_per_day = Some(pages);
        self
    }
}

/// 校验显式配置的逻辑账户 ID，拒绝空白、控制字符以及会与匿名/指纹命名空间冲突的保留前缀。
pub fn validate_account_id(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("account id must not be empty");
    }
    // [策略] 显式逻辑账户 ID 最大长度 128 字节，避免日志与存储键膨胀
    const MAX_ACCOUNT_ID_BYTES: usize = 128;
    if trimmed.len() > MAX_ACCOUNT_ID_BYTES {
        return Err("account id must not exceed 128 bytes");
    }
    if trimmed
        .chars()
        .any(|c| c.is_control() || c == '/' || c == '|')
    {
        return Err("account id must not contain control characters, '/', or '|'");
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower == "anonymous"
        || crate::types::ScrapeOptions::is_anonymous_owner(trimmed)
        || lower.starts_with(crate::types::ScrapeOptions::ANONYMOUS_OWNER)
        || lower.starts_with("key-")
    {
        return Err("account id uses a reserved prefix ('anonymous', '__http_anon__', or 'key-')");
    }
    Ok(trimmed.to_owned())
}

/// 发起请求的调用方：鉴权中间件放进请求扩展里。
#[derive(Debug, Clone)]
pub(crate) struct Caller {
    /// 稳定且可信的账户标识：显式 `KeyLimits.account` 或该 key 的密码学指纹（未鉴权为 `"anonymous"`）。
    pub account: Arc<str>,
    /// 供结构化日志与观测展示的名字：优先取 `KeyLimits.name`，未设时回退到 `account`。
    pub display_name: Arc<str>,
    pub limits: Arc<KeyLimits>,
    pub authenticated: bool,
}

impl Caller {
    pub(crate) fn new(key: Option<&str>, limits: Arc<KeyLimits>) -> Self {
        let authenticated = key.is_some();
        let explicit_account = limits
            .account
            .as_deref()
            .and_then(|raw| validate_account_id(raw).ok());
        let account = match (explicit_account, key) {
            (Some(acct), Some(_)) => acct,
            (None, Some(k)) => fingerprint(k),
            (_, None) => "anonymous".into(),
        };
        Self::with_resolved_account(account, limits, authenticated)
    }

    pub(crate) fn with_resolved_account(
        account: String,
        limits: Arc<KeyLimits>,
        authenticated: bool,
    ) -> Self {
        let display_name = limits
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| account.clone());
        Self::with_resolved_account_and_display(account, display_name, limits, authenticated)
    }

    pub(crate) fn with_resolved_account_and_display(
        account: String,
        display_name: String,
        limits: Arc<KeyLimits>,
        authenticated: bool,
    ) -> Self {
        Self {
            account: account.into(),
            display_name: display_name.into(),
            limits,
            authenticated,
        }
    }
}

impl Caller {
    pub(crate) fn restored(
        account: &str,
        authenticated: Option<bool>,
        limits: Arc<KeyLimits>,
    ) -> Self {
        let display_name = limits
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(account);
        Self {
            account: account.into(),
            display_name: display_name.into(),
            limits,
            authenticated: authenticated.unwrap_or_else(|| {
                account != "anonymous" && !crate::types::ScrapeOptions::is_anonymous_owner(account)
            }),
        }
    }

    /// Server-trusted tenant namespace isolating DataGraph/ChangeTracking per authenticated key
    /// vs unauthenticated HTTP requests (`__http_anon__`).
    pub(crate) fn tenant_owner(&self) -> String {
        if self.authenticated {
            format!("acct:{}", self.account)
        } else {
            crate::types::ScrapeOptions::ANONYMOUS_OWNER.to_string()
        }
    }
}

pub(crate) fn caller_tenant(caller: Option<&Caller>) -> String {
    caller.map_or_else(
        || crate::types::ScrapeOptions::ANONYMOUS_OWNER.to_string(),
        Caller::tenant_owner,
    )
}

/// Hash incoming HTTP identity/session headers (`Cookie`, `X-Session-Id`, `Authorization`)
/// for unauthenticated requests so concurrent anonymous HTTP users do not share change state.
pub(crate) fn client_session_for_headers(
    caller: Option<&Caller>,
    headers: &axum::http::HeaderMap,
) -> Option<String> {
    if caller.is_some_and(|c| c.authenticated) {
        return None;
    }
    const SESSION_HEADERS: [&str; 3] = ["cookie", "x-session-id", "authorization"]; // [策略] 参与匿名 HTTP 会话隔离的身份请求头数量
    let mut parts: Vec<(&str, &str)> = Vec::new();
    for name in SESSION_HEADERS {
        for value in headers.get_all(name) {
            if let Ok(s) = value.to_str()
                && !s.is_empty()
            {
                parts.push((name, s));
            }
        }
    }
    if parts.is_empty() {
        return None;
    }
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    ctx.update(b"pagesift:http_session:v1");
    for (k, v) in parts {
        ctx.update(&(k.len() as u64).to_le_bytes());
        ctx.update(k.as_bytes());
        ctx.update(&(v.len() as u64).to_le_bytes());
        ctx.update(v.as_bytes());
    }
    let digest = ctx.finish();
    const SESSION_HASH_BYTES: usize = 16; // [策略] 匿名 HTTP 会话指纹字节长度
    let hex: String = digest.as_ref()[..SESSION_HASH_BYTES]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Some(hex)
}

pub(crate) fn caller_graph_tenant(
    caller: Option<&Caller>,
    headers: &axum::http::HeaderMap,
) -> String {
    if let Some(c) = caller
        && c.authenticated
    {
        return c.tenant_owner();
    }
    if let Some(sess) = client_session_for_headers(caller, headers) {
        return format!(
            "{}{sess}",
            crate::types::ScrapeOptions::ANONYMOUS_OWNER_PREFIX
        );
    }
    crate::types::ScrapeOptions::ANONYMOUS_OWNER.to_string()
}

/// Bind server-trusted tenant and anonymous HTTP session scope onto `ScrapeOptions`.
pub(crate) fn apply_http_caller_scope(
    opts: &mut crate::types::ScrapeOptions,
    caller: Option<&Caller>,
    headers: Option<&axum::http::HeaderMap>,
) {
    opts.owner = Some(caller_tenant(caller));
    if let Some(hdrs) = headers {
        opts.client_session = client_session_for_headers(caller, hdrs);
    }
    opts.owner = opts.effective_graph_tenant();
}

/// key 的密码学指纹（带域分隔的 SHA-256 截断 128 位），稳定、抗碰撞且绝不泄露明文 key。
pub(crate) fn fingerprint(key: &str) -> String {
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    ctx.update(b"pagesift:api_key:v2:");
    ctx.update(key.as_bytes());
    let digest = ctx.finish();
    const KEY_HASH_BYTES: usize = 16; // [策略] 128 位指纹（32 位 hex）兼顾抗碰撞与键长简洁
    let mut out = String::with_capacity(4 + KEY_HASH_BYTES * 2);
    out.push_str("key-");
    for b in &digest.as_ref()[..KEY_HASH_BYTES] {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// 1.6.0 及更早版本的 64 位 FNV-1a key 指纹（`key-<16hex>`），仅用于启动时识别并兼容已落盘的旧租户数据。
pub(crate) fn legacy_fnv1a_fingerprint(key: &str) -> String {
    // [协议] FNV-1a 64 位偏移基数（与 1.6.0 历史落盘键一致）
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h ^= u64::from(b);
        // [协议] FNV-1a 64 位素数
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
    /// 已结算、真正消费掉的页数；写盘只写这个值。
    settled: u64,
    /// 已预留、还没结算的页数。只在内存里，进程重启即消失，绝不写盘。
    held: u64,
}

impl Usage {
    fn used(&self) -> u64 {
        self.settled + self.held
    }
}

#[path = "limits_settle.rs"]
mod settle;

use settle::PendingLedgerClose;
pub(crate) use settle::{PageHold, QuotaBudget, SettleOutcome};

/// [策略] 挂起待核销账本冻结队列上限，防止持久不可写期间内存无限增长。
const MAX_PENDING_LEDGER_CLOSES: usize = 4096;

#[derive(Default)]
pub(crate) struct Limiter {
    buckets: Mutex<HashMap<Arc<str>, Bucket>>,
    usage: Mutex<HashMap<Arc<str>, Usage>>,
    store: Option<Store>,
    ledger: Option<Arc<pagesift_cluster::Ledger>>,
    /// 每个账户一把写盘锁：同一账户的配额快照按顺序落盘，旧的快照不会覆盖新的。
    write_locks: Mutex<HashMap<Arc<str>, Arc<tokio::sync::Mutex<()>>>>,
    /// 账本结算/释放失败后的幂等核销队列（热路径由 `has_pending_ledger` 原子哨兵短路，零额外锁）。
    pending_ledger: Mutex<VecDeque<PendingLedgerClose>>,
    reconcile_lock: Mutex<()>,
    has_pending_ledger: AtomicBool,
    /// 单机模式下写盘失败、尚未落盘最新 `settled` 值的账户集合（记录所属 UTC 天，支持跨天清理与机会式重刷）。
    dirty_local: Mutex<HashMap<Arc<str>, u64>>,
    has_dirty_local: AtomicBool,
    dirty_flush_in_flight: AtomicBool,
    metrics: Option<Arc<Metrics>>,
    settlement_failures: AtomicU64,
    settlement_uncertain: AtomicU64,
    settlement_dropped: AtomicU64,
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

pub(crate) fn usage_key(account: &str) -> String {
    format!("quota/{account}")
}

impl Limiter {
    pub(crate) fn new(store: Option<Store>) -> Self {
        Self {
            store,
            ..Default::default()
        }
    }

    pub(crate) fn with_metrics(mut self, metrics: Arc<Metrics>) -> Self {
        self.metrics = Some(metrics);
        self
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

    fn note_failure(&self, uncertain: bool) {
        self.settlement_failures.fetch_add(1, Ordering::Relaxed);
        if let Some(m) = &self.metrics {
            m.quota_settlement_failures.fetch_add(1, Ordering::Relaxed);
        }
        if uncertain {
            self.settlement_uncertain.fetch_add(1, Ordering::Relaxed);
            if let Some(m) = &self.metrics {
                m.quota_settlement_uncertain.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn note_dropped(&self) {
        self.settlement_dropped.fetch_add(1, Ordering::Relaxed);
        if let Some(m) = &self.metrics {
            m.quota_settlement_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn enqueue_ledger_close(&self, hold: pagesift_cluster::Hold, settle: Option<u64>) -> bool {
        if hold.id == 0 {
            return true;
        }
        if self.ledger.as_ref().is_some_and(|l| l.is_follower()) {
            self.note_dropped();
            return false;
        }
        let mut q = self
            .pending_ledger
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if q.iter().any(|p| p.hold.id == hold.id) {
            return true;
        }
        if q.len() >= MAX_PENDING_LEDGER_CLOSES {
            self.note_dropped();
            tracing::error!(
                account = %hold.account,
                hold_id = hold.id,
                "pending quota ledger close queue full; failed settlement could not be queued"
            );
            return false;
        }
        q.push_back(PendingLedgerClose { hold, settle });
        self.has_pending_ledger.store(true, Ordering::Release);
        true
    }

    /// 对之前因短暂租约过期或存储故障而挂起的账本冻结执行一轮非阻塞单飞幂等核销；
    /// 始终以队头 peek-then-pop 维持严格 FIFO、严守队列上限且不误清 `has_pending_ledger`，
    /// 遇到再次失败立即停止本轮（绝不热循环无限重试），同一 `hold.id` 在账本侧严格幂等。
    pub(crate) fn reconcile_ledger(&self) {
        if !self.has_pending_ledger.load(Ordering::Acquire) {
            return;
        }
        let Some(ledger) = &self.ledger else {
            return;
        };
        if ledger.is_follower() {
            return;
        }
        let Ok(_singleflight) = self.reconcile_lock.try_lock() else {
            return;
        };
        loop {
            let front = {
                let q = self
                    .pending_ledger
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if q.is_empty() {
                    self.has_pending_ledger.store(false, Ordering::Release);
                    return;
                }
                q.front().cloned()
            };
            let Some(item) = front else {
                return;
            };
            // 账本中该冻结已经关闭（如上次模糊写实际已落盘或跨天已清理），直接弹出。
            let closed = if !ledger.is_hold_open(&item.hold) {
                true
            } else {
                let r = match item.settle {
                    Some(actual) => ledger.settle(&item.hold, actual),
                    None => ledger.release(&item.hold),
                };
                match r {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::debug!(
                            account = %item.hold.account,
                            hold_id = item.hold.id,
                            error = %e,
                            "pending quota ledger reconciliation deferred"
                        );
                        false
                    }
                }
            };
            if !closed {
                return;
            }
            let mut q = self
                .pending_ledger
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if q.front().is_some_and(|p| p.hold.id == item.hold.id) {
                q.pop_front();
            }
            if q.is_empty() {
                self.has_pending_ledger.store(false, Ordering::Release);
                return;
            }
        }
    }

    fn mark_dirty_local(&self, account: Arc<str>, day: u64) {
        let mut dirty = self.dirty_local.lock().unwrap_or_else(|e| e.into_inner());
        dirty.insert(account, day);
        self.has_dirty_local.store(true, Ordering::Release);
    }

    fn clear_dirty_local(&self, account: &Arc<str>, flushed_day: u64) {
        let mut dirty = self.dirty_local.lock().unwrap_or_else(|e| e.into_inner());
        if dirty.get(account).is_some_and(|&d| d <= flushed_day) {
            dirty.remove(account);
        }
        self.has_dirty_local
            .store(!dirty.is_empty(), Ordering::Release);
    }

    fn prune_expired_dirty_local(&self) {
        if !self.has_dirty_local.load(Ordering::Acquire) {
            return;
        }
        let today = today();
        let mut dirty = self.dirty_local.lock().unwrap_or_else(|e| e.into_inner());
        dirty.retain(|_, day| *day >= today);
        self.has_dirty_local
            .store(!dirty.is_empty(), Ordering::Release);
    }

    /// 机会式后台重刷单机模式下此前写盘失败的当天账户配额（单飞保护，跨天记录自动清理）。
    fn maybe_flush_dirty_local(self: &Arc<Self>) {
        if !self.has_dirty_local.load(Ordering::Acquire) || self.store.is_none() {
            return;
        }
        self.prune_expired_dirty_local();
        if !self.has_dirty_local.load(Ordering::Acquire) {
            return;
        }
        if self
            .dirty_flush_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let accounts: Vec<Arc<str>> = {
            let dirty = self.dirty_local.lock().unwrap_or_else(|e| e.into_inner());
            dirty.keys().cloned().collect()
        };
        if accounts.is_empty() {
            self.dirty_flush_in_flight.store(false, Ordering::Release);
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            self.dirty_flush_in_flight.store(false, Ordering::Release);
            return;
        };
        let limiter = self.clone();
        handle.spawn(async move {
            struct Reset(Arc<Limiter>);
            impl Drop for Reset {
                fn drop(&mut self) {
                    self.0.dirty_flush_in_flight.store(false, Ordering::Release);
                }
            }
            let _reset = Reset(limiter.clone());
            let today = today();
            for account in accounts {
                let lock = {
                    let mut locks = limiter
                        .write_locks
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    locks
                        .entry(account.clone())
                        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                        .clone()
                };
                let _guard = lock.lock().await;
                let snapshot = {
                    let map = limiter.usage.lock().unwrap_or_else(|e| e.into_inner());
                    map.get(&account).map(|u| (u.day, u.settled))
                };
                match snapshot {
                    Some((day, settled)) if day >= today => {
                        let outcome = limiter.write_usage(&account, day, settled).await;
                        if !matches!(outcome, SettleOutcome::Confirmed) {
                            break;
                        }
                    }
                    _ => limiter.clear_dirty_local(&account, today),
                }
            }
        });
    }

    /// 供 `/metrics` 与测试观测当前尚未落盘确认的结算/释放条目总数。
    pub(crate) fn pending_settlement_count(&self) -> usize {
        self.prune_expired_dirty_local();
        let ledger_n = if self.has_pending_ledger.load(Ordering::Acquire) {
            self.pending_ledger
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len()
        } else {
            0
        };
        let local_n = if self.has_dirty_local.load(Ordering::Acquire) {
            self.dirty_local
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len()
        } else {
            0
        };
        ledger_n + local_n
    }

    /// 供测试和诊断查看累计结算故障计数 `(failures, uncertain, dropped)`。
    #[cfg(test)]
    pub(crate) fn settlement_counters(&self) -> (u64, u64, u64) {
        (
            self.settlement_failures.load(Ordering::Relaxed),
            self.settlement_uncertain.load(Ordering::Relaxed),
            self.settlement_dropped.load(Ordering::Relaxed),
        )
    }

    /// 每分钟请求数：容量等于每分钟请求数的令牌桶，匀速补充，允许短时突发。
    pub(crate) fn check_rate(&self, c: &Caller) -> Result<(), ApiError> {
        let Some(rpm) = c.limits.requests_per_minute else {
            return Ok(());
        };
        if rpm == 0 {
            return Err(rate_error(0, 60));
        }
        if let Some(l) = &self.ledger {
            self.reconcile_ledger();
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
        let cap = f64::from(rpm);
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
                    settled: n.parse().ok()?,
                    held: 0,
                })
            })
            .filter(|u| u.day == day);
        stored.unwrap_or(Usage {
            day,
            settled: 0,
            held: 0,
        })
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
            self.reconcile_ledger();
            return l.balance(&c.account);
        }
        Some(max.saturating_sub(self.usage(&c.account).used()))
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
            self.reconcile_ledger();
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
        self.maybe_flush_dirty_local();
        let max = c.limits.pages_per_day.unwrap_or(0);
        {
            let mut map = self.usage.lock().unwrap_or_else(|e| e.into_inner());
            let u = self.usage_entry(&mut map, &c.account, day);
            if u.used().saturating_add(amount) > max {
                return Err(quota_error(c));
            }
            u.held += amount;
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
            u.held = u.held.saturating_sub(amount);
        }
    }

    /// 同步把 `amount` 从在途预留转入已结算（不跨越 `.await` 取消点，杜绝取消导致 `held` 永久泄漏）。
    fn settle_in_memory(&self, account: &Arc<str>, day: u64, amount: u64) -> bool {
        let mut map = self.usage.lock().unwrap_or_else(|e| e.into_inner());
        let Some(u) = map.get_mut(account) else {
            return false;
        };
        // 跨天：旧日的额度已经作废，不再记到新的一天
        if u.day != day {
            return false;
        }
        u.held = u.held.saturating_sub(amount);
        u.settled = u.settled.saturating_add(amount);
        true
    }

    /// 串行持久化账户当前最新的已结算值；在独立任务中持锁落盘并等待完成，
    /// 即使调用方在 `.await` 期间被取消也不会提前释放写锁或丢失已结算记录。
    async fn persist_usage(self: &Arc<Self>, account: &Arc<str>) -> SettleOutcome {
        if self.store.is_none() {
            return SettleOutcome::Confirmed;
        }
        let limiter = self.clone();
        let acct_for_err = account.clone();
        let account = account.clone();
        let handle = tokio::spawn(async move {
            let lock = {
                let mut locks = limiter
                    .write_locks
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                locks
                    .entry(account.clone())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                    .clone()
            };
            let _guard = lock.lock().await;
            let snapshot = {
                let map = limiter.usage.lock().unwrap_or_else(|e| e.into_inner());
                map.get(&account).map(|u| (u.day, u.settled))
            };
            if let Some((day, settled)) = snapshot {
                limiter.write_usage(&account, day, settled).await
            } else {
                SettleOutcome::Confirmed
            }
        });
        match handle.await {
            Ok(outcome) => {
                self.maybe_flush_dirty_local();
                outcome
            }
            Err(e) => {
                self.mark_dirty_local(acct_for_err, today());
                self.note_failure(true);
                SettleOutcome::Uncertain {
                    message: format!("quota persistence task aborted: {e}"),
                    recoverable: true,
                }
            }
        }
    }

    async fn write_usage(&self, account: &Arc<str>, day: u64, settled: u64) -> SettleOutcome {
        let Some(store) = &self.store else {
            return SettleOutcome::Confirmed;
        };
        let key = usage_key(account);
        let value = format!("{day} {settled}");
        let r = store
            .blocking(move |s| s.tasks().kv_put(key, value.as_bytes()))
            .await;
        match r {
            Ok(()) => {
                self.clear_dirty_local(account, day);
                SettleOutcome::Confirmed
            }
            Err(e) => {
                self.mark_dirty_local(account.clone(), day);
                self.note_failure(true);
                tracing::error!(account = %account, error = %e, "saving page quota usage failed");
                SettleOutcome::Uncertain {
                    message: format!("saving page quota usage failed: {e}"),
                    recoverable: true,
                }
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
#[path = "limits_tests.rs"]
mod tests;
