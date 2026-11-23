//! Persistent 1.7 `ResolveStore` (`ns::RESOLVE = 15`): atomic conclusion commits, immutable
//! revision history, reverse dependency & scope/negative-dependency indexes, ABA-safe ContextDelta
//! consumer watermarks, bounded event ring, and GC-immune tenant isolation.
//!
//! All keys carry a single-letter domain prefix (`r<tag>\x1f...`) under `t:{tenant}|` so future
//! versions can extend cleanly without colliding with Resolve state or altering `PSSTORE1` segments.

mod index_ops;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use pagesift_graph::resolve::{
    ConclusionRecord, ConclusionRevision, CorrectionRecord, ResolutionState, ResolveEvent,
    ResolveEventKind, detect_dependency_cycle,
};
use serde::{Deserialize, Serialize};

use crate::engine::{AtomicTx, ns};
use crate::snapshot::compress_snapshot;
use crate::sync::now_store_ms;
use crate::{Error, Result, Store};

// [策略] 单租户最多持久化 4096 条活跃结论，防止反向索引与元数据头无界增长
pub const MAX_CONCLUSIONS_PER_TENANT: usize = 4096;
// [策略] 单条结论最多保留最近 128 个不可变历史版本快照供点时间回放与审计
pub const MAX_CONCLUSION_REVISIONS: usize = 128;
// [策略] 单租户有界结论生命周期事件环最多保留最近 512 条事件（支持幂等消费）
pub const MAX_RESOLVE_EVENTS: usize = 512;
// [策略] 单个压缩前后 Resolve 记录负载上限 16 MiB（单结论远小于全库索引，防止恶意超大 payload）
const MAX_RESOLVE_BLOB: usize = 16 << 20;
// [策略] Resolve 逻辑标识符最大字节长度上限 256（与 ContextStore 一致）
const MAX_RESOLVE_IDENT_LEN: usize = 256;
// [协议] ASCII Unit Separator (0x1F) 隔离复合键段，防止标识符含 ':' 造成跨键碰撞
pub(crate) const SEP: char = '\x1f';

/// Compact per-tenant header enabling O(K) conclusion enumeration without full-namespace scan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveStoreMeta {
    pub conclusion_ids: BTreeSet<String>,
    pub latest_event_seq: u64,
    pub earliest_event_seq: u64,
    pub updated_at_ms: u64,
}

/// ABA-safe watermark tracking which `(instance_id, delta_seq)` of a Context source Resolve has processed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveSourceCheckpoint {
    pub source_id: String,
    pub instance_id: String,
    pub consumed_delta_seq: u64,
    pub updated_at_ms: u64,
}

/// Outcome of an atomic [`ResolveStore::commit_conclusion`] transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveCommitOutcome {
    pub conclusion_id: String,
    pub version: u64,
    pub event_seq: u64,
    pub created: bool,
}

/// Tenant-scoped persistent handle over `ns::RESOLVE`.
pub struct ResolveStore<'a> {
    pub(crate) store: &'a Store,
    pub(crate) tenant: Option<&'a str>,
}

impl<'a> ResolveStore<'a> {
    pub(crate) fn new(store: &'a Store, tenant: Option<&'a str>) -> Self {
        Self { store, tenant }
    }

    pub(crate) fn key(&self, tag: &str, rest: &str) -> Vec<u8> {
        match self.tenant.filter(|t| !t.is_empty()) {
            Some(t) => format!("t:{t}|{tag}{SEP}{rest}").into_bytes(),
            None => format!("{tag}{SEP}{rest}").into_bytes(),
        }
    }

    pub(crate) fn meta_key(&self) -> Vec<u8> {
        self.key("ridx", "meta")
    }

    pub(crate) fn conclusion_key(&self, conclusion_id: &str) -> Vec<u8> {
        self.key("rcon", conclusion_id)
    }

    pub(crate) fn revision_key(&self, conclusion_id: &str, version: u64) -> Vec<u8> {
        self.key("rver", &format!("{conclusion_id}{SEP}{version:020}"))
    }

    pub(crate) fn dep_unit_key(&self, unit_id: &str) -> Vec<u8> {
        self.key("rdep_u", unit_id)
    }

