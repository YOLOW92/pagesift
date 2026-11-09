//! In-memory, task-scoped single-flight cache for remote image bytes.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::OnceCell;

use super::Attachment;

/// Shared only by pages within one batch or crawl. Never serialized.
#[derive(Debug, Clone, Default)]
pub struct AssetFetchCache(Arc<Mutex<HashMap<String, Arc<OnceCell<Attachment>>>>>);

impl PartialEq for AssetFetchCache {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl AssetFetchCache {
    /// Limit retained keys and bytes per item; uncached items still work normally.
    pub fn entry(&self, url: &str) -> Option<Arc<OnceCell<Attachment>>> {
        let mut entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cell) = entries.get(url) {
            return Some(cell.clone());
        }
        if entries.len() >= 32 {
            return None;
        }
        let cell = Arc::new(OnceCell::new());
        entries.insert(url.to_owned(), cell.clone());
        Some(cell)
    }

    pub fn discard(&self, url: &str, cell: &Arc<OnceCell<Attachment>>) {
        let mut entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if entries
            .get(url)
            .is_some_and(|existing| Arc::ptr_eq(existing, cell))
        {
            entries.remove(url);
        }
    }

    /// Populate a task-local entry from an already persisted, verified asset.
    pub fn seed(&self, url: &str, attachment: Attachment) {
        if attachment.bytes > 512 * 1024 || attachment.data.is_none() {
            return;
        }
        if let Some(cell) = self.entry(url) {
            let _ = cell.set(attachment);
        }
    }
}
