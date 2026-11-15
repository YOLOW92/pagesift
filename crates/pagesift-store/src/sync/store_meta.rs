//! Persistent per-`source_id` generation tracking and ABA protection helpers for `SyncStore`.

use serde::{Deserialize, Serialize};

use super::*;

const MAX_RETIRED_INSTANCES: usize = 64;

/// Persistent creation generation state (`sgen:<source_id>`) surviving `delete_source`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SourceGenerationState {
    pub next_gen: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_instance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired_instance_ids: Vec<String>,
}

impl<'a> SyncStore<'a> {
    pub(super) fn load_generation_in_tx(
        &self,
        tx: &AtomicTx<'_>,
        source_id: &str,
    ) -> Result<SourceGenerationState> {
        let key = self.key("sgen:", source_id);
        match tx.get(ns::SYNC_STATE, &key)? {
            Some((_, raw)) => {
                serde_json::from_slice(&raw).map_err(|e| Error::Corrupt(e.to_string()))
            }
            None => Ok(SourceGenerationState::default()),
        }
    }

    pub(super) fn allocate_instance_in_tx(
        &self,
        tx: &mut AtomicTx<'_>,
        source_id: &str,
        ts_ms: u64,
    ) -> Result<String> {
        let mut sgen = self.load_generation_in_tx(tx, source_id)?;
        if let Some(prev) = sgen.active_instance_id.take()
            && !prev.is_empty()
            && !sgen.retired_instance_ids.contains(&prev)
        {
            sgen.retired_instance_ids.push(prev);
        }
        sgen.next_gen = sgen.next_gen.saturating_add(1);
        let new_id = format!("inst-{}-{}", sgen.next_gen, uuid::Uuid::new_v4().simple());
        sgen.active_instance_id = Some(new_id.clone());
        trim_retired(&mut sgen.retired_instance_ids);
        let bytes = serde_json::to_vec(&sgen).map_err(|e| Error::Corrupt(e.to_string()))?;
        tx.put(
            ns::SYNC_STATE,
            &self.key("sgen:", source_id),
            &bytes,
            ts_ms.max(1),
        )?;
        Ok(new_id)
    }

    pub(super) fn retire_instance_in_tx(
        &self,
        tx: &mut AtomicTx<'_>,
        source_id: &str,
        current_spec_instance_id: Option<&str>,
        ts_ms: u64,
    ) -> Result<()> {
        let mut sgen = self.load_generation_in_tx(tx, source_id)?;
        let prev = sgen.active_instance_id.take();
        for candidate in prev.as_deref().into_iter().chain(current_spec_instance_id) {
            if !candidate.is_empty() && !sgen.retired_instance_ids.iter().any(|x| x == candidate) {
                sgen.retired_instance_ids.push(candidate.to_owned());
            }
        }
        if sgen.next_gen == 0 && sgen.retired_instance_ids.is_empty() {
            return Ok(());
        }
        trim_retired(&mut sgen.retired_instance_ids);
        let bytes = serde_json::to_vec(&sgen).map_err(|e| Error::Corrupt(e.to_string()))?;
        tx.put(
            ns::SYNC_STATE,
            &self.key("sgen:", source_id),
            &bytes,
            ts_ms.max(1),
        )?;
        Ok(())
    }

    pub(super) fn resolve_instance_id_for_put(
        &self,
        tx: &mut AtomicTx<'_>,
        spec: &mut DataSourceSpec,
        existing: Option<&DataSourceSpec>,
    ) -> Result<()> {
        let ts = spec.updated_at_ms.max(spec.created_at_ms).max(1);
        match existing {
            Some(cur) if !cur.instance_id.is_empty() => {
                spec.instance_id = cur.instance_id.clone();
            }
            _ => {
                spec.instance_id = self.allocate_instance_in_tx(tx, &spec.id, ts)?;
            }
        }
        Ok(())
    }

    pub(super) fn stage_source_write_in_tx(
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
        match tx.get(ns::SYNC_STATE, &meta_key)? {
            None => {
                let meta = SourceMeta {
                    instance_id: stored.instance_id.clone(),
                    ..SourceMeta::default()
                };
                let meta_bytes =
                    serde_json::to_vec(&meta).map_err(|e| Error::Corrupt(e.to_string()))?;
                tx.put(
                    ns::SYNC_STATE,
                    &meta_key,
                    &meta_bytes,
                    stored.created_at_ms.max(1),
                )?;
            }
            Some((_, raw)) => {
                let mut meta: SourceMeta =
                    serde_json::from_slice(&raw).map_err(|e| Error::Corrupt(e.to_string()))?;
                if meta.instance_id.is_empty() && !stored.instance_id.is_empty() {
                    meta.instance_id = stored.instance_id.clone();
                    let meta_bytes =
                        serde_json::to_vec(&meta).map_err(|e| Error::Corrupt(e.to_string()))?;
                    tx.put(
                        ns::SYNC_STATE,
                        &meta_key,
                        &meta_bytes,
                        stored.updated_at_ms.max(1),
                    )?;
                }
            }
        }
        Ok(())
    }

