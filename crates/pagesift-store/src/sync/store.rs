//! `SyncStore` 存储读写、原子 Tick 提交、CAS 修订并发控制与精确历史状态重建（C03 / C05 / C06 / C30 / R02）。

use std::collections::{BTreeMap, BTreeSet};

use pagesift_graph::{ClaimStatus, ConflictRecord};

use super::{
    DataSourceSpec, MAX_CHECKPOINTS_PER_SOURCE, MAX_DELTAS_PER_SOURCE, ResourceRecord, SourceMeta,
    SourceSpecRevision, SourceStateCheckpoint, SourceStateView, SpecCasOutcome, SyncDelta,
    TickCommitOutcome,
};
use crate::engine::{AtomicTx, ns};
use crate::graph::PreparedVersionBundle;
use crate::{Error, Result, Store};

/// 单次同步周期原子提交参数（C05 / C06 / R02）。
pub struct TickCommitRequest<'a> {
    pub source_id: &'a str,
    pub expected_spec_revision: u64,
    pub spec_revision_used: u64,
    pub inventory: &'a mut Vec<ResourceRecord>,
    pub experience_bytes: &'a [u8],
    pub delta: &'a mut SyncDelta,
    pub graph_bundles: Vec<PreparedVersionBundle>,
    pub conflict_subjects: &'a BTreeSet<String>,
    pub conflicts: &'a [ConflictRecord],
}

/// 持续数据同步存储视图。
pub struct SyncStore<'a> {
    store: &'a Store,
    tenant: Option<&'a str>,
}

impl<'a> SyncStore<'a> {
    pub(crate) fn new(store: &'a Store, tenant: Option<&'a str>) -> Self {
        Self { store, tenant }
    }

    fn key(&self, prefix: &str, rest: &str) -> Vec<u8> {
        match self.tenant.filter(|t| !t.is_empty()) {
            Some(t) => format!("t:{t}|{prefix}{rest}").into_bytes(),
            None => format!("{prefix}{rest}").into_bytes(),
        }
    }

    fn prefix_bytes(&self, prefix: &str) -> Vec<u8> {
        match self.tenant.filter(|t| !t.is_empty()) {
            Some(t) => format!("t:{t}|{prefix}").into_bytes(),
            None => prefix.as_bytes().to_vec(),
        }
    }

    fn stage_source_write_in_tx(
        &self,
        tx: &mut AtomicTx<'_>,
        stored: &DataSourceSpec,
    ) -> Result<()> {
        let spec_bytes = serde_json::to_vec(stored).map_err(|e| Error::Corrupt(e.to_string()))?;
        let rev_entry = SourceSpecRevision {
            revision: stored.revision,
            updated_at_ms: stored.updated_at_ms,
            spec: stored.clone(),
        };
        let rev_bytes =
            serde_json::to_vec(&rev_entry).map_err(|e| Error::Corrupt(e.to_string()))?;
        let rev_key = self.key(
            "srev:",
            &format!(
                "{}:{:020}:{:020}",
                stored.id, stored.updated_at_ms, stored.revision
            ),
        );
        tx.put(
            ns::SYNC_STATE,
            &self.key("src:", &stored.id),
            &spec_bytes,
            stored.updated_at_ms.max(1),
        )?;
        tx.put(
            ns::SYNC_STATE,
            &rev_key,
            &rev_bytes,
            stored.updated_at_ms.max(1),
        )?;
        let meta_key = self.key("smeta:", &stored.id);
        if !tx.contains(ns::SYNC_STATE, &meta_key)? {
            let meta_bytes = serde_json::to_vec(&SourceMeta::default())
                .map_err(|e| Error::Corrupt(e.to_string()))?;
            tx.put(
                ns::SYNC_STATE,
                &meta_key,
                &meta_bytes,
                stored.created_at_ms.max(1),
            )?;
        }
        Ok(())
    }