    pub(crate) fn dep_resource_key(&self, resource_key: &str) -> Vec<u8> {
        self.key("rdep_r", resource_key)
    }

    pub(crate) fn dep_source_key(&self, source_id: &str) -> Vec<u8> {
        self.key("rdep_s", source_id)
    }

    pub(crate) fn dep_subject_key(&self, subject_key: &str) -> Vec<u8> {
        self.key("rdep_k", subject_key)
    }

    pub(crate) fn dep_conclusion_key(&self, parent_id: &str) -> Vec<u8> {
        self.key("rdep_c", parent_id)
    }

    pub(crate) fn scope_key(&self, predicate: &str) -> Vec<u8> {
        self.key("rscp", predicate)
    }

    pub(crate) fn checkpoint_key(&self, source_id: &str) -> Vec<u8> {
        self.key("rchk", source_id)
    }

    pub(crate) fn event_key(&self, seq: u64) -> Vec<u8> {
        self.key("revt", &format!("{seq:020}"))
    }

    pub(crate) fn learn_key(&self, scope_id: &str) -> Vec<u8> {
        self.key("rlrn", scope_id)
    }

    /// Read current tenant [`ResolveStoreMeta`].
    pub fn get_meta(&self) -> Result<ResolveStoreMeta> {
        let Some((_, raw)) = self.store.get(ns::RESOLVE, &self.meta_key())? else {
            return Ok(ResolveStoreMeta::default());
        };
        decode_json(&raw, "resolve store meta")
    }

    /// Fetch latest authoritative [`ConclusionRecord`] for `conclusion_id`.
    pub fn get_conclusion(&self, conclusion_id: &str) -> Result<Option<ConclusionRecord>> {
        validate_ident(conclusion_id, "conclusion_id")?;
        let Some((_, raw)) = self
            .store
            .get(ns::RESOLVE, &self.conclusion_key(conclusion_id))?
        else {
            return Ok(None);
        };
        decode_zstd_json(&raw, "resolve conclusion").map(Some)
    }

    /// Fetch immutable historical revision `version` of `conclusion_id`.
    pub fn get_revision(
        &self,
        conclusion_id: &str,
        version: u64,
    ) -> Result<Option<ConclusionRevision>> {
        validate_ident(conclusion_id, "conclusion_id")?;
        let Some((_, raw)) = self
            .store
            .get(ns::RESOLVE, &self.revision_key(conclusion_id, version))?
        else {
            return Ok(None);
        };
        decode_zstd_json(&raw, "resolve conclusion revision").map(Some)
    }

