//! DataGraph persistent storage (`ns 9..=12`, never LRU-evicted by `Store::gc`).
//!
//! - `GRAPH_NODE` (`ns 9`): per-`resource_key` append list of [`VersionRecord`] entries.
//! - `GRAPH_EDGE` (`ns 10`): per-`resource_key` append list of [`VersionEdge`] transitions.
//! - `GRAPH_FACT` (`ns 11`): alias resolution (`alias\0`, `rev\0`), [`ClaimLifecycleRecord`], and [`ConflictRecord`] entries.
//! - `GRAPH_JOB` (`ns 12`): zstd-compressed job [`GraphSummary`] and [`PageGraphDelta`] records.

mod claims;
mod history;
mod job;

use std::collections::BTreeSet;
use std::fmt::Write as _;

pub use claims::DEFAULT_GRAPH_CLAIMS_LIMIT;
pub use history::GraphHistoryResult;
use pagesift_graph::{VersionEdge, VersionRecord, normalize_resource_key};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::engine::{Tx, group_key, ns};
use crate::snapshot::{ContentHash, compress_snapshot};
use crate::tasks::list_push;
use crate::{Error, Result, Store, to_ms};

pub(super) const MAX_ALIAS_HOPS: usize = 16; // [策略] 规范 URL 别名解析最大跳数
pub(super) const MAX_REV_ALIASES: usize = 128; // [策略] 反向别名列表读取上限
pub(super) const MAX_GRAPH_BLOB: usize = 64 << 20; // [策略] DataGraph 单条记录写前与解压容量对称上限 64 MiB
pub const DEFAULT_GRAPH_HISTORY_LIMIT: usize = 4096; // [策略] 单次历史查询默认返回版本与边上限
pub const DEFAULT_GRAPH_CONFLICTS_LIMIT: usize = 4096; // [策略] 单次冲突查询默认返回上限
pub const DEFAULT_GRAPH_DELTAS_LIMIT: usize = 50_000; // [策略] 单任务默认页级增量读取上限

pub(super) const PREFIX_ALIAS: &[u8] = b"alias\0";
pub(super) const PREFIX_REV: &[u8] = b"rev\0";
pub(super) const PREFIX_CONFLICT: &[u8] = b"conflict\0";
pub(super) const PREFIX_CLAIM_LC: &[u8] = b"claimlc\0";
pub(super) const PREFIX_DOC_SNAP: &[u8] = b"docsnap\0";
pub(super) const PREFIX_DOC_VER: &[u8] = b"docver\0";
pub(super) const PREFIX_SUMMARY: &[u8] = b"summary\0";
pub(super) const PREFIX_DELTA: &[u8] = b"delta\0";

const SNAP_FLAG_UNAVAILABLE: u8 = 0; // [协议] docsnap 状态字节：快照不可用或被跳过
const SNAP_FLAG_HASH: u8 = 1; // [协议] docsnap 状态字节：附带 32 字节 BLAKE3 哈希

/// Snapshot lookup outcome tied to a specific [`VersionRecord`] rather than resource latest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadedVersionSnapshot {
    Markdown(String),
    Unavailable { reason: String },
}

/// Atomic compare-and-set version commit parameters keeping heavy diff/compression out of `tx`.
pub struct VersionCommitRequest<'a> {
    pub expected_prev_doc_id: Option<&'a str>,
    pub alias_keys: &'a [String],
    pub record: &'a VersionRecord,
    pub edge: Option<&'a VersionEdge>,
    pub snapshot_bytes: Option<&'a [u8]>,
    pub snapshot_unavailable_reason: Option<&'a str>,
}

/// Result of [`GraphStore::commit_version`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionCommitOutcome {
    Committed {
        snapshot_stored: bool,
        snapshot_warning: Option<String>,
    },
    StaleParent {
        current_latest: VersionRecord,
    },
}

/// Persistent DataGraph handle returned by [`Store::graph`] or [`Store::graph_for`].
pub struct GraphStore<'a> {
    pub(super) store: &'a Store,
    pub(super) scope_prefix: Option<String>,
}

impl<'a> GraphStore<'a> {
    pub(crate) fn new(store: &'a Store, tenant: Option<&str>) -> Self {
        Self {
            store,
            scope_prefix: tenant.map(encode_tenant_prefix),
        }
    }

    pub(super) fn scope_str(&self) -> &str {
        self.scope_prefix.as_deref().unwrap_or("")
    }

    pub(super) fn scoped_key(&self, key: &str) -> Vec<u8> {
        let p = self.scope_str();
        let mut out = Vec::with_capacity(p.len() + key.len());
        out.extend_from_slice(p.as_bytes());
        out.extend_from_slice(key.as_bytes());
        out
    }

