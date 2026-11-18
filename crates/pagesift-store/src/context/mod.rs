//! Persistent 1.6 `ContextStore` (`ns::CONTEXT = 14`): atomic unit/index commits, ABA-safe
//! instance isolation, Store-to-Index lag visibility, O(1) key-indexed ring maintenance,
//! and GC-immune unit storage.

mod checkpoint;
mod history;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use pagesift_graph::context::{AssetSpecStamp, ContextDelta, IndexVisibility, KnowledgeUnit};
use serde::{Deserialize, Serialize};

use crate::engine::{AtomicTx, ns};
use crate::snapshot::compress_snapshot;
use crate::sync::{DataSourceSpec, SourceMeta, now_store_ms};
use crate::{Error, Result, Store};

// [策略] 每个数据源最多保留最近 128 条 ContextDelta 供增量查询与外部索引断线重放
pub const MAX_CONTEXT_DELTAS_PER_SOURCE: usize = 128;
// [策略] 每个数据源最多保留最近 256 条已撤回/被拆合取代的历史单元供谱系审计
pub const MAX_RETIRED_UNITS_PER_SOURCE: usize = 256;
// [策略] 单个压缩前后的 Context 状态/索引负载上限 64 MiB（与 GraphStore MAX_GRAPH_BLOB 对称一致）
const MAX_CONTEXT_BLOB: usize = 64 << 20;
// [策略] Context 逻辑标识符（source_id / consumer_id / scope_id）最大字节长度上限
const MAX_CONTEXT_IDENT_LEN: usize = 256;
// [协议] ASCII Unit Separator (0x1F)：隔离复合键各段，杜绝用户 ID 含 ':' 造成跨源键碰撞
const SEP: char = '\x1f';

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncGenProbe {
    #[serde(default)]
    active_instance_id: Option<String>,
    #[serde(default)]
    retired_instance_ids: Vec<String>,
}

/// Persistent metadata header tracking which `SyncStore` instance & tick a Context index reflects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSourceMeta {
    pub source_id: String,
    pub instance_id: String,
    pub indexed_tick_seq: u64,
    pub latest_delta_seq: u64,
    pub earliest_retained_delta_seq: u64,
    pub spec_stamp: AssetSpecStamp,
    pub updated_at_ms: u64,
    /// Earliest retained retired-unit sequence number in `cret` ring (`retired_seq_start..retired_seq_end`).
    #[serde(default)]
    pub retired_seq_start: u64,
    /// Exclusive upper bound of retired-unit sequence numbers in `cret` ring.
    #[serde(default)]
    pub retired_seq_end: u64,
    /// Active `resource_key`s currently stored under this source for O(K) direct lookup without full-ns scan.
    #[serde(default)]
    pub resource_keys: BTreeSet<String>,
    /// All `resource_key`s with point-in-time `chis` history entries (including withdrawn resources) for O(K) `as_of_ms` replay & purge.
    #[serde(default)]
    pub history_resource_keys: BTreeSet<String>,
}

/// Atomic commit request updating a source's units, serialized index blob, and [`ContextDelta`].
pub struct ContextCommitRequest<'a> {
    pub source_id: &'a str,
    pub instance_id: &'a str,
    pub store_tick_seq: u64,
    pub spec_stamp: AssetSpecStamp,
    /// Per-resource updated live units (`empty Vec` deletes the resource's live unit set).
    pub resource_units: &'a BTreeMap<String, Vec<KnowledgeUnit>>,
    pub retired_units: &'a [KnowledgeUnit],
    pub index_blob: Option<&'a [u8]>,
    pub delta: ContextDelta,
    /// When true, allows multiple ad-hoc manual ingests even when `store_tick_seq` does not advance.
    pub allow_same_zero_tick: bool,
}

/// Outcome of an atomic [`ContextStore::commit_delta`] transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextCommitOutcome {
    Committed { delta_seq: u64 },
    InstanceReplacedAndCommitted { delta_seq: u64 },
    IdempotentReplaySkipped { existing_delta_seq: u64 },
}

/// Tenant-scoped persistent handle over `ns::CONTEXT`.
pub struct ContextStore<'a> {
    pub(crate) store: &'a Store,
    pub(crate) tenant: Option<&'a str>,
}

impl<'a> ContextStore<'a> {
    pub(crate) fn new(store: &'a Store, tenant: Option<&'a str>) -> Self {
        Self { store, tenant }
    }

