//! DataGraph persistent storage (`ns 9..=12`, never LRU-evicted by `Store::gc`).
//!
//! - `GRAPH_NODE` (`ns 9`): per-`resource_key` append list of [`VersionRecord`] entries.
//! - `GRAPH_EDGE` (`ns 10`): per-`resource_key` append list of [`VersionEdge`] transitions.
//! - `GRAPH_FACT` (`ns 11`): alias resolution (`alias\0`, `rev\0`) and [`ConflictRecord`] entries.
//! - `GRAPH_JOB` (`ns 12`): zstd-compressed job [`GraphSummary`] and [`PageGraphDelta`] records.

use std::collections::{BTreeSet, VecDeque};
use std::time::SystemTime;

use pagesift_graph::{
    ConflictRecord, GraphSummary, PageGraphDelta, VersionEdge, VersionRecord,
    normalize_resource_key,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::engine::{group_key, ns};
use crate::snapshot::compress_snapshot;
use crate::tasks::{list_push, list_scan};
use crate::{Error, Result, Store, to_ms};

const MAX_ALIAS_HOPS: usize = 16; // [策略] 规范 URL 别名解析最大跳数
const MAX_REV_ALIASES: usize = 128; // [策略] 反向别名列表读取上限
const MAX_GRAPH_BLOB: usize = 64 << 20; // [策略] DataGraph 单条压缩记录解压上限 64 MiB

const PREFIX_ALIAS: &[u8] = b"alias\0";
const PREFIX_REV: &[u8] = b"rev\0";
const PREFIX_CONFLICT: &[u8] = b"conflict\0";
const PREFIX_SUMMARY: &[u8] = b"summary\0";
const PREFIX_DELTA: &[u8] = b"delta\0";

/// Persistent DataGraph handle returned by [`Store::graph`].
pub struct GraphStore<'a> {
    store: &'a Store,
}

impl<'a> GraphStore<'a> {
    pub(crate) fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// Append a [`VersionRecord`] and optional incoming [`VersionEdge`] under `record.resource_key`.
    pub fn record_version(&self, record: &VersionRecord, edge: Option<&VersionEdge>) -> Result<()> {
        let rec_bytes = encode_json(record, "version record")?;
        let edge_bytes = edge.map(|e| encode_json(e, "version edge")).transpose()?;
        let ts = if record.observed_at_ms > 0 {
            record.observed_at_ms
        } else {
            to_ms(SystemTime::now())
        };
        let key = record.resource_key.as_bytes();
        let mut tx = self.store.tx();
        list_push(&mut tx, ns::GRAPH_NODE, key, &rec_bytes, ts)?;
        if let Some(bytes) = edge_bytes {
            let edge_key = edge.map_or(key, |e| e.resource_key.as_bytes());
            list_push(&mut tx, ns::GRAPH_EDGE, edge_key, &bytes, ts)?;
        }
        drop(tx);
        self.store.finish();
        Ok(())
    }