    /// List immutable revision history for `conclusion_id` (newest first, up to `limit`).
    pub fn list_history(
        &self,
        conclusion_id: &str,
        limit: usize,
    ) -> Result<Vec<ConclusionRevision>> {
        validate_ident(conclusion_id, "conclusion_id")?;
        let Some(curr) = self.get_conclusion(conclusion_id)? else {
            return Ok(Vec::new());
        };
        let cap = limit.max(1);
        let min_v = curr
            .version
            .saturating_sub(MAX_CONCLUSION_REVISIONS as u64)
            .saturating_add(1)
            .max(1);
        let mut out = Vec::with_capacity(cap.min(MAX_CONCLUSION_REVISIONS));
        for v in (min_v..=curr.version).rev() {
            if let Some(rev) = self.get_revision(conclusion_id, v)? {
                out.push(rev);
                if out.len() >= cap {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Point-in-time historical lookup: returns the latest revision with `checked_at_ms <= as_of_ms`.
    /// Never substitutes a newer future revision when no historical revision exists.
    pub fn get_conclusion_as_of(
        &self,
        conclusion_id: &str,
        as_of_ms: u64,
    ) -> Result<Option<ConclusionRevision>> {
        validate_ident(conclusion_id, "conclusion_id")?;
        let history = self.list_history(conclusion_id, MAX_CONCLUSION_REVISIONS)?;
        Ok(history
            .into_iter()
            .find(|rev| rev.checked_at_ms <= as_of_ms))
    }

    /// List live conclusions visible to this tenant, optionally filtered by `state` or `source_id`.
    pub fn list_conclusions(
        &self,
        state_filter: Option<ResolutionState>,
        source_filter: Option<&str>,
        limit: usize,
    ) -> Result<(usize, Vec<ConclusionRecord>)> {
        let meta = self.get_meta()?;
        let cap = limit.max(1);
        let candidate_ids: Vec<String> = if let Some(sid) = source_filter {
            validate_ident(sid, "source_id")?;
            self.load_id_set(&self.dep_source_key(sid))?
                .into_iter()
                .collect()
        } else {
            meta.conclusion_ids.into_iter().collect()
        };

        let mut total = 0usize;
        let mut out = Vec::new();
        for cid in candidate_ids {
            if let Some(rec) = self.get_conclusion(&cid)? {
                if state_filter.is_some_and(|st| rec.state != st) {
                    continue;
                }
                total += 1;
                if out.len() < cap {
                    out.push(rec);
                }
            }
        }
        Ok((total, out))
    }

    /// Atomically commit a new or updated [`ConclusionRecord`], preserving immutable revision history,
    /// maintaining all reverse dependency & scope indexes, rejecting dependency cycles, and appending
    /// a monotonic [`ResolveEvent`].
    pub fn commit_conclusion(
        &self,
        mut record: ConclusionRecord,
        event_kind_override: Option<ResolveEventKind>,
        reason: &str,
    ) -> Result<ResolveCommitOutcome> {
        validate_ident(&record.conclusion_id, "conclusion_id")?;
        for pid in &record.parent_conclusion_ids {
            validate_ident(pid, "parent_conclusion_id")?;
        }
        let now = now_store_ms();
        let lvl = self.store.config().snapshot_compress_level;

        let mut tx = self.store.atomic_tx();
        let mut meta = self.read_meta_in_tx(&tx)?;
        let prev_opt = self.read_conclusion_in_tx(&tx, &record.conclusion_id)?;

        if prev_opt.is_none() && meta.conclusion_ids.len() >= MAX_CONCLUSIONS_PER_TENANT {
            return Err(Error::ValueTooLarge {
                size: meta.conclusion_ids.len() + 1,
                limit: MAX_CONCLUSIONS_PER_TENANT,
            });
        }

        // Enforce DAG invariant on conclusion-to-conclusion edges (prevents self-citation loops!).
        if !record.parent_conclusion_ids.is_empty() {
            let mut cycle_io_err: Option<Error> = None;
            let cycle = detect_dependency_cycle(
                &record.conclusion_id,
                &record.parent_conclusion_ids,
                |pid| match self.read_conclusion_in_tx(&tx, pid) {
                    Ok(Some(c)) => c.parent_conclusion_ids,
                    Ok(None) => Vec::new(),
                    Err(e) => {
                        cycle_io_err = Some(e);
                        Vec::new()
                    }
                },
            );
            if let Some(e) = cycle_io_err {
                return Err(e);
            }
            if let Some(path) = cycle {
                return Err(Error::Corrupt(format!(
                    "resolve_cycle_detected: {}",
                    path.join(" -> ")
                )));
            }
        }

        let (created, next_version, from_state) = match &prev_opt {
            Some(prev) => {
                let next_v = prev.version.saturating_add(1);
                record.created_at_ms = prev.created_at_ms;
                record.version = next_v;
                record.updated_at_ms = now;
                if record.checked_at_ms == 0 {
                    record.checked_at_ms = now;
                }
                let mut corrections = prev.corrections.clone();
                if prev.state != record.state {
                    corrections.push(CorrectionRecord {
                        from_version: prev.version,
                        to_version: next_v,
                        from_state: prev.state,
                        to_state: record.state,
                        reason: reason.to_owned(),
                        triggering_unit_ids: Vec::new(),
                        triggering_delta_seqs: Vec::new(),
                        at_ms: now,
                    });
                    if corrections.len() > MAX_CONCLUSION_REVISIONS {
                        let drop_n = corrections.len() - MAX_CONCLUSION_REVISIONS;
                        corrections.drain(0..drop_n);
                    }
                }
                record.corrections = corrections;
                (false, next_v, Some(prev.state))
            }
            None => {
                record.version = 1;
                if record.created_at_ms == 0 {
                    record.created_at_ms = now;
                }
                record.updated_at_ms = now;
                if record.checked_at_ms == 0 {
                    record.checked_at_ms = now;
                }
                (true, 1u64, None)
            }
        };

        let rev = record.to_revision();
        let rec_bytes = encode_zstd_json(&record, lvl, "resolve conclusion")?;
        let rev_bytes = encode_zstd_json(&rev, lvl, "resolve conclusion revision")?;

        // Update reverse dependency & scope indexes atomically in the same transaction.
        self.update_indexes_in_tx(&mut tx, prev_opt.as_ref(), Some(&record), now)?;

        tx.put(
            ns::RESOLVE,
            &self.conclusion_key(&record.conclusion_id),
            &rec_bytes,
            now,
        )?;
        tx.put(
            ns::RESOLVE,
            &self.revision_key(&record.conclusion_id, next_version),
            &rev_bytes,
            now,
        )?;

        if next_version > MAX_CONCLUSION_REVISIONS as u64 {
            let old_v = next_version - MAX_CONCLUSION_REVISIONS as u64;
            tx.delete(
                ns::RESOLVE,
                &self.revision_key(&record.conclusion_id, old_v),
            )?;
        }

        meta.conclusion_ids.insert(record.conclusion_id.clone());
        let event_kind = event_kind_override.unwrap_or(if created {
            ResolveEventKind::ConclusionCreated
        } else if from_state.is_some_and(|fs| fs != record.state) {
            ResolveEventKind::ConclusionCorrected
        } else {
            ResolveEventKind::ConclusionRevalidated
        });
        let event_seq = self.append_event_in_tx(
            &mut tx,
            &mut meta,
            ResolveEvent {
                event_seq: 0,
                kind: event_kind,
                conclusion_id: record.conclusion_id.clone(),
                version: next_version,
                from_state,
                to_state: record.state,
                reason: reason.to_owned(),
                at_ms: now,
            },
            now,
        )?;

        meta.updated_at_ms = now;
        tx.put(
            ns::RESOLVE,
            &self.meta_key(),
            &encode_json(&meta, "resolve store meta")?,
            now,
        )?;
        tx.commit()?;

        Ok(ResolveCommitOutcome {
            conclusion_id: record.conclusion_id,
            version: next_version,
            event_seq,
            created,
        })
    }

    /// Mark an existing conclusion `StalePendingRevalidation` without erasing its prior evidence or history.
    pub fn mark_stale(
        &self,
        conclusion_id: &str,
        reason: &str,
        event_kind: ResolveEventKind,
        triggering_unit_ids: Vec<String>,
        triggering_delta_seqs: Vec<u64>,
    ) -> Result<Option<ConclusionRecord>> {
        validate_ident(conclusion_id, "conclusion_id")?;
        let Some(mut rec) = self.get_conclusion(conclusion_id)? else {
            return Ok(None);
        };
        let prev_state = rec.state;
        let now = now_store_ms();
        rec.state = ResolutionState::StalePendingRevalidation;
        rec.stale_reason = Some(reason.to_owned());
        if prev_state != ResolutionState::StalePendingRevalidation {
            rec.corrections.push(CorrectionRecord {
                from_version: rec.version,
                to_version: rec.version.saturating_add(1),
                from_state: prev_state,
                to_state: ResolutionState::StalePendingRevalidation,
                reason: reason.to_owned(),
                triggering_unit_ids,
                triggering_delta_seqs,
                at_ms: now,
            });
        }
        let outcome = self.commit_conclusion(rec.clone(), Some(event_kind), reason)?;
        self.get_conclusion(&outcome.conclusion_id)
    }

    /// Atomically delete a conclusion, its revision snapshots, and all reverse dependency edges.
    pub fn delete_conclusion(&self, conclusion_id: &str, reason: &str) -> Result<bool> {
        validate_ident(conclusion_id, "conclusion_id")?;
        let now = now_store_ms();
        let mut tx = self.store.atomic_tx();
        let Some(prev) = self.read_conclusion_in_tx(&tx, conclusion_id)? else {
            return Ok(false);
        };
        let mut meta = self.read_meta_in_tx(&tx)?;
        self.update_indexes_in_tx(&mut tx, Some(&prev), None, now)?;
        tx.delete(ns::RESOLVE, &self.conclusion_key(conclusion_id))?;
        let min_v = prev
            .version
            .saturating_sub(MAX_CONCLUSION_REVISIONS as u64)
            .saturating_add(1)
            .max(1);
        for v in min_v..=prev.version {
            tx.delete(ns::RESOLVE, &self.revision_key(conclusion_id, v))?;
        }
        meta.conclusion_ids.remove(conclusion_id);
        let _ = self.append_event_in_tx(
            &mut tx,
            &mut meta,
            ResolveEvent {
                event_seq: 0,
                kind: ResolveEventKind::ConclusionDeleted,
                conclusion_id: conclusion_id.to_owned(),
                version: prev.version,
                from_state: Some(prev.state),
                to_state: prev.state,
                reason: reason.to_owned(),
                at_ms: now,
            },
            now,
        )?;
        meta.updated_at_ms = now;
        tx.put(
            ns::RESOLVE,
            &self.meta_key(),
            &encode_json(&meta, "resolve store meta")?,
            now,
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Read or persist ABA-safe ContextDelta consumption checkpoint for `source_id`.
    pub fn get_source_checkpoint(
        &self,
        source_id: &str,
    ) -> Result<Option<ResolveSourceCheckpoint>> {
        validate_ident(source_id, "source_id")?;
        let Some((_, raw)) = self
            .store
            .get(ns::RESOLVE, &self.checkpoint_key(source_id))?
        else {
            return Ok(None);
        };
        decode_json(&raw, "resolve source checkpoint").map(Some)
    }

    pub fn put_source_checkpoint(&self, mut cp: ResolveSourceCheckpoint) -> Result<()> {
        validate_ident(&cp.source_id, "source_id")?;
        validate_ident(&cp.instance_id, "instance_id")?;
        let now = now_store_ms();
        cp.updated_at_ms = now;
        let bytes = encode_json(&cp, "resolve source checkpoint")?;
        self.store
            .put(
                ns::RESOLVE,
                &self.checkpoint_key(&cp.source_id),
                &bytes,
                now,
            )
            .map(drop)
    }

    /// List conclusion lifecycle events strictly after `after_seq` (forward-compatible with 1.8 Mesh.Events).
    pub fn list_events(&self, after_seq: Option<u64>, limit: usize) -> Result<Vec<ResolveEvent>> {
        let meta = self.get_meta()?;
        if meta.latest_event_seq == 0 {
            return Ok(Vec::new());
        }
        let cap = limit.max(1);
        let start = match after_seq {
            Some(s) => s.saturating_add(1).max(meta.earliest_event_seq.max(1)),
            None => meta.earliest_event_seq.max(1),
        };
        if start > meta.latest_event_seq {
            return Ok(Vec::new());
        }
        let mut out =
            Vec::with_capacity((meta.latest_event_seq - start + 1).min(cap as u64) as usize);
        for seq in start..=meta.latest_event_seq {
            if let Some((_, raw)) = self.store.get(ns::RESOLVE, &self.event_key(seq))? {
                out.push(decode_json(&raw, "resolve event")?);
                if out.len() >= cap {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Persist or load tenant-isolated `ResolveLearnState` JSON bytes (zstd-compressed).
    pub fn put_learn_bytes(&self, scope_id: &str, json_bytes: &[u8]) -> Result<()> {
        validate_ident(scope_id, "scope_id")?;
        if json_bytes.len() > MAX_RESOLVE_BLOB {
            return Err(Error::ValueTooLarge {
                size: json_bytes.len(),
                limit: MAX_RESOLVE_BLOB,
            });
        }
        let compressed = compress_snapshot(json_bytes, self.store.config().snapshot_compress_level);
        self.store
            .put(
                ns::RESOLVE,
                &self.learn_key(scope_id),
                &compressed,
                now_store_ms(),
            )
            .map(drop)
    }

    pub fn load_learn_bytes(&self, scope_id: &str) -> Result<Option<Vec<u8>>> {
        validate_ident(scope_id, "scope_id")?;
        let Some((_, raw)) = self.store.get(ns::RESOLVE, &self.learn_key(scope_id))? else {
            return Ok(None);
        };
        let dec = pagesift_compress::decompress(&raw, MAX_RESOLVE_BLOB)
            .map_err(|e| Error::Corrupt(format!("resolve learn {scope_id}: {e}")))?;
        Ok(Some(dec))
    }

    pub(crate) fn read_meta_in_tx(&self, tx: &AtomicTx<'_>) -> Result<ResolveStoreMeta> {
        let Some((_, raw)) = tx.get(ns::RESOLVE, &self.meta_key())? else {
            return Ok(ResolveStoreMeta::default());
        };
        decode_json(&raw, "resolve store meta")
    }

    pub(crate) fn read_conclusion_in_tx(
        &self,
        tx: &AtomicTx<'_>,
        conclusion_id: &str,
    ) -> Result<Option<ConclusionRecord>> {
        let Some((_, raw)) = tx.get(ns::RESOLVE, &self.conclusion_key(conclusion_id))? else {
            return Ok(None);
        };
        decode_zstd_json(&raw, "resolve conclusion").map(Some)
    }

    fn append_event_in_tx(
        &self,
        tx: &mut AtomicTx<'_>,
        meta: &mut ResolveStoreMeta,
        mut ev: ResolveEvent,
        now: u64,
    ) -> Result<u64> {
        let next_seq = meta.latest_event_seq.saturating_add(1);
        ev.event_seq = next_seq;
        let bytes = encode_json(&ev, "resolve event")?;
        tx.put(ns::RESOLVE, &self.event_key(next_seq), &bytes, now)?;
        if meta.earliest_event_seq == 0 {
            meta.earliest_event_seq = next_seq;
        }
        meta.latest_event_seq = next_seq;
        let retained = meta
            .latest_event_seq
            .saturating_sub(meta.earliest_event_seq)
            .saturating_add(1);
        if retained > MAX_RESOLVE_EVENTS as u64 {
            let target_earliest = meta.latest_event_seq + 1 - MAX_RESOLVE_EVENTS as u64;
            for old_seq in meta.earliest_event_seq..target_earliest {
                tx.delete(ns::RESOLVE, &self.event_key(old_seq))?;
            }
            meta.earliest_event_seq = target_earliest;
        }
        Ok(next_seq)
    }
}

pub(crate) fn validate_ident(raw: &str, field: &str) -> Result<()> {
    if raw.is_empty() || raw.len() > MAX_RESOLVE_IDENT_LEN {
        return Err(Error::Corrupt(format!(
            "invalid {field}: length {} out of bounds (1..={MAX_RESOLVE_IDENT_LEN})",
            raw.len()
        )));
    }
    // [协议] US-ASCII (RFC 20)：0x00..0x1F 与 0x7F 为控制字符，禁止进入 Resolve 复合键标识符
    if raw
        .bytes()
        .any(|b| b == SEP as u8 || b == b'|' || b < 0x20 || b == 0x7f)
    {
        return Err(Error::Corrupt(format!(
            "invalid {field}: contains forbidden delimiter or control byte"
        )));
    }
    Ok(())
}

pub(crate) fn encode_json<T: Serialize>(val: &T, label: &str) -> Result<Vec<u8>> {
    let bytes =
        serde_json::to_vec(val).map_err(|e| Error::Corrupt(format!("encode {label}: {e}")))?;
    if bytes.len() > MAX_RESOLVE_BLOB {
        return Err(Error::ValueTooLarge {
            size: bytes.len(),
            limit: MAX_RESOLVE_BLOB,
        });
    }
    Ok(bytes)
}

pub(crate) fn decode_json<T: for<'de> Deserialize<'de>>(raw: &[u8], label: &str) -> Result<T> {
    serde_json::from_slice(raw).map_err(|e| Error::Corrupt(format!("decode {label}: {e}")))
}

pub(crate) fn encode_zstd_json<T: Serialize>(val: &T, lvl: i32, label: &str) -> Result<Vec<u8>> {
    let raw = encode_json(val, label)?;
    let comp = compress_snapshot(&raw, lvl);
    if comp.len() > MAX_RESOLVE_BLOB {
        return Err(Error::ValueTooLarge {
            size: comp.len(),
            limit: MAX_RESOLVE_BLOB,
        });
    }
    Ok(comp)
}

pub(crate) fn decode_zstd_json<T: for<'de> Deserialize<'de>>(raw: &[u8], label: &str) -> Result<T> {
    let dec = pagesift_compress::decompress(raw, MAX_RESOLVE_BLOB)
        .map_err(|e| Error::Corrupt(format!("decompress {label}: {e}")))?;
    decode_json(&dec, label)
}