    pub(crate) fn sync_key(&self, prefix: &str, rest: &str) -> Vec<u8> {
        match self.tenant.filter(|t| !t.is_empty()) {
            Some(t) => format!("t:{t}|{prefix}{rest}").into_bytes(),
            None => format!("{prefix}{rest}").into_bytes(),
        }
    }

    pub(crate) fn prefix_bytes(&self, prefix: &str) -> Vec<u8> {
        match self.tenant.filter(|t| !t.is_empty()) {
            Some(t) => format!("t:{t}|{prefix}").into_bytes(),
            None => prefix.as_bytes().to_vec(),
        }
    }

    pub(crate) fn meta_key(&self, source_id: &str) -> Vec<u8> {
        self.sync_key(&format!("cmeta{SEP}"), source_id)
    }

    pub(crate) fn index_key(&self, source_id: &str) -> Vec<u8> {
        self.sync_key(&format!("cidx{SEP}"), source_id)
    }

    pub(crate) fn learn_key(&self, scope_id: &str) -> Vec<u8> {
        self.sync_key(&format!("clrn{SEP}"), scope_id)
    }

    pub(crate) fn res_key(&self, source_id: &str, resource_key: &str) -> Vec<u8> {
        self.sync_key(&format!("cres{SEP}{source_id}{SEP}"), resource_key)
    }

    pub(crate) fn retired_key(&self, source_id: &str, seq: u64) -> Vec<u8> {
        self.sync_key(&format!("cret{SEP}{source_id}{SEP}"), &format!("{seq:020}"))
    }

    pub(crate) fn delta_key(&self, source_id: &str, seq: u64) -> Vec<u8> {
        self.sync_key(&format!("cdlt{SEP}{source_id}{SEP}"), &format!("{seq:020}"))
    }

    pub(crate) fn checkpoint_key(&self, consumer_id: &str, source_id: &str) -> Vec<u8> {
        self.sync_key(&format!("cckp{SEP}{consumer_id}{SEP}"), source_id)
    }

    /// O(1) in-memory hash probe checking whether `source_id` has activated Context (zero disk pread on miss).
    pub fn has_meta(&self, source_id: &str) -> Result<bool> {
        validate_ident(source_id, "source_id")?;
        self.store.contains(ns::CONTEXT, &self.meta_key(source_id))
    }

    /// O(1) in-memory hash probe checking whether `source_id` is a managed `SyncStore` Loop source.
    pub fn has_sync_source(&self, source_id: &str) -> Result<bool> {
        validate_ident(source_id, "source_id")?;
        self.store
            .contains(ns::SYNC_STATE, &self.sync_key("src:", source_id))
    }

    /// Read current [`ContextSourceMeta`] for `source_id`, if indexed.
    pub fn get_meta(&self, source_id: &str) -> Result<Option<ContextSourceMeta>> {
        validate_ident(source_id, "source_id")?;
        let Some((_, raw)) = self.store.get(ns::CONTEXT, &self.meta_key(source_id))? else {
            return Ok(None);
        };
        decode_json(&raw, "context source meta").map(Some)
    }

    /// Inspect `SyncStore` spec & generation state to detect ABA recreation or deleted Sync sources.
    pub(crate) fn inspect_sync_lineage(
        &self,
        source_id: &str,
        ctx_instance_id: &str,
    ) -> Result<(Option<DataSourceSpec>, Option<SourceMeta>, bool)> {
        let sync = self.store.sync_for(self.tenant);
        let spec_opt = sync.get_source(source_id)?;
        let sync_meta_opt: Option<SourceMeta> = self
            .store
            .get(ns::SYNC_STATE, &self.sync_key("smeta:", source_id))?
            .map(|(_, raw)| decode_json(&raw, "sync source meta"))
            .transpose()?;

        let is_stale_or_deleted = match &spec_opt {
            Some(spec) => spec.instance_id != ctx_instance_id,
            None => {
                let sgen_opt: Option<SyncGenProbe> = self
                    .store
                    .get(ns::SYNC_STATE, &self.sync_key("sgen:", source_id))?
                    .map(|(_, raw)| decode_json(&raw, "sync generation state"))
                    .transpose()?;
                match sgen_opt {
                    Some(sgen) => {
                        sgen.retired_instance_ids
                            .iter()
                            .any(|r| r == ctx_instance_id)
                            || sgen
                                .active_instance_id
                                .as_deref()
                                .is_some_and(|a| a != ctx_instance_id)
                    }
                    None => false,
                }
            }
        };
        Ok((spec_opt, sync_meta_opt, is_stale_or_deleted))
    }

