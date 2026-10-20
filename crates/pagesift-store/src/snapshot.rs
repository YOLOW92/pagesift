//! 原始 HTML 快照（F-8.3）：zstd 压缩，按内容的 BLAKE3 哈希去重，按 URL 和时间查询。
//!
//! 每个 URL 一个追加列表，元素是 (时间, 内容哈希, 原始长度)；内容本身按哈希只存一份。
//! 内容可能被 LRU 淘汰，这时 [`SnapshotMeta::available`] 为 false。

use std::fmt;
use std::ops::RangeBounds;
use std::time::SystemTime;

use crate::engine::ns;
use crate::tasks::{list_push, list_scan};
use crate::{Error, Result, Store, from_ms, to_ms};

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash(pub [u8; 32]);

impl ContentHash {
    pub fn of(data: &[u8]) -> Self {
        Self(*blake3::hash(data).as_bytes())
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentHash({self})")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotMeta {
    pub at: SystemTime,
    pub hash: ContentHash,
    pub raw_len: u32,
    pub available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotPut {
    pub hash: ContentHash,
    /// 同样的内容已经存过，这次没有再写内容。
    pub deduped: bool,
    /// 压缩后的大小（去重时为 0）。
    pub stored_bytes: usize,
}

pub struct Snapshots<'a> {
    store: &'a Store,
}

impl<'a> Snapshots<'a> {
    pub(crate) fn new(store: &'a Store) -> Self {
        Self { store }
    }

    pub fn put(&self, url: &str, html: &[u8]) -> Result<SnapshotPut> {
        self.put_at(url, html, SystemTime::now())
    }

    pub fn put_at(&self, url: &str, html: &[u8], at: SystemTime) -> Result<SnapshotPut> {
        let cfg = self.store.config();
        if html.len() > cfg.snapshot_max_bytes {
            return Err(Error::ValueTooLarge {
                size: html.len(),
                limit: cfg.snapshot_max_bytes,
            });
        }
        let hash = ContentHash::of(html);
        // 先压缩，避免持有写锁时做 CPU 密集的工作；去重命中时浪费一次压缩。
        let deduped = self.store.contains(ns::SNAP_BLOB, &hash.0)?;
        let compressed = if deduped {
            Vec::new()
        } else {
            zstd::bulk::compress(html, cfg.snapshot_zstd_level)?
        };
        let mut item = Vec::with_capacity(36);
        item.extend_from_slice(&hash.0);
        item.extend_from_slice(&(html.len() as u32).to_le_bytes());

        let mut tx = self.store.tx();
        let mut stored_bytes = 0;
        if !tx.contains(ns::SNAP_BLOB, &hash.0)? {
            let data = if deduped {
                zstd::bulk::compress(html, cfg.snapshot_zstd_level)?
            } else {
                compressed
            };
            stored_bytes = data.len();
            tx.put(ns::SNAP_BLOB, &hash.0, &data, to_ms(at))?;
        }
        list_push(&mut tx, ns::SNAP_ITEM, url.as_bytes(), &item, to_ms(at))?;
        drop(tx);
        self.store.finish();
        Ok(SnapshotPut {
            hash,
            deduped: stored_bytes == 0,
            stored_bytes,
        })
    }

    /// 按哈希取回原始 HTML。
    pub fn get(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>> {
        let Some((_, data)) = self.store.get(ns::SNAP_BLOB, &hash.0)? else {
            return Ok(None);
        };
        let html = zstd::stream::decode_all(data.as_slice())
            .map_err(|e| Error::Corrupt(format!("snapshot {hash}: {e}")))?;
        Ok(Some(html))
    }

    /// 某个 URL 在时间范围内的快照，按时间先后排列。
    pub fn list(
        &self,
        url: &str,
        range: impl RangeBounds<SystemTime>,
    ) -> Result<Vec<SnapshotMeta>> {
        let mut out = Vec::new();
        for (_, ts, v) in list_scan(self.store, ns::SNAP_ITEM, url.as_bytes(), usize::MAX)? {
            let at = from_ms(ts);
            if !range.contains(&at) || v.len() != 36 {
                continue;
            }
            let hash = ContentHash(v[..32].try_into().expect("32 bytes"));
            out.push(SnapshotMeta {
                at,
                hash,
                raw_len: u32::from_le_bytes(v[32..36].try_into().expect("4 bytes")),
                available: self.store.contains(ns::SNAP_BLOB, &hash.0)?,
            });
        }
        out.sort_by_key(|m| m.at);
        Ok(out)
    }

    /// 最新的一份快照。
    pub fn latest(&self, url: &str) -> Result<Option<SnapshotMeta>> {
        Ok(self.list(url, ..)?.pop())
    }
}
