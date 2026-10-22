//! Explicit item identity for incremental collectors. Callers supply a stable
//! source/item key and a version token (or empty bytes for identity-only mode).
//! Commit AFTER successful delivery. This is not a lease or an atomic reservation:
//! concurrent consumers still need coordination or an idempotent destination.
use crate::engine::{group_key, ns};
use crate::{Result, Store, to_ms};
use std::time::SystemTime;

pub struct Items<'a> {
    store: &'a Store,
}

impl<'a> Items<'a> {
    pub(crate) fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// Whether this exact source/item revision was successfully committed.
    /// URLs are deliberately not part of the key. No missing-page deletion is
    /// inferred: absence from a listing is not proof an item was deleted.
    pub fn matches(&self, source: &[u8], item: &[u8], revision: &[u8]) -> Result<bool> {
        Ok(self
            .store
            .get(ns::ITEM, &group_key(source, item)?)?
            .is_some_and(|(_, value)| value == revision))
    }

    /// Save completion in a single CRC-protected record. Durability follows the
    /// Store's SyncPolicy; process termination and power loss are different risks.
    pub fn commit(&self, source: &[u8], item: &[u8], revision: &[u8]) -> Result<()> {
        self.store
            .put(
                ns::ITEM,
                &group_key(source, item)?,
                revision,
                to_ms(SystemTime::now()),
            )
            .map(drop)
    }

    /// Explicitly forget a completed identity so it will be processed again.
    pub fn forget(&self, source: &[u8], item: &[u8]) -> Result<bool> {
        self.store.delete(ns::ITEM, &group_key(source, item)?)
    }
}