    /// Returns `true` if `meta` is still valid (not superseded by SyncStore deletion or ABA recreation).
    pub fn is_meta_live(&self, meta: &ContextSourceMeta) -> Result<bool> {
        let (_, _, stale) = self.inspect_sync_lineage(&meta.source_id, &meta.instance_id)?;
        Ok(!stale)
    }

    /// Compute exact [`IndexVisibility`] against `SyncStore`'s committed source state.
    pub fn visibility(&self, source_id: &str) -> Result<IndexVisibility> {
        validate_ident(source_id, "source_id")?;
        let ctx_meta_opt = self.get_meta(source_id)?;

        match ctx_meta_opt {
            Some(ctx) => {
                let (spec_opt, sync_meta_opt, stale_or_deleted) =
                    self.inspect_sync_lineage(source_id, &ctx.instance_id)?;
                match spec_opt {
                    Some(spec) => {
                        let store_ticks = sync_meta_opt.map(|m| m.total_ticks).unwrap_or(0);
                        let instance_matched = spec.instance_id == ctx.instance_id;
                        let lag = if instance_matched {
                            store_ticks.saturating_sub(ctx.indexed_tick_seq)
                        } else {
                            store_ticks.max(1)
                        };
                        Ok(IndexVisibility {
                            source_id: source_id.to_owned(),
                            instance_id: ctx.instance_id,
                            store_tick_seq: store_ticks,
                            indexed_tick_seq: ctx.indexed_tick_seq,
                            delta_seq: ctx.latest_delta_seq,
                            lag_ticks: lag,
                            instance_matched,
                            is_caught_up: instance_matched && lag == 0,
                        })
                    }
                    None if stale_or_deleted => Ok(IndexVisibility {
                        source_id: source_id.to_owned(),
                        instance_id: ctx.instance_id,
                        store_tick_seq: 0,
                        indexed_tick_seq: ctx.indexed_tick_seq,
                        delta_seq: ctx.latest_delta_seq,
                        lag_ticks: 1,
                        instance_matched: false,
                        is_caught_up: false,
                    }),
                    None => Ok(IndexVisibility {
                        source_id: source_id.to_owned(),
                        instance_id: ctx.instance_id,
                        store_tick_seq: ctx.indexed_tick_seq,
                        indexed_tick_seq: ctx.indexed_tick_seq,
                        delta_seq: ctx.latest_delta_seq,
                        lag_ticks: 0,
                        instance_matched: true,
                        is_caught_up: true,
                    }),
                }
            }
            None => {
                let sync = self.store.sync_for(self.tenant);
                if let Some(spec) = sync.get_source(source_id)? {
                    let sync_meta_opt: Option<SourceMeta> = self
                        .store
                        .get(ns::SYNC_STATE, &self.sync_key("smeta:", source_id))?
                        .map(|(_, raw)| decode_json(&raw, "sync source meta"))
                        .transpose()?;
                    let store_ticks = sync_meta_opt.map(|m| m.total_ticks).unwrap_or(0);
                    Ok(IndexVisibility {
                        source_id: source_id.to_owned(),
                        instance_id: spec.instance_id,
                        store_tick_seq: store_ticks,
                        indexed_tick_seq: 0,
                        delta_seq: 0,
                        lag_ticks: store_ticks,
                        instance_matched: true,
                        is_caught_up: store_ticks == 0,
                    })
                } else {
                    Ok(IndexVisibility {
                        source_id: source_id.to_owned(),
                        instance_id: "adhoc-0".to_owned(),
                        store_tick_seq: 0,
                        indexed_tick_seq: 0,
                        delta_seq: 0,
                        lag_ticks: 0,
                        instance_matched: true,
                        is_caught_up: true,
                    })
                }
            }
        }
    }