    pub fn put_source(&self, spec: &DataSourceSpec) -> Result<()> {
        let mut stored = spec.clone();
        if stored.revision == 0 {
            stored.revision = 1;
        }
        let mut tx = self.store.atomic_tx();
        self.stage_source_write_in_tx(&mut tx, &stored)?;
        tx.commit()
    }

    /// 原子创建或更新数据源规范：若已存在则保留 `created_at_ms` 并单调递增 `revision`（R02）。
    pub fn upsert_source(&self, mut spec: DataSourceSpec) -> Result<DataSourceSpec> {
        let mut tx = self.store.atomic_tx();
        let src_key = self.key("src:", &spec.id);
        if let Some((_, raw)) = tx.get(ns::SYNC_STATE, &src_key)? {
            let current: DataSourceSpec =
                serde_json::from_slice(&raw).map_err(|e| Error::Corrupt(e.to_string()))?;
            spec.revision = current.revision.saturating_add(1);
            spec.created_at_ms = current.created_at_ms;
            if spec.updated_at_ms <= current.updated_at_ms {
                spec.updated_at_ms = current.updated_at_ms.saturating_add(1);
            }
        } else if spec.revision == 0 {
            spec.revision = 1;
        }
        self.stage_source_write_in_tx(&mut tx, &spec)?;
        tx.commit()?;
        Ok(spec)
    }

    /// 原子校验并更新 `DataSourceSpec`，若已被并发修改或删除则中止（C05 / R02）。
    pub fn update_source_cas(
        &self,
        mut spec: DataSourceSpec,
        expected_revision: Option<u64>,
    ) -> Result<SpecCasOutcome> {
        let mut tx = self.store.atomic_tx();
        let src_key = self.key("src:", &spec.id);
        let Some((_, raw)) = tx.get(ns::SYNC_STATE, &src_key)? else {
            return Ok(SpecCasOutcome::NotFound);
        };
        let current: DataSourceSpec =
            serde_json::from_slice(&raw).map_err(|e| Error::Corrupt(e.to_string()))?;
        if let Some(exp) = expected_revision
            && current.revision != exp
        {
            return Ok(SpecCasOutcome::Conflict { current });
        }
        spec.revision = current.revision.saturating_add(1);
        spec.created_at_ms = current.created_at_ms;
        if spec.updated_at_ms <= current.updated_at_ms {
            spec.updated_at_ms = current.updated_at_ms.saturating_add(1);
        }
        self.stage_source_write_in_tx(&mut tx, &spec)?;
        tx.commit()?;
        Ok(SpecCasOutcome::Updated(spec))
    }

    pub fn get_source(&self, source_id: &str) -> Result<Option<DataSourceSpec>> {
        let Some((_, bytes)) = self
            .store
            .get(ns::SYNC_STATE, &self.key("src:", source_id))?
        else {
            return Ok(None);
        };
        let spec = serde_json::from_slice(&bytes).map_err(|e| Error::Corrupt(e.to_string()))?;
        Ok(Some(spec))
    }

