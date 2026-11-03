//! 会话快照的导出与恢复（含秘密的显式导出，Debug 不显示内容）。

use std::fmt;

use serde::{Deserialize, Serialize};
use url::Url;

use super::*;

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