    /// Bind `alias_key -> canonical_key` so URL moves and canonical declarations share history.
    pub fn bind_alias(&self, alias_key: &str, canonical_key: &str) -> Result<()> {
        if alias_key.is_empty() || canonical_key.is_empty() || alias_key == canonical_key {
            return Ok(());
        }
        let target = self.resolve_forward(canonical_key)?;
        if alias_key == target || self.forward_reaches(&target, alias_key)? {
            return Ok(());
        }
        let now = to_ms(SystemTime::now());
        let mut tx = self.store.tx();
        tx.put(
            ns::GRAPH_FACT,
            &prefixed_key(PREFIX_ALIAS, alias_key),
            target.as_bytes(),
            now,
        )?;
        let rev_key = prefixed_key(PREFIX_REV, &target);
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

    /// Return `(canonical_resource_key, versions, edges)` across all linked aliases of `url_or_key`.
    pub fn history(
        &self,
        url_or_key: &str,
    ) -> Result<(String, Vec<VersionRecord>, Vec<VersionEdge>)> {
        let start = initial_key(url_or_key);
        let canonical = self.resolve_forward(&start)?;
        let component = self.alias_component(&start)?;

        let mut versions = Vec::new();
        let mut edges = Vec::new();
        for key in component {
            for (_, _, raw) in list_scan(self.store, ns::GRAPH_NODE, key.as_bytes(), usize::MAX)? {
                versions.push(decode_json::<VersionRecord>(&raw, "version record")?);
            }
            for (_, _, raw) in list_scan(self.store, ns::GRAPH_EDGE, key.as_bytes(), usize::MAX)? {
                edges.push(decode_json::<VersionEdge>(&raw, "version edge")?);
            }
        }
        versions.sort_by(|a, b| {
            a.observed_at_ms
                .cmp(&b.observed_at_ms)
                .then_with(|| a.doc_id.cmp(&b.doc_id))
        });
        versions.dedup_by(|a, b| a.observed_at_ms == b.observed_at_ms && a.doc_id == b.doc_id);

        edges.sort_by(|a, b| {
            a.curr_observed_at_ms
                .cmp(&b.curr_observed_at_ms)
                .then_with(|| a.curr_doc_id.cmp(&b.curr_doc_id))
        });
        edges.dedup_by(|a, b| {
            a.curr_observed_at_ms == b.curr_observed_at_ms && a.curr_doc_id == b.curr_doc_id
        });
        Ok((canonical, versions, edges))
    }

    /// Persist conflict records idempotently into `ns::GRAPH_FACT`.
    pub fn put_conflicts(&self, conflicts: &[ConflictRecord]) -> Result<()> {
        if conflicts.is_empty() {
            return Ok(());
        }
        let now = to_ms(SystemTime::now());
        let mut tx = self.store.tx();
        for conflict in conflicts {
            let key = conflict_key(&conflict.subject_key, &conflict.conflict_id);
            let val = encode_json(conflict, "conflict record")?;
            tx.put(ns::GRAPH_FACT, &key, &val, now)?;
        }
        drop(tx);
        self.store.finish();
        Ok(())
    }

    /// List stored conflicts, optionally filtered by `subject_key`.
    pub fn list_conflicts(&self, subject_key: Option<&str>) -> Result<Vec<ConflictRecord>> {
        let mut prefix = Vec::from(PREFIX_CONFLICT);
        if let Some(subj) = subject_key {
            prefix.extend_from_slice(subj.as_bytes());
            prefix.push(0);
        }
        let keys = self
            .store
            .keys(ns::GRAPH_FACT, |k| k.starts_with(&prefix))?;
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some((_, raw)) = self.store.get(ns::GRAPH_FACT, &key)? {
                out.push(decode_json(&raw, "conflict record")?);
            }
        }
        Ok(out)
    }

    /// Persist a zstd-compressed job [`GraphSummary`] under `ns::GRAPH_JOB`.
    pub fn put_job_summary(&self, job_id: &str, summary: &GraphSummary) -> Result<()> {
        let json = encode_json(summary, "graph summary")?;
        let compressed = compress_snapshot(&json, self.store.config().snapshot_compress_level);
        self.store
            .put(
                ns::GRAPH_JOB,
                &prefixed_key(PREFIX_SUMMARY, job_id),
                &compressed,
                to_ms(SystemTime::now()),
            )
            .map(drop)
    }

    /// Load and decompress a stored job [`GraphSummary`].
    pub fn get_job_summary(&self, job_id: &str) -> Result<Option<GraphSummary>> {
        let Some((_, raw)) = self
            .store
            .get(ns::GRAPH_JOB, &prefixed_key(PREFIX_SUMMARY, job_id))?
        else {
            return Ok(None);
        };
        let decompressed = pagesift_compress::decompress(&raw, MAX_GRAPH_BLOB)
            .map_err(|e| Error::Corrupt(format!("graph summary {job_id}: {e}")))?;
        decode_json(&decompressed, "graph summary").map(Some)
    }

    /// Persist one zstd-compressed [`PageGraphDelta`] belonging to `job_id`.
    pub fn append_job_delta(&self, job_id: &str, delta: &PageGraphDelta) -> Result<()> {
        let json = encode_json(delta, "page graph delta")?;
        let compressed = compress_snapshot(&json, self.store.config().snapshot_compress_level);
        let key = delta_key(job_id, &delta.node.doc_id);
        let ts = if delta.node.observed_at_ms > 0 {
            delta.node.observed_at_ms
        } else {
            to_ms(SystemTime::now())
        };
        self.store
            .put(ns::GRAPH_JOB, &key, &compressed, ts)
            .map(drop)
    }

