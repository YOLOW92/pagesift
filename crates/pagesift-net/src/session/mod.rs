//! Explicit, bounded HTTP sessions. Cookies are restricted to one exact origin
//! (scheme/host/port), stricter than browser Domain/SameSite behavior.
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::Secret;
use reqwest::header::{HeaderMap, HeaderValue, SET_COOKIE};
use serde::Serialize;
use url::Url;

mod cookie;
mod lease;
mod snapshot;
#[cfg(test)]
mod tests;

use cookie::*;
pub(crate) use lease::SessionUse;
pub use snapshot::SessionSnapshot;

#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Reject overlapping requests on the same identity.
    pub exclusive: bool,
    /// A cancelled request does not count as an identity failure.
    pub neutral_on_cancel: bool,
    pub max_age: Duration,
    /// 一个身份最多用多少次（轮换预算，用户策略）。
    pub max_uses: u64,
    /// 连续失败多少次判定会话失效的**最少**次数。会话自身历史失败率高时，按序贯概率比检验
    /// 需要更多次连续失败才判失效（序贯概率比检验），不会少于这个值。
    pub max_consecutive_errors: u32,
    /// 每个会话最多保存的 cookie 数（安全策略：限制对方塞给我们的状态量）。
    pub max_cookies: usize,
    pub max_cookie_bytes: usize,
    pub max_total_bytes: usize,
    pub max_header_bytes: usize,
    pub max_snapshot_bytes: usize,
}
impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            exclusive: false,
            neutral_on_cancel: false,
            max_age: Duration::from_secs(24 * 3600),
            // [策略] 用户策略：身份轮换预算（0.81 的现值）。与失效概率无关，不由数据推出
            max_uses: 10_000,
            // [策略] 先验包 v0：0.81 的现值，SPRT 判定失效的最少连续失败数
            max_consecutive_errors: 3,
            // [策略] 安全策略：0.81 的现值
            max_cookies: 64,
            max_cookie_bytes: 4096,
            max_total_bytes: 64 * 1024,
            max_header_bytes: 8192,
            max_snapshot_bytes: 256 * 1024,
        }
    }
}
impl SessionConfig {
    fn validate(&self) -> Result<(), SessionError> {
        if self.max_age.as_secs() == 0
            || self.max_age.as_secs() > 400 * 24 * 3600
            || self.max_uses == 0
            || self.max_consecutive_errors == 0
            || !(1..=1024).contains(&self.max_cookies)
            || !(1..=4096).contains(&self.max_cookie_bytes)
            || !(1..=1024 * 1024).contains(&self.max_total_bytes)
            || !(1..=65536).contains(&self.max_header_bytes)
            || !(1024..=4 * 1024 * 1024).contains(&self.max_snapshot_bytes)
        {
            return Err(SessionError::Config);
        }
        Ok(())
    }
}
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("invalid session limits")]
    Config,
    #[error("session requires an HTTP(S) origin without URL credentials")]
    Origin,
    #[error("session belongs to a different origin")]
    OriginMismatch,
    #[error("session has expired")]
    Expired,
    #[error("session is retired")]
    Retired,
    #[error("session is already in use")]
    Busy,
    #[error("session use budget exhausted")]
    Exhausted,
    #[error("session cookie header exceeds its configured limit")]
    HeaderLimit,
    #[error("invalid or oversized session snapshot")]
    Snapshot,
}
#[derive(Clone, Debug, Serialize)]
pub struct SessionStats {
    pub origin: String,
    pub cookies: usize,
    pub uses: u64,
    pub consecutive_errors: u32,
    pub retired: bool,
    pub expires_at: u64,
}

/// Explicit, secret-bearing cookie export for a browser in the same origin.
/// Values must never be logged or exposed in API responses.
#[derive(Clone)]
pub struct BrowserCookie {
    pub name: String,
    pub value: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub expires_at: Option<u64>,
}

impl fmt::Debug for BrowserCookie {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BrowserCookie([REDACTED])")
    }
}

