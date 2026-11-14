//! DataGraph persistent storage (`ns 9..=12`, never LRU-evicted by `Store::gc`).
//!
//! - `GRAPH_NODE` (`ns 9`): per-`resource_key` append list of [`VersionRecord`] entries.
//! - `GRAPH_EDGE` (`ns 10`): per-`resource_key` append list of [`VersionEdge`] transitions.
//! - `GRAPH_FACT` (`ns 11`): alias resolution (`alias\0`, `rev\0`) and [`ConflictRecord`] entries.
//! - `GRAPH_JOB` (`ns 12`): zstd-compressed job [`GraphSummary`] and [`PageGraphDelta`] records.

use std::collections::{BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::time::SystemTime;

use pagesift_graph::{
    ConflictRecord, GraphSummary, PageGraphDelta, VersionEdge, VersionRecord,
    normalize_resource_key,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::engine::{Tx, group_key, ns};
use crate::snapshot::{ContentHash, compress_snapshot};
use crate::tasks::list_push;
use crate::{Error, Result, Store, to_ms};

const MAX_ALIAS_HOPS: usize = 16; // [策略] 规范 URL 别名解析最大跳数
const MAX_REV_ALIASES: usize = 128; // [策略] 反向别名列表读取上限
const MAX_GRAPH_BLOB: usize = 64 << 20; // [策略] DataGraph 单条记录写前与解压容量对称上限 64 MiB
pub const DEFAULT_GRAPH_HISTORY_LIMIT: usize = 4096; // [策略] 单次历史查询默认返回版本与边上限
pub const DEFAULT_GRAPH_CONFLICTS_LIMIT: usize = 4096; // [策略] 单次冲突查询默认返回上限
pub const DEFAULT_GRAPH_DELTAS_LIMIT: usize = 50_000; // [策略] 单任务默认页级增量读取上限

const PREFIX_ALIAS: &[u8] = b"alias\0";
const PREFIX_REV: &[u8] = b"rev\0";
const PREFIX_CONFLICT: &[u8] = b"conflict\0";
const PREFIX_DOC_SNAP: &[u8] = b"docsnap\0";
const PREFIX_DOC_VER: &[u8] = b"docver\0";
const PREFIX_SUMMARY: &[u8] = b"summary\0";
const PREFIX_DELTA: &[u8] = b"delta\0";

const SNAP_FLAG_UNAVAILABLE: u8 = 0; // [协议] docsnap 状态字节：快照不可用或被跳过
const SNAP_FLAG_HASH: u8 = 1; // [协议] docsnap 状态字节：附带 32 字节 BLAKE3 哈希

/// Paginated/bounded history result for a resource key across its verified aliases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphHistoryResult {
    pub resource_key: String,
    pub versions: Vec<VersionRecord>,
    pub edges: Vec<VersionEdge>,
    pub truncated: bool,
}

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
    store: &'a Store,
    scope_prefix: Option<String>,
}

impl<'a> GraphStore<'a> {
    pub(crate) fn new(store: &'a Store, tenant: Option<&str>) -> Self {
        Self {
            store,
            scope_prefix: tenant.map(encode_tenant_prefix),
        }
    }

    fn scope_str(&self) -> &str {
        self.scope_prefix.as_deref().unwrap_or("")
    }

    fn scoped_key(&self, key: &str) -> Vec<u8> {
        let p = self.scope_str();
        let mut out = Vec::with_capacity(p.len() + key.len());
        out.extend_from_slice(p.as_bytes());
        out.extend_from_slice(key.as_bytes());
        out
    }

    fn scoped_prefixed(&self, prefix: &[u8], body: &str) -> Vec<u8> {
        let p = self.scope_str();
        let mut out = Vec::with_capacity(p.len() + prefix.len() + body.len());
        out.extend_from_slice(p.as_bytes());
        out.extend_from_slice(prefix);
        out.extend_from_slice(body.as_bytes());
        out
    }