    pub(super) fn scoped_prefixed(&self, prefix: &[u8], body: &str) -> Vec<u8> {
        let p = self.scope_str();
        let mut out = Vec::with_capacity(p.len() + prefix.len() + body.len());
        out.extend_from_slice(p.as_bytes());
        out.extend_from_slice(prefix);
        out.extend_from_slice(body.as_bytes());
        out
    }

    pub(super) fn scoped_compound(&self, prefix: &[u8], a: &str, b: &str) -> Vec<u8> {
        let p = self.scope_str();
        let mut out = Vec::with_capacity(p.len() + prefix.len() + a.len() + b.len() + 1);
        out.extend_from_slice(p.as_bytes());
        out.extend_from_slice(prefix);
        out.extend_from_slice(a.as_bytes());
        out.push(0);
        out.extend_from_slice(b.as_bytes());
        out
    }

    /// Tenant-isolated body snapshot key for DataGraph version diffs.
    pub fn snapshot_key(&self, resource_key: &str) -> String {
        format!("graph\0{}{resource_key}", self.scope_str())
    }

    /// Idempotently append a [`VersionRecord`] and optional incoming [`VersionEdge`] under `record.resource_key`.
    pub fn record_version(&self, record: &VersionRecord, edge: Option<&VersionEdge>) -> Result<()> {
        let rec_bytes = encode_json(record, "version record")?;
        let edge_bytes = edge.map(|e| encode_json(e, "version edge")).transpose()?;
        let ts = record_ts(record);
        let rec_key = self.scoped_key(&record.resource_key);
        let doc_snap_key = self.scoped_prefixed(PREFIX_DOC_SNAP, &record.doc_id);
        let doc_ver_key = self.scoped_prefixed(PREFIX_DOC_VER, &record.doc_id);
        let scope = self.scope_str();

        let mut candidate_keys = BTreeSet::new();
        candidate_keys.insert(record.resource_key.clone());
        for member in self.alias_component(&record.resource_key)? {
            candidate_keys.insert(member);
        }

        let mut tx = self.store.tx();
        if tx.contains(ns::GRAPH_FACT, &doc_snap_key)?
            || tx.contains(ns::GRAPH_FACT, &doc_ver_key)?
        {
            drop(tx);
            return Ok(());
        }
        let mut current_latest: Option<VersionRecord> = None;
        for key in &candidate_keys {
            if let Some(rec) = latest_tail_in_tx(&tx, scope, key)? {
                if rec.doc_id == record.doc_id {
                    drop(tx);
                    return Ok(());
                }
                let replace = current_latest
                    .as_ref()
                    .is_none_or(|b| rec.observed_at_ms >= b.observed_at_ms);
                if replace {
                    current_latest = Some(rec);
                }
            }
        }
        if let Some(e) = edge {
            let expected_prev = e.prev_doc_id.as_deref();
            let actual_prev = current_latest.as_ref().map(|c| c.doc_id.as_str());
            if expected_prev != actual_prev {
                drop(tx);
                return Err(Error::Corrupt(format!(
                    "stale parent in record_version for {}: expected {:?}, current {:?}",
                    record.resource_key, expected_prev, actual_prev
                )));
            }
        }
        tx.put(ns::GRAPH_FACT, &doc_ver_key, &[1], ts)?;
        if !tx.contains(ns::GRAPH_FACT, &doc_snap_key)? {
            let reason = b"snapshot_omitted";
            let mut flag_val = Vec::with_capacity(1 + reason.len());
            flag_val.push(SNAP_FLAG_UNAVAILABLE);
            flag_val.extend_from_slice(reason);
            tx.put(ns::GRAPH_FACT, &doc_snap_key, &flag_val, ts)?;
        }
        list_push(&mut tx, ns::GRAPH_NODE, &rec_key, &rec_bytes, ts)?;
        if let Some(bytes) = edge_bytes {
            let ek = edge.map_or(record.resource_key.as_str(), |e| e.resource_key.as_str());
            let edge_key = self.scoped_key(ek);
            list_push(&mut tx, ns::GRAPH_EDGE, &edge_key, &bytes, ts)?;
        }
        drop(tx);
        self.store.finish();
        Ok(())
    }