    pub fn list_sources(&self) -> Result<Vec<DataSourceSpec>> {
        let pfx = self.prefix_bytes("src:");
        let keys = self.store.keys(ns::SYNC_STATE, |k| k.starts_with(&pfx))?;
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            if let Some((_, bytes)) = self.store.get(ns::SYNC_STATE, &k)?
                && let Ok(spec) = serde_json::from_slice::<DataSourceSpec>(&bytes)
            {
                out.push(spec);
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// 原子清理数据源及其清单、经验、增量、检查点、规格修订历史（R02 传播存储错误，绝不静默丢弃）。
    pub fn delete_source(&self, source_id: &str) -> Result<bool> {
        let mut tx = self.store.atomic_tx();
        let existed = tx.delete(ns::SYNC_STATE, &self.key("src:", source_id))?;
        tx.delete(ns::SYNC_STATE, &self.key("inv:", source_id))?;
        tx.delete(ns::SYNC_STATE, &self.key("exp:", source_id))?;
        tx.delete(ns::SYNC_STATE, &self.key("smeta:", source_id))?;
        for prefix_name in ["dlt:", "sck:", "srev:"] {
            let pfx = self.key(prefix_name, &format!("{source_id}:"));
            for k in tx.keys_matching(ns::SYNC_STATE, &pfx)? {
                tx.delete(ns::SYNC_STATE, &k)?;
            }
        }
        tx.commit()?;
        Ok(existed)
    }

    pub fn get_inventory(&self, source_id: &str) -> Result<Vec<ResourceRecord>> {
        let Some((_, bytes)) = self
            .store
            .get(ns::SYNC_STATE, &self.key("inv:", source_id))?
        else {
            return Ok(Vec::new());
        };
        serde_json::from_slice(&bytes).map_err(|e| Error::Corrupt(e.to_string()))
    }

    pub fn put_inventory(
        &self,
        source_id: &str,
        records: &[ResourceRecord],
        ts_ms: u64,
    ) -> Result<()> {
        let bytes = serde_json::to_vec(records).map_err(|e| Error::Corrupt(e.to_string()))?;
        self.store
            .put(ns::SYNC_STATE, &self.key("inv:", source_id), &bytes, ts_ms)?;
        Ok(())
    }

    pub fn get_experience_bytes(&self, source_id: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .store
            .get(ns::SYNC_STATE, &self.key("exp:", source_id))?
            .map(|(_, b)| b))
    }

    pub fn put_experience_bytes(&self, source_id: &str, bytes: &[u8], ts_ms: u64) -> Result<()> {
        self.store
            .put(ns::SYNC_STATE, &self.key("exp:", source_id), bytes, ts_ms)?;
        Ok(())
    }

    pub fn append_delta(&self, delta: &SyncDelta) -> Result<()> {
        let mut tx = self.store.atomic_tx();
        let meta_key = self.key("smeta:", &delta.source_id);
        let mut meta: SourceMeta = match tx.get(ns::SYNC_STATE, &meta_key)? {
            Some((_, raw)) => {
                serde_json::from_slice(&raw).map_err(|e| Error::Corrupt(e.to_string()))?
            }
            None => SourceMeta::default(),
        };
        meta.total_ticks = meta.total_ticks.saturating_add(1).max(delta.tick_seq);
        if meta.first_tick_at_ms.is_none() {
            meta.first_tick_at_ms = Some(delta.completed_at_ms);
        }
        let mut stored_delta = delta.clone();
        if stored_delta.tick_seq == 0 {
            stored_delta.tick_seq = meta.total_ticks;
        }
        let bytes = serde_json::to_vec(&stored_delta).map_err(|e| Error::Corrupt(e.to_string()))?;
        let dlt_key = self.key(
            "dlt:",
            &format!(
                "{}:{:020}:{}",
                delta.source_id, delta.completed_at_ms, delta.tick_id
            ),
        );
        tx.put(
            ns::SYNC_STATE,
            &dlt_key,
            &bytes,
            delta.completed_at_ms.max(1),
        )?;
        let meta_bytes = serde_json::to_vec(&meta).map_err(|e| Error::Corrupt(e.to_string()))?;
        tx.put(
            ns::SYNC_STATE,
            &meta_key,
            &meta_bytes,
            delta.completed_at_ms.max(1),
        )?;

        let pfx = self.key("dlt:", &format!("{}:", delta.source_id));
        let mut all_keys = tx.keys_matching(ns::SYNC_STATE, &pfx)?;
        if all_keys.len() > MAX_DELTAS_PER_SOURCE {
            all_keys.sort();
            let remove_count = all_keys.len() - MAX_DELTAS_PER_SOURCE;
            for old_key in all_keys.into_iter().take(remove_count) {
                tx.delete(ns::SYNC_STATE, &old_key)?;
            }
        }
        tx.commit()
    }

    /// 单个 `AtomicTx` 提交整轮 Sync Tick：校验 Source 存在及 `revision` -> 写图版本/声明/冲突 -> 写清单/经验/增量/检查点/总计数（C05 / C06 / C30 / R02）。
    pub fn commit_tick_atomic(&self, req: TickCommitRequest<'_>) -> Result<TickCommitOutcome> {
        let mut tx = self.store.atomic_tx();
        let src_key = self.key("src:", req.source_id);
        let Some((_, src_raw)) = tx.get(ns::SYNC_STATE, &src_key)? else {
            return Ok(TickCommitOutcome::SourceDeleted);
        };
        let current_spec: DataSourceSpec =
            serde_json::from_slice(&src_raw).map_err(|e| Error::Corrupt(e.to_string()))?;
        if current_spec.revision != req.expected_spec_revision {
            return Ok(TickCommitOutcome::StaleSpec {
                current: Box::new(current_spec),
            });
        }

        let meta_key = self.key("smeta:", req.source_id);
        let mut meta: SourceMeta = match tx.get(ns::SYNC_STATE, &meta_key)? {
            Some((_, raw)) => {
                serde_json::from_slice(&raw).map_err(|e| Error::Corrupt(e.to_string()))?
            }
            None => SourceMeta::default(),
        };
        meta.total_ticks = meta.total_ticks.saturating_add(1);
        if meta.first_tick_at_ms.is_none() {
            meta.first_tick_at_ms = Some(req.delta.completed_at_ms);
        }
        if meta.earliest_retained_checkpoint_at_ms.is_none() {
            meta.earliest_retained_checkpoint_at_ms = Some(req.delta.completed_at_ms);
        }
        let tick_seq = meta.total_ticks;
        req.delta.tick_seq = tick_seq;

        let gs = self.store.graph_for(self.tenant);
        for bundle in &req.graph_bundles {
            match gs.stage_prepared_bundle_in_tx(&mut tx, bundle)? {
                crate::VersionBundleOutcome::Committed {
                    claims,
                    claim_transitions,
                    ..
                } => {
                    req.delta.claim_transitions.extend(claim_transitions);
                    // 用事务内协调后的真实 active+unverified 数量刷新资源记录 claim_count
                    if let Some(first_claim) = claims.first()
                        && let Some(rec) = req
                            .inventory
                            .iter_mut()
                            .find(|r| r.resource_key == first_claim.claim.resource_key)
                    {
                        rec.claim_count = claims
                            .iter()
                            .filter(|c| {
                                matches!(
                                    c.status,
                                    ClaimStatus::Active | ClaimStatus::Unverified { .. }
                                )
                            })
                            .count();
                    }
                }
                crate::VersionBundleOutcome::StaleParent { .. } => {}
            }
        }

        if !req.conflicts.is_empty() {
            gs.stage_conflicts_for_subjects_in_tx(
                &mut tx,
                req.conflict_subjects,
                req.conflicts,
                req.delta.completed_at_ms,
            )?;
            req.delta.conflicts = req.conflicts.to_vec();
        } else if !req.conflict_subjects.is_empty() {
            let recomputed = gs.recompute_conflicts_for_subjects_in_tx(
                &mut tx,
                req.conflict_subjects,
                req.delta.completed_at_ms,
            )?;
            req.delta.conflicts = recomputed;
        }

        let ts = req.delta.completed_at_ms.max(1);
        let inv_bytes =
            serde_json::to_vec(req.inventory).map_err(|e| Error::Corrupt(e.to_string()))?;
        tx.put(
            ns::SYNC_STATE,
            &self.key("inv:", req.source_id),
            &inv_bytes,
            ts,
        )?;
        tx.put(
            ns::SYNC_STATE,
            &self.key("exp:", req.source_id),
            req.experience_bytes,
            ts,
        )?;

        let delta_bytes =
            serde_json::to_vec(req.delta).map_err(|e| Error::Corrupt(e.to_string()))?;
        let dlt_key = self.key(
            "dlt:",
            &format!(
                "{}:{:020}:{}",
                req.source_id, req.delta.completed_at_ms, req.delta.tick_id
            ),
        );
        tx.put(ns::SYNC_STATE, &dlt_key, &delta_bytes, ts)?;

        let dlt_pfx = self.key("dlt:", &format!("{}:", req.source_id));
        let mut dlt_keys = tx.keys_matching(ns::SYNC_STATE, &dlt_pfx)?;
        if dlt_keys.len() > MAX_DELTAS_PER_SOURCE {
            dlt_keys.sort();
            let remove_count = dlt_keys.len() - MAX_DELTAS_PER_SOURCE;
            for old_key in dlt_keys.into_iter().take(remove_count) {
                tx.delete(ns::SYNC_STATE, &old_key)?;
            }
        }

        let ck = SourceStateCheckpoint {
            tick_seq,
            tick_id: req.delta.tick_id.clone(),
            completed_at_ms: req.delta.completed_at_ms,
            spec_revision: req.spec_revision_used,
            coverage_status: req.delta.coverage_status.clone(),
            resources: req.inventory.clone(),
        };
        let ck_bytes = serde_json::to_vec(&ck).map_err(|e| Error::Corrupt(e.to_string()))?;
        let ck_key = self.key(
            "sck:",
            &format!(
                "{}:{:020}:{:020}",
                req.source_id, req.delta.completed_at_ms, tick_seq
            ),
        );
        tx.put(ns::SYNC_STATE, &ck_key, &ck_bytes, ts)?;

        let ck_pfx = self.key("sck:", &format!("{}:", req.source_id));
        let mut ck_keys = tx.keys_matching(ns::SYNC_STATE, &ck_pfx)?;
        if ck_keys.len() > MAX_CHECKPOINTS_PER_SOURCE {
            ck_keys.sort();
            let remove_count = ck_keys.len() - MAX_CHECKPOINTS_PER_SOURCE;
            for old_key in &ck_keys[..remove_count] {
                tx.delete(ns::SYNC_STATE, old_key)?;
            }
            if let Some(earliest_key) = ck_keys.get(remove_count)
                && let Some((_, raw)) = tx.get(ns::SYNC_STATE, earliest_key)?
                && let Ok(earliest) = serde_json::from_slice::<SourceStateCheckpoint>(&raw)
            {
                meta.earliest_retained_checkpoint_at_ms = Some(earliest.completed_at_ms);
            }
        }

        let meta_bytes = serde_json::to_vec(&meta).map_err(|e| Error::Corrupt(e.to_string()))?;
        tx.put(ns::SYNC_STATE, &meta_key, &meta_bytes, ts)?;

        tx.commit()?;
        Ok(TickCommitOutcome::Committed { tick_seq })
    }

    pub fn list_deltas(&self, source_id: &str, limit: usize) -> Result<Vec<SyncDelta>> {
        let pfx = self.key("dlt:", &format!("{source_id}:"));
        let mut keys = self.store.keys(ns::SYNC_STATE, |k| k.starts_with(&pfx))?;
        keys.sort();
        keys.reverse();
        let mut out = Vec::new();
        for k in keys.into_iter().take(limit.max(1)) {
            if let Some((_, bytes)) = self.store.get(ns::SYNC_STATE, &k)?
                && let Ok(d) = serde_json::from_slice::<SyncDelta>(&bytes)
            {
                out.push(d);
            }
        }
        Ok(out)
    }

    pub fn get_delta(&self, source_id: &str, tick_id: &str) -> Result<Option<SyncDelta>> {
        let pfx = self.key("dlt:", &format!("{source_id}:"));
        let suffix = format!(":{tick_id}");
        let keys = self.store.keys(ns::SYNC_STATE, |k| {
            k.starts_with(&pfx) && std::str::from_utf8(k).is_ok_and(|s| s.ends_with(&suffix))
        })?;
        for k in keys.into_iter().rev() {
            if let Some((_, bytes)) = self.store.get(ns::SYNC_STATE, &k)?
                && let Ok(d) = serde_json::from_slice::<SyncDelta>(&bytes)
            {
                return Ok(Some(d));
            }
        }
        Ok(None)
    }
}

#[path = "store_view.rs"]
mod store_view;

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