    /// Read all stored [`PageGraphDelta`] entries for `job_id`, sorted deterministically.
    pub fn job_deltas(&self, job_id: &str) -> Result<Vec<PageGraphDelta>> {
        let prefix = delta_key(job_id, "");
        let keys = self.store.keys(ns::GRAPH_JOB, |k| k.starts_with(&prefix))?;
        let mut out: Vec<PageGraphDelta> = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some((_, raw)) = self.store.get(ns::GRAPH_JOB, &key)? {
                let decompressed = pagesift_compress::decompress(&raw, MAX_GRAPH_BLOB)
                    .map_err(|e| Error::Corrupt(format!("graph delta {job_id}: {e}")))?;
                out.push(decode_json(&decompressed, "page graph delta")?);
            }
        }
        out.sort_by(|a, b| {
            a.node
                .observed_at_ms
                .cmp(&b.node.observed_at_ms)
                .then_with(|| a.node.doc_id.cmp(&b.node.doc_id))
        });
        Ok(out)
    }

    fn latest_tail(&self, resource_key: &str) -> Result<Option<VersionRecord>> {
        let list_key = resource_key.as_bytes();
        let Some(g) = self.store.group(ns::GRAPH_NODE, list_key) else {
            return Ok(None);
        };
        if g.live == 0 || g.next <= g.head {
            return Ok(None);
        }
        for seq in (g.head..g.next).rev() {
            let key = group_key(list_key, &seq.to_be_bytes())?;
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
                .get(ns::GRAPH_FACT, &prefixed_key(PREFIX_ALIAS, &curr))?
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

    fn forward_reaches(&self, start_key: &str, needle: &str) -> Result<bool> {
        let mut curr = start_key.to_string();
        let mut seen = BTreeSet::new();
        seen.insert(curr.clone());
        for _ in 0..MAX_ALIAS_HOPS {
            if curr == needle {
                return Ok(true);
            }
            let Some((_, raw)) = self
                .store
                .get(ns::GRAPH_FACT, &prefixed_key(PREFIX_ALIAS, &curr))?
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
        Ok(curr == needle)
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
                .get(ns::GRAPH_FACT, &prefixed_key(PREFIX_ALIAS, &key))?
            {
                let next = String::from_utf8(raw)
                    .map_err(|e| Error::Corrupt(format!("graph alias utf8: {e}")))?;
                if visited.insert(next.clone()) {
                    queue.push_back(next);
                }
            }
            if let Some((_, raw)) = self
                .store
                .get(ns::GRAPH_FACT, &prefixed_key(PREFIX_REV, &key))?
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

fn initial_key(url_or_key: &str) -> String {
    if url_or_key.contains("://") {
        normalize_resource_key(url_or_key)
    } else {
        url_or_key.to_string()
    }
}

fn prefixed_key(prefix: &[u8], body: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(prefix.len() + body.len());
    out.extend_from_slice(prefix);
    out.extend_from_slice(body.as_bytes());
    out
}

fn conflict_key(subject_key: &str, conflict_id: &str) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(PREFIX_CONFLICT.len() + subject_key.len() + conflict_id.len() + 1);
    out.extend_from_slice(PREFIX_CONFLICT);
    out.extend_from_slice(subject_key.as_bytes());
    out.push(0);
    out.extend_from_slice(conflict_id.as_bytes());
    out
}

fn delta_key(job_id: &str, doc_id: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(PREFIX_DELTA.len() + job_id.len() + doc_id.len() + 1);
    out.extend_from_slice(PREFIX_DELTA);
    out.extend_from_slice(job_id.as_bytes());
    out.push(0);
    out.extend_from_slice(doc_id.as_bytes());
    out
}

fn encode_json<T: Serialize>(value: &T, label: &str) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|e| Error::Corrupt(format!("encode {label}: {e}")))
}

fn decode_json<T: DeserializeOwned>(raw: &[u8], label: &str) -> Result<T> {
    serde_json::from_slice(raw).map_err(|e| Error::Corrupt(format!("decode {label}: {e}")))
}