    /// Atomically commit a version, aliases, and bound snapshot only if the tail matches `expected_prev_doc_id`.
    pub fn commit_version(&self, req: VersionCommitRequest<'_>) -> Result<VersionCommitOutcome> {
        let rec_bytes = encode_json(req.record, "version record")?;
        let edge_bytes = req
            .edge
            .map(|e| encode_json(e, "version edge"))
            .transpose()?;
        let ts = record_ts(req.record);
        let cfg = self.store.config();

        enum PreparedSnap {
            Store {
                hash: ContentHash,
                raw_len: u32,
                compressed: Option<Vec<u8>>,
            },
            Unavailable {
                reason: String,
                warning: Option<String>,
            },
        }

        let prepared_snap = match req.snapshot_bytes {
            Some(bytes) if bytes.len() > cfg.snapshot_max_bytes => {
                let reason = format!(
                    "snapshot_too_large:{}_gt_{}",
                    bytes.len(),
                    cfg.snapshot_max_bytes
                );
                let warning = format!(
                    "DataGraph body snapshot skipped ({} bytes exceeds limit {} bytes)",
                    bytes.len(),
                    cfg.snapshot_max_bytes
                );
                PreparedSnap::Unavailable {
                    reason,
                    warning: Some(warning),
                }
            }
            Some(bytes) => {
                let hash = ContentHash::of(bytes);
                let compressed = if self.store.contains(ns::SNAP_BLOB, &hash.0)? {
                    None
                } else {
                    Some(compress_snapshot(bytes, cfg.snapshot_compress_level))
                };
                PreparedSnap::Store {
                    hash,
                    raw_len: bytes.len() as u32,
                    compressed,
                }
            }
            None => PreparedSnap::Unavailable {
                reason: req
                    .snapshot_unavailable_reason
                    .unwrap_or("snapshot_omitted")
                    .to_string(),
                warning: None,
            },
        };

        let mut candidate_keys = BTreeSet::new();
        candidate_keys.insert(req.record.resource_key.clone());
        for a in req.alias_keys {
            if !self.is_foreign_alias_pointer(a, &req.record.resource_key)? {
                candidate_keys.insert(a.clone());
            }
        }
        for member in self.alias_component(&req.record.resource_key)? {
            candidate_keys.insert(member);
        }

        let scope = self.scope_str();
        let rec_key = self.scoped_key(&req.record.resource_key);
        let snap_list_key = self.snapshot_key(&req.record.resource_key);
        let doc_snap_key = self.scoped_prefixed(PREFIX_DOC_SNAP, &req.record.doc_id);

        let mut tx = self.store.tx();
        let mut current_latest: Option<VersionRecord> = None;
        for key in &candidate_keys {
            if let Some(rec) = latest_tail_in_tx(&tx, scope, key)? {
                let replace = current_latest
                    .as_ref()
                    .is_none_or(|b| rec.observed_at_ms >= b.observed_at_ms);
                if replace {
                    current_latest = Some(rec);
                }
            }
        }
        if let Some(ref curr) = current_latest {
            let matches_expected = req.expected_prev_doc_id == Some(curr.doc_id.as_str());
            if !matches_expected {
                drop(tx);
                return Ok(VersionCommitOutcome::StaleParent {
                    current_latest: curr.clone(),
                });
            }
        }

        for alias in req.alias_keys {
            history::bind_alias_in_tx(&mut tx, scope, alias, &req.record.resource_key, ts)?;
        }

        let (snapshot_stored, snapshot_warning) = match prepared_snap {
            PreparedSnap::Store {
                hash,
                raw_len,
                compressed,
            } => {
                if !tx.contains(ns::SNAP_BLOB, &hash.0)? {
                    let blob = match compressed {
                        Some(c) => c,
                        None => compress_snapshot(
                            req.snapshot_bytes.unwrap_or(&[]),
                            cfg.snapshot_compress_level,
                        ),
                    };
                    tx.put(ns::SNAP_BLOB, &hash.0, &blob, ts)?;
                }
                let mut item = Vec::with_capacity(36); // [协议] 32 字节 BLAKE3 + 4 字节小端长度
                item.extend_from_slice(&hash.0);
                item.extend_from_slice(&raw_len.to_le_bytes());
                list_push(&mut tx, ns::SNAP_ITEM, snap_list_key.as_bytes(), &item, ts)?;

                let mut flag_val = Vec::with_capacity(33); // [协议] 1 字节状态标志 + 32 字节哈希
                flag_val.push(SNAP_FLAG_HASH);
                flag_val.extend_from_slice(&hash.0);
                tx.put(ns::GRAPH_FACT, &doc_snap_key, &flag_val, ts)?;
                (true, None)
            }
            PreparedSnap::Unavailable { reason, warning } => {
                let mut flag_val = Vec::with_capacity(1 + reason.len());
                flag_val.push(SNAP_FLAG_UNAVAILABLE);
                flag_val.extend_from_slice(reason.as_bytes());
                tx.put(ns::GRAPH_FACT, &doc_snap_key, &flag_val, ts)?;
                (false, warning)
            }
        };

        list_push(&mut tx, ns::GRAPH_NODE, &rec_key, &rec_bytes, ts)?;
        if let Some(bytes) = edge_bytes {
            let ek = req.edge.map_or(req.record.resource_key.as_str(), |e| {
                e.resource_key.as_str()
            });
            let edge_key = self.scoped_key(ek);
            list_push(&mut tx, ns::GRAPH_EDGE, &edge_key, &bytes, ts)?;
        }
        drop(tx);
        self.store.finish();
        Ok(VersionCommitOutcome::Committed {
            snapshot_stored,
            snapshot_warning,
        })
    }