impl Drop for BrowserCookie {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.value);
    }
}
struct State {
    in_flight: bool,
    cookies: Vec<Cookie>,
    created_at: u64,
    expires_at: u64,
    uses: u64,
    errors: u32,
    /// 已结束的使用里成功和失败的总数（失效判定的基线失败率），不写进快照。
    finished_ok: u64,
    finished_failed: u64,
    retired: bool,
    next_order: u64,
}
impl State {
    fn active(&self, now: u64, cfg: &SessionConfig) -> Result<(), SessionError> {
        if self.retired {
            Err(SessionError::Retired)
        } else if now >= self.expires_at {
            Err(SessionError::Expired)
        } else if self.uses >= cfg.max_uses {
            Err(SessionError::Exhausted)
        } else {
            Ok(())
        }
    }
    fn prune(&mut self, now: u64) {
        self.cookies
            .retain(|c| c.expires_at.is_none_or(|t| t > now));
    }
}
/// No implicit global jar: share an Arc only between requests of one identity.
pub struct CookieSession {
    origin: String,
    host: String,
    config: SessionConfig,
    state: Mutex<State>,
}
impl fmt::Debug for CookieSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CookieSession")
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}
impl CookieSession {
    /// Export only the cookies belonging to this exact origin.
    pub fn browser_cookies(&self, origin: &Url) -> Result<Vec<BrowserCookie>, SessionError> {
        if origin_key(origin)? != self.origin {
            return Err(SessionError::OriginMismatch);
        }
        let mut state = self.lock();
        if state.retired {
            return Err(SessionError::Retired);
        }
        if now() >= state.expires_at {
            return Err(SessionError::Expired);
        }
        state.prune(now());
        Ok(state
            .cookies
            .iter()
            .map(|c| BrowserCookie {
                name: c.name.clone(),
                value: c.value.expose().to_owned(),
                path: c.path.clone(),
                secure: c.secure,
                http_only: c.http_only,
                expires_at: c.expires_at,
            })
            .collect())
    }
    pub fn new(origin: &Url, config: SessionConfig) -> Result<Self, SessionError> {
        Self::new_at(origin, config, now())
    }
    fn new_at(origin: &Url, config: SessionConfig, time: u64) -> Result<Self, SessionError> {
        config.validate()?;
        let key = origin_key(origin)?;
        let expires_at = time.saturating_add(config.max_age.as_secs());
        Ok(Self {
            origin: key,
            host: origin.host_str().unwrap_or_default().into(),
            config,
            state: Mutex::new(State {
                in_flight: false,
                cookies: Vec::new(),
                created_at: time,
                expires_at,
                uses: 0,
                errors: 0,
                finished_ok: 0,
                finished_failed: 0,
                retired: false,
                next_order: 0,
            }),
        })
    }
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn origin(&self) -> &str {
        &self.origin
    }
    pub fn retire(&self) {
        self.lock().retired = true;
    }
    pub fn stats(&self) -> SessionStats {
        let mut s = self.lock();
        s.prune(now());
        SessionStats {
            origin: self.origin.clone(),
            cookies: s.cookies.len(),
            uses: s.uses,
            consecutive_errors: s.errors,
            retired: s.retired,
            expires_at: s.expires_at,
        }
    }
    pub(crate) fn begin(self: &Arc<Self>, url: &Url) -> Result<SessionUse, SessionError> {
        if origin_key(url)? != self.origin {
            return Err(SessionError::OriginMismatch);
        }
        let mut s = self.lock();
        s.active(now(), &self.config)?;
        if self.config.exclusive && s.in_flight {
            return Err(SessionError::Busy);
        }
        if self.config.exclusive {
            s.in_flight = true;
        }
        s.uses += 1;
        Ok(SessionUse {
            session: self.clone(),
            finished: false,
        })
    }
    /// Manually seed a cookie using Set-Cookie syntax; rejected input returns false.
    pub fn set_cookie(&self, url: &Url, value: &str) -> bool {
        self.store_at(url, value, now())
    }
    pub(crate) fn receive(&self, url: &Url, headers: &HeaderMap) {
        for value in headers.get_all(SET_COOKIE) {
            if let Ok(value) = value.to_str() {
                self.set_cookie(url, value);
            }
        }
    }
    fn store_at(&self, url: &Url, value: &str, time: u64) -> bool {
        if origin_key(url).ok().as_deref() != Some(&self.origin)
            || value.len() > self.config.max_cookie_bytes
        {
            return false;
        }
        let mut s = self.lock();
        if s.retired || time >= s.expires_at {
            return false;
        }
        s.prune(time);
        let Some(mut cookie) = parse_cookie(url, &self.host, value, time, s.expires_at) else {
            return false;
        };
        if cookie.name.len() + cookie.value.expose().len() + cookie.path.len()
            > self.config.max_cookie_bytes
        {
            return false;
        }
        let old = s
            .cookies
            .iter()
            .position(|c| c.name == cookie.name && c.path == cookie.path);
        if cookie.expires_at.is_some_and(|t| t <= time) {
            if let Some(i) = old {
                s.cookies.remove(i);
            }
            return true;
        }
        let old_size = old.map_or(0, |i| s.cookies[i].size());
        if old.is_none() && s.cookies.len() >= self.config.max_cookies
            || s.cookies.iter().map(Cookie::size).sum::<usize>() - old_size + cookie.size()
                > self.config.max_total_bytes
        {
            return false;
        }
        if let Some(i) = old {
            cookie.order = s.cookies[i].order;
            s.cookies[i] = cookie;
        } else {
            let Some(next) = s.next_order.checked_add(1) else {
                return false;
            };
            cookie.order = s.next_order;
            s.next_order = next;
            s.cookies.push(cookie);
        }
        true
    }
    pub(crate) fn cookie_header(&self, url: &Url) -> Result<Option<HeaderValue>, SessionError> {
        self.header_at(url, now())
    }
    fn header_at(&self, url: &Url, time: u64) -> Result<Option<HeaderValue>, SessionError> {
        if origin_key(url)? != self.origin {
            return Ok(None);
        }
        let mut s = self.lock();
        // The use counter was reserved by begin(); the last admitted use may finish.
        if s.retired {
            return Err(SessionError::Retired);
        }
        if time >= s.expires_at {
            return Err(SessionError::Expired);
        }
        s.prune(time);
        let mut applicable: Vec<_> = s
            .cookies
            .iter()
            .filter(|c| (!c.secure || url.scheme() == "https") && path_matches(url.path(), &c.path))
            .collect();
        applicable.sort_by_key(|c| (std::cmp::Reverse(c.path.len()), c.order));
        if applicable.is_empty() {
            return Ok(None);
        }
        let size: usize = applicable
            .iter()
            .map(|c| c.name.len() + 1 + c.value.expose().len() + 2)
            .sum();
        if size.saturating_sub(2) > self.config.max_header_bytes {
            return Err(SessionError::HeaderLimit);
        }
        let value = applicable
            .iter()
            .map(|c| format!("{}={}", c.name, c.value.expose()))
            .collect::<Vec<_>>()
            .join("; ");
        let mut header = HeaderValue::from_str(&value).map_err(|_| SessionError::HeaderLimit)?;
        header.set_sensitive(true);
        Ok(Some(header))
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn origin_key(url: &Url) -> Result<String, SessionError> {
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(SessionError::Origin);
    }
    Ok(url.origin().ascii_serialization())
}
