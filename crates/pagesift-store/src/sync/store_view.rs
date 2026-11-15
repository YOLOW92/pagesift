//! Point-in-time state reconstruction helpers for `SyncStore`.

use super::*;

impl<'a> SyncStore<'a> {
    pub fn build_state_view(
        &self,
        source_id: &str,
        as_of_ms: Option<u64>,
    ) -> Result<Option<SourceStateView>> {
        let Some(current_spec) = self.get_source(source_id)? else {
            return Ok(None);
        };
        let meta = match self
            .store
            .get(ns::SYNC_STATE, &self.key("smeta:", source_id))?
        {
            Some((_, raw)) => serde_json::from_slice::<SourceMeta>(&raw)
                .map_err(|e| Error::Corrupt(e.to_string()))?,
            None => SourceMeta::default(),
        };

        let delta_keys = self.store.keys(ns::SYNC_STATE, |k| {
            k.starts_with(&self.key("dlt:", &format!("{source_id}:")))
        })?;
        let deltas = self.list_deltas(source_id, MAX_DELTAS_PER_SOURCE)?;

        let mut history_unavailable = false;
        let (
            spec,
            resources,
            total_ticks,
            retained_ticks,
            last_tick_id,
            last_synced_at_ms,
            coverage_status,
        ) = if let Some(cutoff) = as_of_ms {
            let spec_at = self.load_spec_as_of(source_id, cutoff, &current_spec)?;
            let mut cp_keys = self.store.keys(ns::SYNC_STATE, |k| {
                k.starts_with(&self.key("sck:", &format!("{source_id}:")))
            })?;
            cp_keys.sort();

            if !cp_keys.is_empty() {
                let mut chosen: Option<SourceStateCheckpoint> = None;
                for k in cp_keys.iter().rev() {
                    if let Some((_, raw)) = self.store.get(ns::SYNC_STATE, k)?
                        && let Ok(cp) = serde_json::from_slice::<SourceStateCheckpoint>(&raw)
                        && cp.completed_at_ms <= cutoff
                    {
                        chosen = Some(cp);
                        break;
                    }
                }
                if let Some(cp) = chosen {
                    let ret_ticks = deltas
                        .iter()
                        .filter(|d| d.completed_at_ms <= cutoff)
                        .count() as u64;
                    (
                        spec_at,
                        cp.resources,
                        cp.tick_seq,
                        ret_ticks,
                        Some(cp.tick_id),
                        Some(cp.completed_at_ms),
                        Some(cp.coverage_status),
                    )
                } else {
                    // 无 cutoff 之前检查点：若最早周期就在 cutoff 之前或等于 cutoff 则说明被裁剪，否则该时刻尚未执行过同步
                    if meta.first_tick_at_ms.is_some_and(|first| cutoff >= first)
                        && meta.total_ticks > 0
                    {
                        history_unavailable = true;
                    }
                    (spec_at, Vec::new(), 0, 0, None, None, None)
                }
            } else {
                // 兼容直接写 put_inventory + append_delta（无检查点）的历史数据与单测路径
                let filtered_deltas: Vec<&SyncDelta> = deltas
                    .iter()
                    .filter(|d| d.completed_at_ms <= cutoff)
                    .collect();
                let latest_delta = filtered_deltas.first().copied();
                let mut inv = self.get_inventory(source_id)?;
                inv.retain(|r| {
                    r.discovered_at_ms <= cutoff && r.last_checked_at_ms.is_none_or(|t| t <= cutoff)
                });
                if filtered_deltas.is_empty() && meta.total_ticks == 0 {
                    inv.clear();
                }
                let total = latest_delta
                    .map(|d| d.tick_seq.max(filtered_deltas.len() as u64))
                    .unwrap_or(0);
                (
                    spec_at,
                    inv,
                    total,
                    filtered_deltas.len() as u64,
                    latest_delta.map(|d| d.tick_id.clone()),
                    latest_delta.map(|d| d.completed_at_ms),
                    latest_delta.map(|d| d.coverage_status.clone()),
                )
            }
        } else {
            let latest_delta = deltas.first();
            let inv = self.get_inventory(source_id)?;
            let total = meta
                .total_ticks
                .max(latest_delta.map_or(0, |d| d.tick_seq))
                .max(delta_keys.len() as u64);
            (
                current_spec,
                inv,
                total,
                delta_keys.len() as u64,
                latest_delta.map(|d| d.tick_id.clone()),
                latest_delta.map(|d| d.completed_at_ms),
                latest_delta.map(|d| d.coverage_status.clone()),
            )
        };

        let mut inventory_counts = BTreeMap::new();
        for r in &resources {
            *inventory_counts
                .entry(r.state.as_str().to_string())
                .or_insert(0) += 1;
        }

        let gs = self.store.graph_for(self.tenant);
        let mut active_claims = Vec::new();
        for r in &resources {
            if let Ok(res) = gs.claims_for_resource_as_of_checked(&r.resource_key, as_of_ms) {
                if as_of_ms.is_some() {
                    history_unavailable |= res.history_unavailable;
                }
                for c in res.claims {
                    if c.status.is_active_or_unverified() {
                        active_claims.push(c);
                    }
                }
            }
        }
        let raw_claims: Vec<_> = active_claims.iter().map(|c| c.claim.clone()).collect();
        let conflicts = gs.detect_conflicts_for_claims(&raw_claims, as_of_ms)?;

        Ok(Some(SourceStateView {
            spec,
            as_of_ms,
            last_tick_id,
            last_synced_at_ms,
            total_ticks,
            retained_ticks,
            history_unavailable,
            coverage_status,
            inventory_counts,
            resources,
            active_claims,
            conflicts,
        }))
    }

    fn load_spec_as_of(
        &self,
        source_id: &str,
        cutoff_ms: u64,
        fallback_current: &DataSourceSpec,
    ) -> Result<DataSourceSpec> {
        let pfx = self.key("srev:", &format!("{source_id}:"));
        let mut keys = self.store.keys(ns::SYNC_STATE, |k| k.starts_with(&pfx))?;
        keys.sort();
        let mut earliest: Option<SourceSpecRevision> = None;
        let mut best: Option<SourceSpecRevision> = None;
        for k in keys {
            if let Some((_, raw)) = self.store.get(ns::SYNC_STATE, &k)?
                && let Ok(rev) = serde_json::from_slice::<SourceSpecRevision>(&raw)
            {
                if earliest.is_none() {
                    earliest = Some(rev.clone());
                }
                if rev.updated_at_ms <= cutoff_ms {
                    best = Some(rev);
                }
            }
        }
        Ok(best
            .map(|b| b.spec)
            .or_else(|| earliest.map(|e| e.spec))
            .unwrap_or_else(|| fallback_current.clone()))
    }
}