    fn scoped_compound(&self, prefix: &[u8], a: &str, b: &str) -> Vec<u8> {
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

        let mut tx = self.store.tx();
        if tx.contains(ns::GRAPH_FACT, &doc_snap_key)?
            || tx.contains(ns::GRAPH_FACT, &doc_ver_key)?
            || latest_tail_in_tx(&tx, scope, &record.resource_key)?
                .is_some_and(|tail| tail.doc_id == record.doc_id)
        {
            drop(tx);
            return Ok(());
        }
        tx.put(ns::GRAPH_FACT, &doc_ver_key, &[1], ts)?;
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
            candidate_keys.insert(a.clone());
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
            bind_alias_in_tx(&mut tx, scope, alias, &req.record.resource_key, ts)?;
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
                        None => {
                            let bytes = req.snapshot_bytes.unwrap_or(&[]);
                            compress_snapshot(bytes, cfg.snapshot_compress_level)
                        }
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

    /// Bind `alias_key -> canonical_key` so URL moves and same-site canonical declarations share history.
    pub fn bind_alias(&self, alias_key: &str, canonical_key: &str) -> Result<()> {
        let now = to_ms(SystemTime::now());
        let mut tx = self.store.tx();
        bind_alias_in_tx(&mut tx, self.scope_str(), alias_key, canonical_key, now)?;
        drop(tx);
        self.store.finish();
        Ok(())
    }

    /// Return the canonical resource key reached by following stored forward aliases.
    pub fn resolve_resource_key(&self, url_or_key: &str) -> Result<String> {
        let initial = initial_key(url_or_key);
        self.resolve_forward(&initial)
    }

    /// Look up the most recent [`VersionRecord`] for any URL or alias sharing `urls`.
    pub fn latest_version_for_urls(&self, urls: &[&str]) -> Result<Option<VersionRecord>> {
        let mut keys = BTreeSet::new();
        for url in urls {
            let k = initial_key(url);
            for member in self.alias_component(&k)? {
                keys.insert(member);
            }
        }
        let mut best: Option<VersionRecord> = None;
        for key in keys {
            if let Some(rec) = self.latest_tail(&key)? {
                let replace = best
                    .as_ref()
                    .is_none_or(|b| rec.observed_at_ms >= b.observed_at_ms);
                if replace {
                    best = Some(rec);
                }
            }
        }
        Ok(best)
    }

    /// Return `(canonical_resource_key, versions, edges)` bounded to the latest [`DEFAULT_GRAPH_HISTORY_LIMIT`].
    pub fn history(
        &self,
        url_or_key: &str,
    ) -> Result<(String, Vec<VersionRecord>, Vec<VersionEdge>)> {
        let page = self.history_bounded(url_or_key, DEFAULT_GRAPH_HISTORY_LIMIT)?;
        Ok((page.resource_key, page.versions, page.edges))
    }

    /// Return bounded latest history across all linked aliases of `url_or_key`.
    pub fn history_bounded(&self, url_or_key: &str, limit: usize) -> Result<GraphHistoryResult> {
        let start = initial_key(url_or_key);
        let canonical = self.resolve_forward(&start)?;
        let component = self.alias_component(&start)?;

        let mut versions = Vec::new();
        let mut edges = Vec::new();
        let mut truncated = false;
        for key in component {
            let scoped = self.scoped_key(&key);
            let (rec_raws, rec_more) = self.list_scan_tail(ns::GRAPH_NODE, &scoped, limit)?;
            truncated |= rec_more;
            for raw in rec_raws {
                versions.push(decode_json::<VersionRecord>(&raw, "version record")?);
            }
            let (edge_raws, edge_more) = self.list_scan_tail(ns::GRAPH_EDGE, &scoped, limit)?;
            truncated |= edge_more;
            for raw in edge_raws {
                edges.push(decode_json::<VersionEdge>(&raw, "version edge")?);
            }
        }
        versions.sort_by(|a, b| {
            a.observed_at_ms
                .cmp(&b.observed_at_ms)
                .then_with(|| a.doc_id.cmp(&b.doc_id))
        });
        versions.dedup_by(|a, b| a.observed_at_ms == b.observed_at_ms && a.doc_id == b.doc_id);
        if versions.len() > limit {
            truncated = true;
            let drop_n = versions.len() - limit;
            versions.drain(..drop_n);
        }

        edges.sort_by(|a, b| {
            a.curr_observed_at_ms
                .cmp(&b.curr_observed_at_ms)
                .then_with(|| a.curr_doc_id.cmp(&b.curr_doc_id))
        });
        edges.dedup_by(|a, b| {
            a.curr_observed_at_ms == b.curr_observed_at_ms && a.curr_doc_id == b.curr_doc_id
        });
        if edges.len() > limit {
            truncated = true;
            let drop_n = edges.len() - limit;
            edges.drain(..drop_n);
        }
        Ok(GraphHistoryResult {
            resource_key: canonical,
            versions,
            edges,
            truncated,
        })
    }

    /// Persist conflict records idempotently into `ns::GRAPH_FACT`.
    pub fn put_conflicts(&self, conflicts: &[ConflictRecord]) -> Result<()> {
        if conflicts.is_empty() {
            return Ok(());
        }
        let now = to_ms(SystemTime::now());
        let mut tx = self.store.tx();
        for conflict in conflicts {
            let key = self.scoped_compound(
                PREFIX_CONFLICT,
                &conflict.subject_key,
                &conflict.conflict_id,
            );
            let val = encode_json(conflict, "conflict record")?;
            tx.put(ns::GRAPH_FACT, &key, &val, now)?;
        }
        drop(tx);
        self.store.finish();
        Ok(())
    }

    /// List up to [`DEFAULT_GRAPH_CONFLICTS_LIMIT`] stored conflicts, optionally filtered by `subject_key`.
    pub fn list_conflicts(&self, subject_key: Option<&str>) -> Result<Vec<ConflictRecord>> {
        let (items, _, _) =
            self.list_conflicts_bounded(subject_key, DEFAULT_GRAPH_CONFLICTS_LIMIT, 0)?;
        Ok(items)
    }

    /// Paginated conflict listing returning `(conflicts, total_matching, has_more)`.
    pub fn list_conflicts_bounded(
        &self,
        subject_key: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<ConflictRecord>, usize, bool)> {
        let mut prefix = self.scoped_prefixed(PREFIX_CONFLICT, "");
        if let Some(subj) = subject_key {
            prefix.extend_from_slice(subj.as_bytes());
            prefix.push(0);
        }
        let keys = self
            .store
            .keys(ns::GRAPH_FACT, |k| k.starts_with(&prefix))?;
        let total = keys.len();
        if offset >= total || limit == 0 {
            return Ok((Vec::new(), total, offset < total));
        }
        let end = total.min(offset.saturating_add(limit));
        let has_more = end < total;
        let mut out = Vec::with_capacity(end - offset);
        for key in &keys[offset..end] {
            if let Some((_, raw)) = self.store.get(ns::GRAPH_FACT, key)? {
                out.push(decode_json(&raw, "conflict record")?);
            }
        }
        Ok((out, total, has_more))
    }

    /// Persist a zstd-compressed job [`GraphSummary`] under `ns::GRAPH_JOB` with symmetric capacity checks.
    pub fn put_job_summary(&self, job_id: &str, summary: &GraphSummary) -> Result<()> {
        let json = encode_json(summary, "graph summary")?;
        if json.len() > MAX_GRAPH_BLOB {
            return Err(Error::ValueTooLarge {
                size: json.len(),
                limit: MAX_GRAPH_BLOB,
            });
        }
        let compressed = compress_snapshot(&json, self.store.config().snapshot_compress_level);
        if compressed.len() > MAX_GRAPH_BLOB {
            return Err(Error::ValueTooLarge {
                size: compressed.len(),
                limit: MAX_GRAPH_BLOB,
            });
        }
        self.store
            .put(
                ns::GRAPH_JOB,
                &self.scoped_prefixed(PREFIX_SUMMARY, job_id),
                &compressed,
                to_ms(SystemTime::now()),
            )
            .map(drop)
    }

    /// Load and decompress a stored job [`GraphSummary`].
    pub fn get_job_summary(&self, job_id: &str) -> Result<Option<GraphSummary>> {
        let Some((_, raw)) = self
            .store
            .get(ns::GRAPH_JOB, &self.scoped_prefixed(PREFIX_SUMMARY, job_id))?
        else {
            return Ok(None);
        };
        let decompressed = pagesift_compress::decompress(&raw, MAX_GRAPH_BLOB)
            .map_err(|e| Error::Corrupt(format!("graph summary {job_id}: {e}")))?;
        decode_json(&decompressed, "graph summary").map(Some)
    }

    /// Persist one zstd-compressed [`PageGraphDelta`] belonging to `job_id` with symmetric capacity checks.
    pub fn append_job_delta(&self, job_id: &str, delta: &PageGraphDelta) -> Result<()> {
        let json = encode_json(delta, "page graph delta")?;
        if json.len() > MAX_GRAPH_BLOB {
            return Err(Error::ValueTooLarge {
                size: json.len(),
                limit: MAX_GRAPH_BLOB,
            });
        }
        let compressed = compress_snapshot(&json, self.store.config().snapshot_compress_level);
        if compressed.len() > MAX_GRAPH_BLOB {
            return Err(Error::ValueTooLarge {
                size: compressed.len(),
                limit: MAX_GRAPH_BLOB,
            });
        }
        let key = self.scoped_compound(PREFIX_DELTA, job_id, &delta.node.doc_id);
        let ts = if delta.node.observed_at_ms > 0 {
            delta.node.observed_at_ms
        } else {
            to_ms(SystemTime::now())
        };
        self.store
            .put(ns::GRAPH_JOB, &key, &compressed, ts)
            .map(drop)
    }

    /// Read stored [`PageGraphDelta`] entries for `job_id`, bounded by [`DEFAULT_GRAPH_DELTAS_LIMIT`].
    pub fn job_deltas(&self, job_id: &str) -> Result<Vec<PageGraphDelta>> {
        let (deltas, _) = self.job_deltas_bounded(job_id, DEFAULT_GRAPH_DELTAS_LIMIT)?;
        Ok(deltas)
    }

    /// Read up to `limit` [`PageGraphDelta`] entries for `job_id`, sorted deterministically.
    pub fn job_deltas_bounded(
        &self,
        job_id: &str,
        limit: usize,
    ) -> Result<(Vec<PageGraphDelta>, bool)> {
        let mut out = Vec::new();
        let has_more = self.visit_job_deltas(job_id, limit, |delta| {
            out.push(delta);
            Ok(())
        })?;
        out.sort_by(|a, b| {
            a.node
                .observed_at_ms
                .cmp(&b.node.observed_at_ms)
                .then_with(|| a.node.doc_id.cmp(&b.node.doc_id))
        });
        Ok((out, has_more))
    }

    /// Stream at most `limit` decoded [`PageGraphDelta`] items for `job_id` without buffering all in RAM.
    pub fn visit_job_deltas(
        &self,
        job_id: &str,
        limit: usize,
        mut visitor: impl FnMut(PageGraphDelta) -> Result<()>,
    ) -> Result<bool> {
        let prefix = self.scoped_compound(PREFIX_DELTA, job_id, "");
        let keys = self.store.keys(ns::GRAPH_JOB, |k| k.starts_with(&prefix))?;
        let has_more = keys.len() > limit;
        for key in keys.into_iter().take(limit) {
            if let Some((_, raw)) = self.store.get(ns::GRAPH_JOB, &key)? {
                let decompressed = pagesift_compress::decompress(&raw, MAX_GRAPH_BLOB)
                    .map_err(|e| Error::Corrupt(format!("graph delta {job_id}: {e}")))?;
                visitor(decode_json(&decompressed, "page graph delta")?)?;
            }
        }
        Ok(has_more)
    }

    /// Delete stored summary and page deltas belonging to `job_id` within this tenant scope.
    pub fn purge_job(&self, job_id: &str) -> Result<usize> {
        let summary_key = self.scoped_prefixed(PREFIX_SUMMARY, job_id);
        let delta_prefix = self.scoped_compound(PREFIX_DELTA, job_id, "");
        let delta_keys = self
            .store
            .keys(ns::GRAPH_JOB, |k| k.starts_with(&delta_prefix))?;
        let mut removed = 0usize;
        if self.store.delete(ns::GRAPH_JOB, &summary_key)? {
            removed += 1;
        }
        for key in delta_keys {
            if self.store.delete(ns::GRAPH_JOB, &key)? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn list_scan_tail(&self, n: u8, list_key: &[u8], limit: usize) -> Result<(Vec<Vec<u8>>, bool)> {
        let Some(g) = self.store.group(n, list_key) else {
            return Ok((Vec::new(), false));
        };
        if g.live == 0 || g.next <= g.head {
            return Ok((Vec::new(), false));
        }
        if limit == 0 {
            return Ok((Vec::new(), true));
        }
        let mut out = Vec::with_capacity(limit.min(g.live as usize));
        let mut has_more = false;
        for seq in (g.head..g.next).rev() {
            if let Some((_, v)) = self
                .store
                .get(n, &group_key(list_key, &seq.to_be_bytes())?)?
            {
                if out.len() >= limit {
                    has_more = true;
                    break;
                }
                out.push(v);
            }
        }
        out.reverse();
        Ok((out, has_more))
    }

    fn latest_tail(&self, resource_key: &str) -> Result<Option<VersionRecord>> {
        let list_key = self.scoped_key(resource_key);
        let Some(g) = self.store.group(ns::GRAPH_NODE, &list_key) else {
            return Ok(None);
        };
        if g.live == 0 || g.next <= g.head {
            return Ok(None);
        }
        for seq in (g.head..g.next).rev() {
            let key = group_key(&list_key, &seq.to_be_bytes())?;
            if let Some((_, raw)) = self.store.get(ns::GRAPH_NODE, &key)? {
                return decode_json(&raw, "version record").map(Some);
            }
        }
        Ok(None)
    }

    fn resolve_forward(&self, start_key: &str) -> Result<String> {
        let mut curr = start_key.to_string();
        let mut seen = BTreeSet::new();
        seen.insert(curr.clone());
        for _ in 0..MAX_ALIAS_HOPS {
            let Some((_, raw)) = self
                .store
                .get(ns::GRAPH_FACT, &self.scoped_prefixed(PREFIX_ALIAS, &curr))?
            else {
                break;
            };
            let next = String::from_utf8(raw)
                .map_err(|e| Error::Corrupt(format!("graph alias utf8: {e}")))?;
            if !seen.insert(next.clone()) {
                break;
            }
            curr = next;
        }
        Ok(curr)
    }

    fn alias_component(&self, start_key: &str) -> Result<Vec<String>> {
        let mut visited = BTreeSet::new();
        let mut queue = VecDeque::new();
        visited.insert(start_key.to_string());
        queue.push_back(start_key.to_string());

        while let Some(key) = queue.pop_front() {
            if visited.len() >= MAX_REV_ALIASES * 2 {
                break;
            }
            if let Some((_, raw)) = self
                .store
                .get(ns::GRAPH_FACT, &self.scoped_prefixed(PREFIX_ALIAS, &key))?
            {
                let next = String::from_utf8(raw)
                    .map_err(|e| Error::Corrupt(format!("graph alias utf8: {e}")))?;
                if visited.insert(next.clone()) {
                    queue.push_back(next);
                }
            }
            if let Some((_, raw)) = self
                .store
                .get(ns::GRAPH_FACT, &self.scoped_prefixed(PREFIX_REV, &key))?
            {
                let revs: Vec<String> = decode_json(&raw, "rev aliases")?;
                for prev in revs {
                    if visited.insert(prev.clone()) {
                        queue.push_back(prev);
                    }
                }
            }
        }
        Ok(visited.into_iter().collect())
    }
}

fn encode_tenant_prefix(tenant: &str) -> String {
    let hash = blake3::hash(tenant.as_bytes());
    let mut out = String::with_capacity(35); // [协议] 固定前缀 "t:" + 32 位 hex + "\0" 共 35 字节
    out.push_str("t:");
    for b in &hash.as_bytes()[..16] {
        let _ = write!(out, "{b:02x}");
    }
    out.push('\0');
    out
}

fn scope_prefixed_key(scope: &str, prefix: &[u8], body: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(scope.len() + prefix.len() + body.len());
    out.extend_from_slice(scope.as_bytes());
    out.extend_from_slice(prefix);
    out.extend_from_slice(body.as_bytes());
    out
}

fn scope_list_key(scope: &str, key: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(scope.len() + key.len());
    out.extend_from_slice(scope.as_bytes());
    out.extend_from_slice(key.as_bytes());
    out
}

fn latest_tail_in_tx(
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

fn resolve_forward_in_tx(tx: &Tx<'_>, scope: &str, start_key: &str) -> Result<String> {
    let mut curr = start_key.to_string();
    let mut seen = BTreeSet::new();
    seen.insert(curr.clone());
    for _ in 0..MAX_ALIAS_HOPS {
        let Some((_, raw)) = tx.get(
            ns::GRAPH_FACT,
            &scope_prefixed_key(scope, PREFIX_ALIAS, &curr),
        )?
        else {
            break;
        };
        let next =
            String::from_utf8(raw).map_err(|e| Error::Corrupt(format!("graph alias utf8: {e}")))?;
        if !seen.insert(next.clone()) {
            break;
        }
        curr = next;
    }
    Ok(curr)
}

fn forward_reaches_in_tx(tx: &Tx<'_>, scope: &str, start_key: &str, needle: &str) -> Result<bool> {
    let mut curr = start_key.to_string();
    let mut seen = BTreeSet::new();
    seen.insert(curr.clone());
    for _ in 0..MAX_ALIAS_HOPS {
        if curr == needle {
            return Ok(true);
        }
        let Some((_, raw)) = tx.get(
            ns::GRAPH_FACT,
            &scope_prefixed_key(scope, PREFIX_ALIAS, &curr),
        )?
        else {
            break;
        };
        let next =
            String::from_utf8(raw).map_err(|e| Error::Corrupt(format!("graph alias utf8: {e}")))?;
        if !seen.insert(next.clone()) {
            break;
        }
        curr = next;
    }
    Ok(curr == needle)
}

fn bind_alias_in_tx(
    tx: &mut Tx<'_>,
    scope: &str,
    alias_key: &str,
    canonical_key: &str,
    now: u64,
) -> Result<()> {
    if alias_key.is_empty() || canonical_key.is_empty() || alias_key == canonical_key {
        return Ok(());
    }
    let target = resolve_forward_in_tx(tx, scope, canonical_key)?;
    if alias_key == target || forward_reaches_in_tx(tx, scope, &target, alias_key)? {
        return Ok(());
    }
    let alias_store_key = scope_prefixed_key(scope, PREFIX_ALIAS, alias_key);
    if let Some((_, old_raw)) = tx.get(ns::GRAPH_FACT, &alias_store_key)? {
        let old_target = String::from_utf8(old_raw)
            .map_err(|e| Error::Corrupt(format!("graph alias utf8: {e}")))?;
        if old_target != target {
            let old_rev_key = scope_prefixed_key(scope, PREFIX_REV, &old_target);
            if let Some((_, raw)) = tx.get(ns::GRAPH_FACT, &old_rev_key)? {
                let mut old_revs: Vec<String> = decode_json(&raw, "rev aliases")?;
                let prev_len = old_revs.len();
                old_revs.retain(|k| k != alias_key);
                if old_revs.len() != prev_len {
                    if old_revs.is_empty() {
                        tx.delete(ns::GRAPH_FACT, &old_rev_key)?;
                    } else {
                        let encoded = encode_json(&old_revs, "rev aliases")?;
                        tx.put(ns::GRAPH_FACT, &old_rev_key, &encoded, now)?;
                    }
                }
            }
        }
    }
    tx.put(ns::GRAPH_FACT, &alias_store_key, target.as_bytes(), now)?;
    let rev_key = scope_prefixed_key(scope, PREFIX_REV, &target);
    let mut revs: Vec<String> = match tx.get(ns::GRAPH_FACT, &rev_key)? {
        Some((_, raw)) => decode_json(&raw, "rev aliases")?,
        None => Vec::new(),
    };
    if !revs.iter().any(|k| k == alias_key) {
        if revs.len() >= MAX_REV_ALIASES {
            revs.remove(0);
        }
        revs.push(alias_key.to_string());
        let raw = encode_json(&revs, "rev aliases")?;
        tx.put(ns::GRAPH_FACT, &rev_key, &raw, now)?;
    }
    Ok(())
}

fn record_ts(record: &VersionRecord) -> u64 {
    if record.observed_at_ms > 0 {
        record.observed_at_ms
    } else {
        to_ms(SystemTime::now())
    }
}

fn initial_key(url_or_key: &str) -> String {
    if url_or_key.contains("://") {
        normalize_resource_key(url_or_key)
    } else {
        url_or_key.to_string()
    }
}

fn encode_json<T: Serialize>(value: &T, label: &str) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|e| Error::Corrupt(format!("encode {label}: {e}")))
}

fn decode_json<T: DeserializeOwned>(raw: &[u8], label: &str) -> Result<T> {
    serde_json::from_slice(raw).map_err(|e| Error::Corrupt(format!("decode {label}: {e}")))
}
