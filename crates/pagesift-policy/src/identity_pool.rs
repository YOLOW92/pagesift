//! Origin- and owner-scoped identity leases. No cookie values enter diagnostics.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use pagesift_net::{CookieSession, FingerprintProfile, SessionConfig, SessionError, Url};

/// Identity sharing is restricted to this complete key. `owner` is supplied by
/// the caller and must identify the tenant, not merely the requested host.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct IdentityKey {
    pub owner: String,
    pub origin: String,
    pub egress: Option<String>,
    pub fingerprint: Option<FingerprintProfile>,
}

impl std::fmt::Debug for IdentityKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdentityKey")
            .field("owner", &self.owner)
            .field("origin", &self.origin)
            .field("egress", &self.egress.as_ref().map(|_| "[REDACTED]"))
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

struct Slot {
    id: u64,
    cookie: Arc<CookieSession>,
    leased: bool,
}

#[derive(Default)]
struct State {
    next_id: u64,
    slots: HashMap<IdentityKey, Vec<Slot>>,
}

struct Inner {
    state: Mutex<State>,
    config: SessionConfig,
    max_total: usize,
    max_per_key: usize,
}

#[derive(Clone)]
pub struct IdentityPool(Arc<Inner>);

/// A slot is exclusively held until this value is dropped, even on cancellation.
pub struct IdentityLease {
    pool: Arc<Inner>,
    key: IdentityKey,
    id: u64,
    cookie: Arc<CookieSession>,
    retire: bool,
}

impl IdentityPool {
    pub fn new(mut config: SessionConfig, max_total: usize, max_per_key: usize) -> Self {
        // The lease invariant must also hold if a caller retains a cloned Arc.
        config.exclusive = true;
        config.neutral_on_cancel = true;
        Self(Arc::new(Inner {
            state: Mutex::new(State::default()),
            config,
            max_total,
            max_per_key,
        }))
    }

    /// Returns `None` when all matching slots are in use or capacity is full.
    /// The given URL must exactly match the key's normalized origin.
    pub fn try_acquire(
        &self,
        key: IdentityKey,
        url: &Url,
    ) -> Result<Option<IdentityLease>, SessionError> {
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(SessionError::Origin);
        }
        if key.owner.is_empty() || url.origin().ascii_serialization() != key.origin {
            return Err(SessionError::OriginMismatch);
        }
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let max_uses = self.0.config.max_uses;
        state.slots.retain(|_, slots| {
            slots.retain(|s| {
                let stats = s.cookie.stats();
                s.leased || (!stats.retired && stats.expires_at > now && stats.uses < max_uses)
            });
            !slots.is_empty()
        });
        if let Some(slot) = state
            .slots
            .get_mut(&key)
            .and_then(|slots| slots.iter_mut().find(|s| !s.leased))
        {
            slot.leased = true;
            return Ok(Some(IdentityLease {
                pool: self.0.clone(),
                key,
                id: slot.id,
                cookie: slot.cookie.clone(),
                retire: false,
            }));
        }
        let count: usize = state.slots.values().map(Vec::len).sum();
        if count >= self.0.max_total
            || state.slots.get(&key).map_or(0, Vec::len) >= self.0.max_per_key
        {
            return Ok(None);
        }
        let cookie = Arc::new(CookieSession::new(url, self.0.config.clone())?);
        let id = state.next_id;
        state.next_id = state
            .next_id
            .checked_add(1)
            .ok_or(SessionError::Exhausted)?;
        state.slots.entry(key.clone()).or_default().push(Slot {
            id,
            cookie: cookie.clone(),
            leased: true,
        });
        Ok(Some(IdentityLease {
            pool: self.0.clone(),
            key,
            id,
            cookie,
            retire: false,
        }))
    }
}

impl IdentityLease {
    pub fn cookies(&self) -> Arc<CookieSession> {
        self.cookie.clone()
    }
    pub fn key(&self) -> &IdentityKey {
        &self.key
    }
    pub fn retire(&mut self) {
        self.retire = true;
    }
}

impl Drop for IdentityLease {
    fn drop(&mut self) {
        let mut state = self.pool.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slots) = state.slots.get_mut(&self.key) {
            if self.retire {
                self.cookie.retire();
                slots.retain(|s| s.id != self.id);
            } else if let Some(slot) = slots.iter_mut().find(|s| s.id == self.id) {
                slot.leased = false;
            }
            if slots.is_empty() {
                state.slots.remove(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(owner: &str, url: &Url) -> IdentityKey {
        IdentityKey {
            owner: owner.into(),
            origin: url.origin().ascii_serialization(),
            egress: Some("one".into()),
            fingerprint: None,
        }
    }
    #[test]
    fn exclusive_and_scoped() {
        let url = Url::parse("https://example.com/a").unwrap();
        let pool = IdentityPool::new(SessionConfig::default(), 2, 1);
        let a = pool.try_acquire(key("a", &url), &url).unwrap().unwrap();
        assert!(pool.try_acquire(key("a", &url), &url).unwrap().is_none());
        assert!(a.cookies().set_cookie(&url, "sid=secret; Path=/; Secure"));
        let b = pool.try_acquire(key("b", &url), &url).unwrap().unwrap();
        assert_eq!(b.cookies().stats().cookies, 0);
        drop(a);
        let mut a = pool.try_acquire(key("a", &url), &url).unwrap().unwrap();
        assert_eq!(a.cookies().stats().cookies, 1);
        a.retire();
        drop(a);
        assert_eq!(
            pool.try_acquire(key("a", &url), &url)
                .unwrap()
                .unwrap()
                .cookies()
                .stats()
                .cookies,
            0
        );
    }
    #[test]
    fn rejects_origin_mismatch() {
        let url = Url::parse("https://example.com/").unwrap();
        let mut k = key("owner", &url);
        k.origin = "https://example.com:444".into();
        assert!(matches!(
            IdentityPool::new(SessionConfig::default(), 1, 1).try_acquire(k, &url),
            Err(SessionError::OriginMismatch)
        ));
        let with_credentials = Url::parse("https://user:pass@example.com/").unwrap();
        assert!(matches!(
            IdentityPool::new(SessionConfig::default(), 1, 1)
                .try_acquire(key("owner", &with_credentials), &with_credentials),
            Err(SessionError::Origin)
        ));
    }

    #[test]
    fn identity_debug_redacts_proxy_credentials() {
        let url = Url::parse("https://example.com/").unwrap();
        let mut identity = key("tenant", &url);
        identity.egress = Some("http://user:secret@proxy.example/".into());
        let debug = format!("{identity:?}");
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("proxy.example"));
    }
}
