//! Explicit, bounded HTTP sessions. Cookies are restricted to one exact origin
//! (scheme/host/port), stricter than browser Domain/SameSite behavior.
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::Secret;
use reqwest::header::{HeaderMap, HeaderValue, SET_COOKIE};
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub max_age: Duration,
    pub max_uses: u64,
    pub max_consecutive_errors: u32,
    pub max_cookies: usize,
    pub max_cookie_bytes: usize,
    pub max_total_bytes: usize,
    pub max_header_bytes: usize,
    pub max_snapshot_bytes: usize,
}
impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_age: Duration::from_secs(24 * 3600),
            max_uses: 10_000,
            max_consecutive_errors: 3,
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
struct Cookie {
    name: String,
    value: Secret,
    path: String,
    secure: bool,
    http_only: bool,
    expires_at: Option<u64>,
    order: u64,
}
impl Cookie {
    fn size(&self) -> usize {
        self.name.len() + self.value.expose().len() + self.path.len() + 64
    }
}
struct State {
    cookies: Vec<Cookie>,
    created_at: u64,
    expires_at: u64,
    uses: u64,
    errors: u32,
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
/// Explicit secret-bearing export. Debug never includes its serialized cookies.
pub struct SessionSnapshot(Vec<u8>);
impl SessionSnapshot {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}
impl fmt::Debug for SessionSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionSnapshot([REDACTED])")
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    version: u32,
    origin: String,
    created_at: u64,
    expires_at: u64,
    uses: u64,
    errors: u32,
    retired: bool,
    cookies: Vec<SavedCookie>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedCookie {
    name: String,
    value: String,
    path: String,
    secure: bool,
    http_only: bool,
    expires_at: Option<u64>,
    order: u64,
}
impl CookieSession {
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
                cookies: Vec::new(),
                created_at: time,
                expires_at,
                uses: 0,
                errors: 0,
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
    pub fn snapshot(&self) -> Result<SessionSnapshot, SessionError> {
        let mut s = self.lock();
        s.prune(now());
        let saved = Saved {
            version: 1,
            origin: self.origin.clone(),
            created_at: s.created_at,
            expires_at: s.expires_at,
            uses: s.uses,
            errors: s.errors,
            retired: s.retired,
            cookies: s
                .cookies
                .iter()
                .map(|c| SavedCookie {
                    name: c.name.clone(),
                    value: c.value.expose().into(),
                    path: c.path.clone(),
                    secure: c.secure,
                    http_only: c.http_only,
                    expires_at: c.expires_at,
                    order: c.order,
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&saved).map_err(|_| SessionError::Snapshot)?;
        if bytes.len() > self.config.max_snapshot_bytes {
            return Err(SessionError::Snapshot);
        }
        Ok(SessionSnapshot(bytes))
    }
    pub fn restore(
        origin: &Url,
        config: SessionConfig,
        bytes: &[u8],
    ) -> Result<Self, SessionError> {
        config.validate()?;
        if bytes.len() > config.max_snapshot_bytes {
            return Err(SessionError::Snapshot);
        }
        let saved: Saved = serde_json::from_slice(bytes).map_err(|_| SessionError::Snapshot)?;
        if saved.origin != origin_key(origin)? {
            return Err(SessionError::OriginMismatch);
        }
        let time = now();
        if saved.version != 1
            || saved.created_at > time
            || saved.expires_at < saved.created_at
            || saved.expires_at - saved.created_at > config.max_age.as_secs()
            || saved.cookies.len() > config.max_cookies
            || saved.uses > config.max_uses
        {
            return Err(SessionError::Snapshot);
        }
        let session = Self::new_at(origin, config, time)?;
        let mut s = session.lock();
        s.created_at = saved.created_at;
        s.expires_at = saved.expires_at;
        s.uses = saved.uses;
        s.errors = saved.errors;
        s.retired = saved.retired || saved.errors >= session.config.max_consecutive_errors;
        let mut size = 0;
        for c in saved.cookies {
            if !valid_name(&c.name)
                || !valid_value(&c.value)
                || !valid_path(&c.path)
                || c.name.len() + c.value.len() + c.path.len() > session.config.max_cookie_bytes
                || c.secure && origin.scheme() != "https"
                || c.name.starts_with("__Secure-") && !c.secure
                || c.name.starts_with("__Host-") && (!c.secure || c.path != "/")
                || c.expires_at.is_some_and(|t| t > s.expires_at)
                || c.order == u64::MAX
                || s.cookies
                    .iter()
                    .any(|old| old.name == c.name && old.path == c.path)
            {
                return Err(SessionError::Snapshot);
            }
            let cookie = Cookie {
                name: c.name,
                value: Secret::new(c.value),
                path: c.path,
                secure: c.secure,
                http_only: c.http_only,
                expires_at: c.expires_at,
                order: c.order,
            };
            size += cookie.size();
            if size > session.config.max_total_bytes {
                return Err(SessionError::Snapshot);
            }
            s.next_order = s.next_order.max(cookie.order + 1);
            s.cookies.push(cookie);
        }
        s.prune(time);
        drop(s);
        Ok(session)
    }
}
pub(crate) struct SessionUse {
    session: Arc<CookieSession>,
    finished: bool,
}
impl SessionUse {
    pub(crate) fn finish(mut self, status: u16) {
        self.record(!matches!(status, 401 | 403 | 429 | 500..=599));
    }
    fn record(&mut self, success: bool) {
        if self.finished {
            return;
        }
        self.finished = true;
        let mut s = self.session.lock();
        if success {
            s.errors = 0;
        } else {
            s.errors = s.errors.saturating_add(1);
            if s.errors >= self.session.config.max_consecutive_errors {
                s.retired = true;
            }
        }
    }
}
impl Drop for SessionUse {
    fn drop(&mut self) {
        self.record(false);
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
fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}
fn valid_value(s: &str) -> bool {
    s.bytes()
        .all(|b| matches!(b,0x21|0x23..=0x2b|0x2d..=0x3a|0x3c..=0x5b|0x5d..=0x7e))
}
fn valid_path(s: &str) -> bool {
    s.starts_with('/') && s.bytes().all(|b| (0x20..=0x7e).contains(&b) && b != b';')
}
fn path_matches(request: &str, cookie: &str) -> bool {
    request == cookie
        || request.starts_with(cookie)
            && (cookie.ends_with('/') || request.as_bytes().get(cookie.len()) == Some(&b'/'))
}
fn parse_cookie(url: &Url, host: &str, raw: &str, time: u64, session_end: u64) -> Option<Cookie> {
    if !raw.is_ascii() || raw.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return None;
    }
    let mut parts = raw.split(';');
    let (name, value) = parts.next()?.trim().split_once('=')?;
    let (name, value) = (name.trim(), value.trim());
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    if !valid_name(name) || !valid_value(value) {
        return None;
    }
    let default_path = url
        .path()
        .rsplit_once('/')
        .map_or("/", |(p, _)| if p.is_empty() { "/" } else { p });
    let mut path = default_path;
    let mut explicit_root_path = false;
    let (mut secure, mut http_only, mut domain) = (false, false, false);
    let (mut expires, mut max_age) = (None, None);
    for attr in parts {
        let (key, value) = attr.trim().split_once('=').unwrap_or((attr.trim(), ""));
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "domain" => {
                domain = true;
                if !value.trim_start_matches('.').eq_ignore_ascii_case(host) {
                    return None;
                }
            }
            "path" => {
                explicit_root_path = value == "/";
                path = if valid_path(value) {
                    value
                } else {
                    default_path
                };
            }
            "secure" => secure = true,
            "httponly" => http_only = true,
            "max-age" => {
                let digits = value.strip_prefix('-').unwrap_or(value);
                if !digits.is_empty()
                    && digits.bytes().all(|b| b.is_ascii_digit())
                    && let Ok(v) = value.parse::<i64>()
                {
                    max_age = Some(v);
                }
            }
            "expires" => {
                if let Ok(t) = httpdate::parse_http_date(value) {
                    expires = Some(t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs());
                }
            }
            _ => (),
        }
    }
    if secure && url.scheme() != "https"
        || name.starts_with("__Secure-") && (!secure || url.scheme() != "https")
        || name.starts_with("__Host-")
            && (!secure || url.scheme() != "https" || domain || path != "/" || !explicit_root_path)
    {
        return None;
    }
    let expires_at = max_age
        .map(|v| {
            if v <= 0 {
                time
            } else {
                time.saturating_add(v as u64)
            }
        })
        .or(expires)
        .map(|t| t.min(session_end));
    Some(Cookie {
        name: name.into(),
        value: Secret::new(value),
        path: path.into(),
        secure,
        http_only,
        expires_at,
        order: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn url(p: &str) -> Url {
        Url::parse(&format!("https://one.test{p}")).unwrap()
    }
    fn text(s: &CookieSession, p: &str) -> String {
        s.cookie_header(&url(p))
            .unwrap()
            .map(|v| v.to_str().unwrap().into())
            .unwrap_or_default()
    }
    #[test]
    fn path_defaults_order_replacement_and_deletion() {
        let s = CookieSession::new(&url("/a/login"), SessionConfig::default()).unwrap();
        assert!(s.set_cookie(&url("/a/login"), "id=base; Path=/"));
        assert!(s.set_cookie(&url("/a/login"), "id=specific; HttpOnly"));
        assert_eq!(text(&s, "/a/view"), "id=specific; id=base");
        assert_eq!(text(&s, "/apple"), "id=base");
        assert!(s.set_cookie(&url("/a/login"), "id=replaced; Path=/a"));
        assert_eq!(text(&s, "/a"), "id=replaced; id=base");
        assert!(s.set_cookie(
            &url("/a/login"),
            "id=gone; Path=/a; Max-Age=0; Expires=Wed, 09 Jun 2099 10:18:14 GMT"
        ));
        assert_eq!(text(&s, "/a/view"), "id=base");
    }
    #[test]
    fn secure_prefixes_and_exact_origin_reject_cross_scope() {
        let s = CookieSession::new(&url("/"), SessionConfig::default()).unwrap();
        assert!(s.set_cookie(&url("/"), "__Host-id=secret; Path=/; Secure; HttpOnly"));
        for v in [
            "__Host-no-path=bad; Secure",
            "__Host-domain=bad; Secure; Path=/; Domain=one.test",
            "__Secure-id=bad",
            "id=bad; Domain=test",
            "id=bad; Domain=other.test",
            "bad name=x",
            "id=bad\\value",
            "id=x\r\nInjected:y",
        ] {
            assert!(!s.set_cookie(&url("/"), v), "{v}");
        }
        for other in [
            "http://one.test/",
            "https://one.test:444/",
            "https://sub.one.test/",
            "https://other.test/",
        ] {
            let u = Url::parse(other).unwrap();
            assert!(s.cookie_header(&u).unwrap().is_none());
            assert!(!s.set_cookie(&u, "other=bad"));
        }
    }
    #[test]
    fn age_expiry_and_resource_limits() {
        let config = SessionConfig {
            max_cookies: 2,
            ..Default::default()
        };
        let s = CookieSession::new_at(&url("/"), config, 100).unwrap();
        assert!(s.store_at(&url("/"), "id=x; Max-Age=5", 100));
        assert!(s.header_at(&url("/"), 104).unwrap().is_some());
        assert!(s.header_at(&url("/"), 105).unwrap().is_none());
        assert!(s.store_at(&url("/"), "one=x", 105));
        assert!(s.store_at(&url("/"), "two=y", 105));
        assert!(!s.store_at(&url("/"), "three=z", 105));
        assert!(!s.store_at(&url("/"), &format!("big={}", "x".repeat(5000)), 105));
        assert!(s.store_at(&url("/"), "one=new", 105));
    }
    #[test]
    fn session_age_header_budget_and_invalid_last_path_are_bounded() {
        let s = CookieSession::new_at(
            &url("/a/login"),
            SessionConfig {
                max_age: Duration::from_secs(10),
                max_header_bytes: 3,
                ..Default::default()
            },
            100,
        )
        .unwrap();
        assert!(s.store_at(&url("/a/login"), "id=value; Path=/; Path=invalid", 100));
        assert!(s.header_at(&url("/b"), 101).unwrap().is_none());
        assert!(matches!(
            s.header_at(&url("/a/view"), 101),
            Err(SessionError::HeaderLimit)
        ));
        assert!(matches!(
            s.header_at(&url("/a/view"), 110),
            Err(SessionError::Expired)
        ));
    }
    #[test]
    fn snapshot_roundtrip_keeps_secrets_redacted_and_scope_fixed() {
        let origin = url("/");
        let s = CookieSession::new(&origin, SessionConfig::default()).unwrap();
        s.set_cookie(&origin, "id=VERY_SECRET_TOKEN; Secure; HttpOnly; Path=/");
        let snapshot = s.snapshot().unwrap();
        assert!(!format!("{s:?} {snapshot:?}").contains("VERY_SECRET"));
        let restored =
            CookieSession::restore(&origin, SessionConfig::default(), snapshot.as_bytes()).unwrap();
        assert_eq!(text(&restored, "/"), "id=VERY_SECRET_TOKEN");
        assert!(
            CookieSession::restore(
                &Url::parse("https://other.test").unwrap(),
                SessionConfig::default(),
                snapshot.as_bytes()
            )
            .is_err()
        );
        let future = String::from_utf8(snapshot.as_bytes().to_vec())
            .unwrap()
            .replace("\"version\":1", "\"version\":2");
        assert!(
            CookieSession::restore(&origin, SessionConfig::default(), future.as_bytes()).is_err()
        );
    }
    #[test]
    fn leases_count_errors_retirement_and_use_budget() {
        let config = SessionConfig {
            max_uses: 4,
            max_consecutive_errors: 2,
            ..Default::default()
        };
        let s = Arc::new(CookieSession::new(&url("/"), config).unwrap());
        drop(s.begin(&url("/")).unwrap());
        assert_eq!(s.stats().consecutive_errors, 1);
        s.begin(&url("/")).unwrap().finish(200);
        assert_eq!(s.stats().consecutive_errors, 0);
        s.begin(&url("/")).unwrap().finish(401);
        s.begin(&url("/")).unwrap().finish(403);
        assert!(s.stats().retired);
        assert!(s.begin(&url("/")).is_err());
        let s = Arc::new(
            CookieSession::new(
                &url("/"),
                SessionConfig {
                    max_uses: 1,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        s.begin(&url("/")).unwrap().finish(200);
        assert!(matches!(s.begin(&url("/")), Err(SessionError::Exhausted)));
    }
}