    /// Atomically commit unit updates, compressed index state, and a monotonic [`ContextDelta`].
    pub fn commit_delta(&self, req: ContextCommitRequest<'_>) -> Result<ContextCommitOutcome> {
        validate_ident(req.source_id, "source_id")?;
        validate_ident(req.instance_id, "instance_id")?;
        let now = now_store_ms();
        let lvl = self.store.config().snapshot_compress_level;

        // Pre-compress payloads outside the write transaction lock
        let mut encoded_res = Vec::with_capacity(req.resource_units.len());
        for (res_key, units) in req.resource_units {
            let k = self.res_key(req.source_id, res_key);
            if k.len() > u16::MAX as usize {
                return Err(Error::KeyTooLarge(k.len()));
            }
            if units.is_empty() {
                encoded_res.push((res_key.as_str(), k, None));
            } else {
                encoded_res.push((
                    res_key.as_str(),
                    k,
                    Some(encode_zstd_json(units, lvl, "resource units")?),
                ));
            }
        }
        let mut encoded_ret: Vec<Vec<u8>> = Vec::with_capacity(req.retired_units.len());
        for u in req.retired_units {
            encoded_ret.push(encode_zstd_json(u, lvl, "retired unit")?);
        }
        let compressed_index = match req.index_blob {
            Some(raw) => {
                if raw.len() > MAX_CONTEXT_BLOB {
                    return Err(Error::ValueTooLarge {
                        size: raw.len(),
                        limit: MAX_CONTEXT_BLOB,
                    });
                }
                let c = compress_snapshot(raw, lvl);
                if c.len() > MAX_CONTEXT_BLOB {
                    return Err(Error::ValueTooLarge {
                        size: c.len(),
                        limit: MAX_CONTEXT_BLOB,
                    });
                }
                Some(c)
            }
            None => None,
        };

        let mut tx = self.store.atomic_tx();
        let meta_key = self.meta_key(req.source_id);
        let existing_meta: Option<ContextSourceMeta> = match tx.get(ns::CONTEXT, &meta_key)? {
            Some((_, raw)) => Some(decode_json(&raw, "context source meta")?),
            None => None,
        };

        let mut instance_replaced = false;
        let (
            next_delta_seq,
            mut earliest_seq,
            mut ret_start,
            mut ret_end,
            mut res_keys,
            mut hist_keys,
            indexed_tick_seq,
        ) = match &existing_meta {
            Some(m) if m.instance_id != req.instance_id => {
                // ABA guard: source was deleted and recreated with a fresh `instance_id`!
                self.purge_source_data_in_tx(&mut tx, req.source_id, Some(m))?;
                instance_replaced = true;
                (
                    1u64,
                    1u64,
                    0u64,
                    0u64,
                    BTreeSet::new(),
                    BTreeSet::new(),
                    req.store_tick_seq,
                )
            }
            Some(m) => {
                // Reject out-of-order stale ticks; skip duplicate same-tick replay unless spec_stamp upgraded
                // or caller explicitly requested ad-hoc manual progression (`allow_same_zero_tick`).
                if req.store_tick_seq < m.indexed_tick_seq && !req.allow_same_zero_tick {
                    return Ok(ContextCommitOutcome::IdempotentReplaySkipped {
                        existing_delta_seq: m.latest_delta_seq,
                    });
                }
                if !req.allow_same_zero_tick
                    && req.store_tick_seq == m.indexed_tick_seq
                    && m.spec_stamp == req.spec_stamp
                {
                    return Ok(ContextCommitOutcome::IdempotentReplaySkipped {
                        existing_delta_seq: m.latest_delta_seq,
                    });
                }
                (
                    m.latest_delta_seq.saturating_add(1),
                    m.earliest_retained_delta_seq.max(1),
                    m.retired_seq_start,
                    m.retired_seq_end,
                    m.resource_keys.clone(),
                    m.history_resource_keys.clone(),
                    m.indexed_tick_seq.max(req.store_tick_seq),
                )
            }
            None => (
                1u64,
                1u64,
                0u64,
                0u64,
                BTreeSet::new(),
                BTreeSet::new(),
                req.store_tick_seq,
            ),
        };

        let mut delta = req.delta;
        delta.instance_id = req.instance_id.to_owned();
        delta.delta_seq = next_delta_seq;
        delta.store_tick_seq = indexed_tick_seq;
        if delta.created_at_ms == 0 {
            delta.created_at_ms = now;
        }
        let delta_bytes = encode_zstd_json(&delta, lvl, "context delta")?;

        for (rk, k, v_opt) in encoded_res {
            let units_slice = req.resource_units.get(rk).map(Vec::as_slice).unwrap_or(&[]);
            let event_ms = history::infer_resource_event_ms(
                rk,
                units_slice,
                req.retired_units,
                delta.created_at_ms,
            );
            self.record_resource_history_in_tx(
                &mut tx,
                req.source_id,
                rk,
                event_ms,
                units_slice,
                now,
            )?;
            hist_keys.insert(rk.to_owned());
            match v_opt {
                Some(bytes) => {
                    tx.put(ns::CONTEXT, &k, &bytes, now)?;
                    res_keys.insert(rk.to_owned());
                }
                None => {
                    tx.delete(ns::CONTEXT, &k)?;
                    res_keys.remove(rk);
                }
            }
        }

        for bytes in encoded_ret {
            let k = self.retired_key(req.source_id, ret_end);
            tx.put(ns::CONTEXT, &k, &bytes, now)?;
            ret_end = ret_end.saturating_add(1);
        }
        let live_ret = ret_end.saturating_sub(ret_start);
        if live_ret > MAX_RETIRED_UNITS_PER_SOURCE as u64 {
            let target_start = ret_end - MAX_RETIRED_UNITS_PER_SOURCE as u64;
            for old_seq in ret_start..target_start {
                tx.delete(ns::CONTEXT, &self.retired_key(req.source_id, old_seq))?;
            }
            ret_start = target_start;
        }

        if let Some(idx_bytes) = compressed_index {
            tx.put(ns::CONTEXT, &self.index_key(req.source_id), &idx_bytes, now)?;
        }

        let d_key = self.delta_key(req.source_id, next_delta_seq);
        tx.put(ns::CONTEXT, &d_key, &delta_bytes, now)?;
        let retained_deltas = next_delta_seq
            .saturating_sub(earliest_seq)
            .saturating_add(1);
        if retained_deltas > MAX_CONTEXT_DELTAS_PER_SOURCE as u64 {
            let target_earliest = next_delta_seq + 1 - MAX_CONTEXT_DELTAS_PER_SOURCE as u64;
            for old_seq in earliest_seq..target_earliest {
                tx.delete(ns::CONTEXT, &self.delta_key(req.source_id, old_seq))?;
            }
            earliest_seq = target_earliest;
        }

        let new_meta = ContextSourceMeta {
            source_id: req.source_id.to_owned(),
            instance_id: req.instance_id.to_owned(),
            indexed_tick_seq,
            latest_delta_seq: next_delta_seq,
            earliest_retained_delta_seq: earliest_seq,
            spec_stamp: req.spec_stamp,
            updated_at_ms: now,
            retired_seq_start: ret_start,
            retired_seq_end: ret_end,
            resource_keys: res_keys,
            history_resource_keys: hist_keys,
        };
        tx.put(
            ns::CONTEXT,
            &meta_key,
            &encode_json(&new_meta, "context source meta")?,
            now,
        )?;
        tx.commit()?;

        if instance_replaced {
            Ok(ContextCommitOutcome::InstanceReplacedAndCommitted {
                delta_seq: next_delta_seq,
            })
        } else {
            Ok(ContextCommitOutcome::Committed {
                delta_seq: next_delta_seq,
            })
        }
    }

