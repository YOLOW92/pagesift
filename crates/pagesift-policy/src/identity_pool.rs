//! Origin- and owner-scoped identity leases. No cookie values enter diagnostics.

use std::collections::{BTreeMap, HashMap};
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
    /// Present in `State::idle_lru` while unleased (`None` while leased).
    idle_seq: Option<u64>,
}

#[derive(Default)]
struct State {
    next_id: u64,
    next_lru_seq: u64,
    total_slots: usize,
    slots: HashMap<IdentityKey, Vec<Slot>>,
    /// Ordered index of currently idle slots for O(log N) LRU eviction when the pool is full.
    idle_lru: BTreeMap<u64, (IdentityKey, u64)>,
}

impl State {
    fn alloc_lru_seq(&mut self) -> u64 {
        let seq = self.next_lru_seq;
        self.next_lru_seq = self.next_lru_seq.wrapping_add(1);
        seq
    }

    fn remove_slot(&mut self, key: &IdentityKey, slot_id: u64) -> Option<Slot> {
        let slots = self.slots.get_mut(key)?;
        let pos = slots.iter().position(|s| s.id == slot_id)?;
        let removed = slots.swap_remove(pos);
        if let Some(seq) = removed.idle_seq {
            self.idle_lru.remove(&seq);
        }
        self.total_slots = self.total_slots.saturating_sub(1);
        if slots.is_empty() {
            self.slots.remove(key);
        }
        Some(removed)
    }

    fn evict_one_idle(&mut self) -> bool {
        while let Some((_, (victim_key, victim_id))) = self.idle_lru.pop_first() {
            if let Some(slots) = self.slots.get_mut(&victim_key)
                && let Some(pos) = slots.iter().position(|s| s.id == victim_id && !s.leased)
            {
                let victim = slots.swap_remove(pos);
                victim.cookie.retire();
                self.total_slots = self.total_slots.saturating_sub(1);
                if slots.is_empty() {
                    self.slots.remove(&victim_key);
                }
                return true;
            }
        }
        false
    }
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

        // O(1) lookup for the target key: prune expired/retired/exhausted idle slots for this key
        // only, avoiding an O(pool) scan and nested CookieSession locks on every acquire.
        let mut stale_ids = Vec::new();
        let mut chosen_idx = None;
        if let Some(slots) = state.slots.get(&key) {
            for (idx, s) in slots.iter().enumerate() {
                if s.leased {
                    continue;
                }
                let stats = s.cookie.stats();
                if stats.retired || stats.expires_at <= now || stats.uses >= max_uses {
                    stale_ids.push(s.id);
                } else if chosen_idx.is_none() {
                    chosen_idx = Some(idx);
                }
            }
        }
        if let Some(idx) = chosen_idx
            && let Some(slots) = state.slots.get_mut(&key)
        {
            let slot = &mut slots[idx];
            slot.leased = true;
            let old_seq = slot.idle_seq.take();
            let id = slot.id;
            let cookie = slot.cookie.clone();
            if let Some(seq) = old_seq {
                state.idle_lru.remove(&seq);
            }
            for sid in stale_ids {
                state.remove_slot(&key, sid);
            }
            return Ok(Some(IdentityLease {
                pool: self.0.clone(),
                key,
                id,
                cookie,
                retire: false,
            }));
        }
        for sid in stale_ids {
            state.remove_slot(&key, sid);
        }

        if state.slots.get(&key).map_or(0, Vec::len) >= self.0.max_per_key || self.0.max_total == 0
        {
            return Ok(None);
        }
        if state.total_slots >= self.0.max_total && !state.evict_one_idle() {
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
            idle_seq: None,
        });
        state.total_slots += 1;
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
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let stats = self.cookie.stats();
        let should_drop = self.retire
            || stats.retired
            || stats.expires_at <= now
            || stats.uses >= self.pool.config.max_uses;
        if self.retire {
            self.cookie.retire();
        }
        let mut state = self.pool.state.lock().unwrap_or_else(|e| e.into_inner());
        if should_drop {
            state.remove_slot(&self.key, self.id);
        } else {
            let seq = state.alloc_lru_seq();
            if let Some(slots) = state.slots.get_mut(&self.key)
                && let Some(slot) = slots.iter_mut().find(|s| s.id == self.id)
            {
                slot.leased = false;
                slot.idle_seq = Some(seq);
                state.idle_lru.insert(seq, (self.key.clone(), self.id));
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

    #[test]
    fn evicts_least_recently_used_idle_slot_when_full_and_keeps_hot_origins_reusable() {
        let pool = IdentityPool::new(SessionConfig::default(), 512, 4);
        // Fill all 512 slots with distinct cold origins.
        for i in 0..512 {
            let u = Url::parse(&format!("https://origin-{i}.example/")).unwrap();
            let lease = pool.try_acquire(key("tenant", &u), &u).unwrap().unwrap();
            assert!(lease.cookies().set_cookie(&u, "c=1; Path=/; Secure"));
        }
        // Touch origin-0 again so it becomes hot (most recently used).
        let hot = Url::parse("https://origin-0.example/").unwrap();
        {
            let lease = pool
                .try_acquire(key("tenant", &hot), &hot)
                .unwrap()
                .unwrap();
            assert_eq!(lease.cookies().stats().cookies, 1);
        }
        // Acquire a 513th distinct origin: should evict the coldest idle slot (origin-1),
        // succeed, and allow subsequent reuse for the new origin while keeping origin-0 warm.
        let new_origin = Url::parse("https://origin-512.example/").unwrap();
        {
            let lease = pool
                .try_acquire(key("tenant", &new_origin), &new_origin)
                .unwrap()
                .expect("513th origin should evict coldest idle slot");
            assert!(
                lease
                    .cookies()
                    .set_cookie(&new_origin, "fresh=1; Path=/; Secure")
            );
        }
        let reused_new = pool
            .try_acquire(key("tenant", &new_origin), &new_origin)
            .unwrap()
            .unwrap();
        assert_eq!(reused_new.cookies().stats().cookies, 1);
        let reused_hot = pool
            .try_acquire(key("tenant", &hot), &hot)
            .unwrap()
            .unwrap();
        assert_eq!(reused_hot.cookies().stats().cookies, 1);

        // When all slots are actively leased concurrently, eviction must NOT touch leased slots.
        let small = IdentityPool::new(SessionConfig::default(), 2, 1);
        let u1 = Url::parse("https://a.example/").unwrap();
        let u2 = Url::parse("https://b.example/").unwrap();
        let u3 = Url::parse("https://c.example/").unwrap();
        let _l1 = small.try_acquire(key("t", &u1), &u1).unwrap().unwrap();
        let _l2 = small.try_acquire(key("t", &u2), &u2).unwrap().unwrap();
        assert!(small.try_acquire(key("t", &u3), &u3).unwrap().is_none());
    }
}