    /// Load the exact body snapshot belonging to `record.doc_id`, never falling back to an older version.
    pub fn load_version_snapshot(&self, record: &VersionRecord) -> Result<LoadedVersionSnapshot> {
        let key = self.scoped_prefixed(PREFIX_DOC_SNAP, &record.doc_id);
        let Some((_, raw)) = self.store.get(ns::GRAPH_FACT, &key)? else {
            return Ok(LoadedVersionSnapshot::Unavailable {
                reason: "previous_snapshot_missing".into(),
            });
        };
        const SNAP_HASH_RECORD_LEN: usize = 33; // [协议] 1 字节标志 + 32 字节哈希总长度
        match raw.first().copied() {
            Some(SNAP_FLAG_HASH) if raw.len() == SNAP_HASH_RECORD_LEN => {
                let hash = ContentHash(raw[1..SNAP_HASH_RECORD_LEN].try_into().expect("32 bytes"));
                match self.store.snapshots().get(&hash)? {
                    Some(bytes) => {
                        let text = String::from_utf8(bytes).map_err(|e| {
                            Error::Corrupt(format!("graph snapshot {} utf8: {e}", record.doc_id))
                        })?;
                        Ok(LoadedVersionSnapshot::Markdown(text))
                    }
                    None => Ok(LoadedVersionSnapshot::Unavailable {
                        reason: "previous_snapshot_evicted".into(),
                    }),
                }
            }
            Some(SNAP_FLAG_UNAVAILABLE) => {
                let reason = String::from_utf8_lossy(&raw[1..]).into_owned();
                Ok(LoadedVersionSnapshot::Unavailable {
                    reason: if reason.is_empty() {
                        "previous_snapshot_unavailable".into()
                    } else {
                        reason
                    },
                })
            }
            _ => Err(Error::Corrupt(format!(
                "invalid docsnap record for {}",
                record.doc_id
            ))),
        }
    }
}

pub(crate) fn encode_tenant_prefix(tenant: &str) -> String {
    let hash = blake3::hash(tenant.as_bytes());
    let mut out = String::with_capacity(35); // [协议] 固定前缀 "t:" + 32 位 hex + "\0" 共 35 字节
    out.push_str("t:");
    for b in &hash.as_bytes()[..16] {
        let _ = write!(out, "{b:02x}");
    }
    out.push('\0');
    out
}

pub(super) fn scope_prefixed_key(scope: &str, prefix: &[u8], body: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(scope.len() + prefix.len() + body.len());
    out.extend_from_slice(scope.as_bytes());
    out.extend_from_slice(prefix);
    out.extend_from_slice(body.as_bytes());
    out
}

pub(super) fn scope_list_key(scope: &str, key: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(scope.len() + key.len());
    out.extend_from_slice(scope.as_bytes());
    out.extend_from_slice(key.as_bytes());
    out
}

pub(super) fn latest_tail_in_tx(
    tx: &Tx<'_>,
    scope: &str,
    resource_key: &str,
) -> Result<Option<VersionRecord>> {
    let list_key = scope_list_key(scope, resource_key);
    let Some(g) = tx.group(ns::GRAPH_NODE, &list_key) else {
        return Ok(None);
    };
    if g.live == 0 || g.next <= g.head {
        return Ok(None);
    }
    for seq in (g.head..g.next).rev() {
        let key = group_key(&list_key, &seq.to_be_bytes())?;
        if let Some((_, raw)) = tx.get(ns::GRAPH_NODE, &key)? {
            return decode_json(&raw, "version record").map(Some);
        }
    }
    Ok(None)
}

fn record_ts(record: &VersionRecord) -> u64 {
    if record.observed_at_ms > 0 {
        record.observed_at_ms
    } else {
        to_ms(std::time::SystemTime::now())
    }
}

pub(super) fn initial_key(url_or_key: &str) -> String {
    if url_or_key.contains("://") {
        normalize_resource_key(url_or_key)
    } else {
        url_or_key.to_string()
    }
}

pub(super) fn encode_json<T: Serialize + ?Sized>(value: &T, label: &str) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|e| Error::Corrupt(format!("encode {label}: {e}")))
}

pub(super) fn decode_json<T: DeserializeOwned>(raw: &[u8], label: &str) -> Result<T> {
    serde_json::from_slice(raw).map_err(|e| Error::Corrupt(format!("decode {label}: {e}")))
}