    /// Load live units for a specific `(source_id, resource_key)`, returning empty if ABA-superseded or deleted.
    pub fn load_resource_units(
        &self,
        source_id: &str,
        resource_key: &str,
    ) -> Result<Vec<KnowledgeUnit>> {
        validate_ident(source_id, "source_id")?;
        let Some(meta) = self.get_meta(source_id)? else {
            return Ok(Vec::new());
        };
        if !self.is_meta_live(&meta)? {
            return Ok(Vec::new());
        }
        let k = self.res_key(source_id, resource_key);
        let Some((_, raw)) = self.store.get(ns::CONTEXT, &k)? else {
            return Ok(Vec::new());
        };
        decode_zstd_json(&raw, "resource units")
    }

    /// Load all live units across all resources of `source_id` via O(K) direct lookups (no full-ns key scan).
    pub fn load_all_live_units(&self, source_id: &str) -> Result<Vec<KnowledgeUnit>> {
        validate_ident(source_id, "source_id")?;
        let Some(meta) = self.get_meta(source_id)? else {
            return Ok(Vec::new());
        };
        if !self.is_meta_live(&meta)? {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for rk in &meta.resource_keys {
            let k = self.res_key(source_id, rk);
            if let Some((_, raw)) = self.store.get(ns::CONTEXT, &k)? {
                let batch: Vec<KnowledgeUnit> = decode_zstd_json(&raw, "resource units")?;
                out.extend(batch);
            }
        }
        Ok(out)
    }

    /// List distinct live indexed `source_id`s visible to this tenant (filtering deleted Sync ghosts).
    pub fn list_indexed_sources(&self) -> Result<Vec<ContextSourceMeta>> {
        let pfx = self.prefix_bytes(&format!("cmeta{SEP}"));
        let mut keys = self.store.keys(ns::CONTEXT, |k| k.starts_with(&pfx))?;
        keys.sort();
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            if let Some((_, raw)) = self.store.get(ns::CONTEXT, &k)? {
                let meta: ContextSourceMeta = decode_json(&raw, "context source meta")?;
                if self.is_meta_live(&meta)? {
                    out.push(meta);
                }
            }
        }
        Ok(out)
    }