    pub fn append_delta(&self, delta: &SyncDelta) -> Result<()> {
        let mut tx = self.store.atomic_tx();
        let current_spec = match tx.get(ns::SYNC_STATE, &self.key("src:", &delta.source_id))? {
            Some((_, raw)) => serde_json::from_slice::<DataSourceSpec>(&raw)
                .map_err(|e| Error::Corrupt(e.to_string()))?,
            None => {
                return Err(Error::Corrupt(format!(
                    "cannot append delta for missing source '{}'",
                    delta.source_id
                )));
            }
        };
        if !delta.instance_id.is_empty() && delta.instance_id != current_spec.instance_id {
            return Err(Error::Corrupt(format!(
                "stale instance_id on delta for source '{}'",
                delta.source_id
            )));
        }

        let meta_key = self.key("smeta:", &delta.source_id);
        let mut meta: SourceMeta = match tx.get(ns::SYNC_STATE, &meta_key)? {
            Some((_, raw)) => {
                serde_json::from_slice(&raw).map_err(|e| Error::Corrupt(e.to_string()))?
            }
            None => SourceMeta {
                instance_id: current_spec.instance_id.clone(),
                ..SourceMeta::default()
            },
        };
        if meta.instance_id.is_empty() {
            meta.instance_id = current_spec.instance_id.clone();
        }
        meta.total_ticks = meta.total_ticks.saturating_add(1).max(delta.tick_seq);
        if meta.first_tick_at_ms.is_none() {
            meta.first_tick_at_ms = Some(delta.completed_at_ms);
        }
        let mut stored_delta = delta.clone();
        if stored_delta.instance_id.is_empty() {
            stored_delta.instance_id = current_spec.instance_id;
        }
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

    pub fn list_deltas(&self, source_id: &str, limit: usize) -> Result<Vec<SyncDelta>> {
        let Some(spec) = self.get_source(source_id)? else {
            return Ok(Vec::new());
        };
        let pfx = self.key("dlt:", &format!("{source_id}:"));
        let mut keys = self.store.keys(ns::SYNC_STATE, |k| k.starts_with(&pfx))?;
        keys.sort();
        keys.reverse();
        let mut out = Vec::new();
        for k in keys {
            if let Some((_, bytes)) = self.store.get(ns::SYNC_STATE, &k)? {
                let d: SyncDelta =
                    serde_json::from_slice(&bytes).map_err(|e| Error::Corrupt(e.to_string()))?;
                if !d.instance_id.is_empty()
                    && !spec.instance_id.is_empty()
                    && d.instance_id != spec.instance_id
                {
                    continue;
                }
                out.push(d);
                if out.len() >= limit.max(1) {
                    break;
                }
            }
        }
        Ok(out)
    }

    pub fn get_delta(&self, source_id: &str, tick_id: &str) -> Result<Option<SyncDelta>> {
        let Some(spec) = self.get_source(source_id)? else {
            return Ok(None);
        };
        let pfx = self.key("dlt:", &format!("{source_id}:"));
        let suffix = format!(":{tick_id}");
        let keys = self.store.keys(ns::SYNC_STATE, |k| {
            k.starts_with(&pfx) && std::str::from_utf8(k).is_ok_and(|s| s.ends_with(&suffix))
        })?;
        for k in keys.into_iter().rev() {
            if let Some((_, bytes)) = self.store.get(ns::SYNC_STATE, &k)? {
                let d: SyncDelta =
                    serde_json::from_slice(&bytes).map_err(|e| Error::Corrupt(e.to_string()))?;
                if !d.instance_id.is_empty()
                    && !spec.instance_id.is_empty()
                    && d.instance_id != spec.instance_id
                {
                    continue;
                }
                return Ok(Some(d));
            }
        }
        Ok(None)
    }
}

fn trim_retired(retired: &mut Vec<String>) {
    if retired.len() > MAX_RETIRED_INSTANCES {
        let extra = retired.len() - MAX_RETIRED_INSTANCES;
        retired.drain(0..extra);
    }
}