    /// Load decompressed serialized `ContextIndex` blob for `source_id`, returning `None` if ABA-superseded or deleted.
    pub fn load_index_blob(&self, source_id: &str) -> Result<Option<Vec<u8>>> {
        validate_ident(source_id, "source_id")?;
        let Some(meta) = self.get_meta(source_id)? else {
            return Ok(None);
        };
        if !self.is_meta_live(&meta)? {
            return Ok(None);
        }
        let Some((_, raw)) = self.store.get(ns::CONTEXT, &self.index_key(source_id))? else {
            return Ok(None);
        };
        let decompressed = pagesift_compress::decompress(&raw, MAX_CONTEXT_BLOB)
            .map_err(|e| Error::Corrupt(format!("context index {source_id}: {e}")))?;
        Ok(Some(decompressed))
    }

    /// Read recent [`ContextDelta`] records for `source_id` via O(limit) direct sequence lookups.
    pub fn list_deltas(
        &self,
        source_id: &str,
        after_seq: Option<u64>,
        limit: usize,
    ) -> Result<Vec<ContextDelta>> {
        validate_ident(source_id, "source_id")?;
        let Some(meta) = self.get_meta(source_id)? else {
            return Ok(Vec::new());
        };
        if !self.is_meta_live(&meta)? || meta.latest_delta_seq == 0 {
            return Ok(Vec::new());
        }
        let cap = limit.max(1);
        let start_seq = match after_seq {
            Some(min_s) => min_s
                .saturating_add(1)
                .max(meta.earliest_retained_delta_seq),
            None => meta.earliest_retained_delta_seq.max(1),
        };
        if start_seq > meta.latest_delta_seq {
            return Ok(Vec::new());
        }
        let mut out =
            Vec::with_capacity((meta.latest_delta_seq - start_seq + 1).min(cap as u64) as usize);
        for seq in start_seq..=meta.latest_delta_seq {
            if let Some((_, raw)) = self
                .store
                .get(ns::CONTEXT, &self.delta_key(source_id, seq))?
            {
                out.push(decode_zstd_json(&raw, "context delta")?);
                if out.len() >= cap {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Persist or load tenant-isolated `ContextLearnState` JSON bytes.
    pub fn put_learn_bytes(&self, scope_id: &str, json_bytes: &[u8]) -> Result<()> {
        validate_ident(scope_id, "scope_id")?;
        if json_bytes.len() > MAX_CONTEXT_BLOB {
            return Err(Error::ValueTooLarge {
                size: json_bytes.len(),
                limit: MAX_CONTEXT_BLOB,
            });
        }
        let compressed = compress_snapshot(json_bytes, self.store.config().snapshot_compress_level);
        self.store
            .put(
                ns::CONTEXT,
                &self.learn_key(scope_id),
                &compressed,
                now_store_ms(),
            )
            .map(drop)
    }

    pub fn load_learn_bytes(&self, scope_id: &str) -> Result<Option<Vec<u8>>> {
        validate_ident(scope_id, "scope_id")?;
        let Some((_, raw)) = self.store.get(ns::CONTEXT, &self.learn_key(scope_id))? else {
            return Ok(None);
        };
        let dec = pagesift_compress::decompress(&raw, MAX_CONTEXT_BLOB)
            .map_err(|e| Error::Corrupt(format!("context learn {scope_id}: {e}")))?;
        Ok(Some(dec))
    }

    /// Purge all Context state belonging to `source_id` within this tenant scope.
    pub fn purge_source(&self, source_id: &str) -> Result<()> {
        validate_ident(source_id, "source_id")?;
        let mut tx = self.store.atomic_tx();
        let meta_key = self.meta_key(source_id);
        let existing_meta: Option<ContextSourceMeta> = match tx.get(ns::CONTEXT, &meta_key)? {
            Some((_, raw)) => decode_json(&raw, "context source meta").ok(),
            None => None,
        };
        self.purge_source_data_in_tx(&mut tx, source_id, existing_meta.as_ref())?;
        tx.delete(ns::CONTEXT, &meta_key)?;
        tx.commit()
    }

    fn purge_source_data_in_tx(
        &self,
        tx: &mut AtomicTx<'_>,
        source_id: &str,
        known_meta: Option<&ContextSourceMeta>,
    ) -> Result<()> {
        if let Some(m) = known_meta {
            for rk in &m.resource_keys {
                tx.delete(ns::CONTEXT, &self.res_key(source_id, rk))?;
            }
            for rk in m.resource_keys.iter().chain(m.history_resource_keys.iter()) {
                tx.delete(ns::CONTEXT, &self.history_key(source_id, rk))?;
            }
            if m.latest_delta_seq > 0 {
                for seq in m.earliest_retained_delta_seq.max(1)..=m.latest_delta_seq {
                    tx.delete(ns::CONTEXT, &self.delta_key(source_id, seq))?;
                }
            }
            for seq in m.retired_seq_start..m.retired_seq_end {
                tx.delete(ns::CONTEXT, &self.retired_key(source_id, seq))?;
            }
        } else {
            for sub in [
                format!("cres{SEP}{source_id}{SEP}"),
                format!("chis{SEP}{source_id}{SEP}"),
                format!("cret{SEP}{source_id}{SEP}"),
                format!("cdlt{SEP}{source_id}{SEP}"),
            ] {
                let pfx = self.prefix_bytes(&sub);
                for k in tx.keys_matching(ns::CONTEXT, &pfx)? {
                    tx.delete(ns::CONTEXT, &k)?;
                }
            }
        }
        tx.delete(ns::CONTEXT, &self.index_key(source_id))?;
        Ok(())
    }
}

pub(crate) fn validate_ident(s: &str, label: &str) -> Result<()> {
    if s.is_empty() || s.trim().is_empty() {
        return Err(Error::Corrupt(format!("{label} must not be empty")));
    }
    if s.len() > MAX_CONTEXT_IDENT_LEN {
        return Err(Error::KeyTooLarge(s.len()));
    }
    // [协议] US-ASCII (RFC 20)：0x00..0x1F（含 0x1F 分隔符）与 0x7F 为控制字符，严禁出现在逻辑标识符中
    if s.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(Error::Corrupt(format!(
            "{label} must not contain control characters"
        )));
    }
    Ok(())
}

pub(crate) fn encode_json<T: Serialize + ?Sized>(val: &T, label: &str) -> Result<Vec<u8>> {
    serde_json::to_vec(val).map_err(|e| Error::Corrupt(format!("encode {label}: {e}")))
}

pub(crate) fn decode_json<T: serde::de::DeserializeOwned>(raw: &[u8], label: &str) -> Result<T> {
    serde_json::from_slice(raw).map_err(|e| Error::Corrupt(format!("decode {label}: {e}")))
}

fn encode_zstd_json<T: Serialize + ?Sized>(val: &T, level: i32, label: &str) -> Result<Vec<u8>> {
    let json = encode_json(val, label)?;
    if json.len() > MAX_CONTEXT_BLOB {
        return Err(Error::ValueTooLarge {
            size: json.len(),
            limit: MAX_CONTEXT_BLOB,
        });
    }
    let compressed = compress_snapshot(&json, level);
    if compressed.len() > MAX_CONTEXT_BLOB {
        return Err(Error::ValueTooLarge {
            size: compressed.len(),
            limit: MAX_CONTEXT_BLOB,
        });
    }
    Ok(compressed)
}

fn decode_zstd_json<T: serde::de::DeserializeOwned>(raw: &[u8], label: &str) -> Result<T> {
    let dec = pagesift_compress::decompress(raw, MAX_CONTEXT_BLOB)
        .map_err(|e| Error::Corrupt(format!("decompress {label}: {e}")))?;
    decode_json(&dec, label)
}

pub use checkpoint::DEFAULT_DELTA_POLL_LIMIT;
