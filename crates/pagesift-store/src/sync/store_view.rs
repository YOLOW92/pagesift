//! Point-in-time state reconstruction helpers for `SyncStore` (P1-06 & P1-07).

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
        if !meta.instance_id.is_empty()
            && !current_spec.instance_id.is_empty()
            && meta.instance_id != current_spec.instance_id
        {
            return Err(Error::Corrupt(format!(
                "source meta instance mismatch for '{source_id}'"
            )));
        }

        let deltas = self.list_deltas(source_id, MAX_DELTAS_PER_SOURCE)?;

        let Some(cutoff) = as_of_ms else {
            return self.build_current_view(current_spec, &meta, &deltas);
        };

        if cutoff < current_spec.created_at_ms {
            return Ok(Some(SourceStateView {
                source_id: current_spec.id,
                spec: None,
                as_of_ms: Some(cutoff),
                last_tick_id: None,
                last_synced_at_ms: None,
                total_ticks: 0,
                retained_ticks: 0,
                history_status: SourceHistoryStatus::NotCreatedYet,
                history_unavailable: false,
                coverage_status: None,
                inventory_counts: BTreeMap::new(),
                resources: Vec::new(),
                active_claims: Vec::new(),
                conflicts: Vec::new(),
                fact_clusters: Vec::new(),
                fact_clusters_truncated: false,
            }));
        }

        let (spec_at, spec_unavailable) = self.load_spec_as_of(source_id, cutoff, &current_spec)?;
        let checkpoints = self.load_checkpoints_sorted(source_id, &current_spec.instance_id)?;

        let mut inv_unavailable = false;
        let (
            resources,
            total_ticks,
            retained_ticks,
            last_tick_id,
            last_synced_at_ms,
            coverage_status,
        ) = if !checkpoints.is_empty() {
            if let Some(cp) = checkpoints
                .iter()
                .rfind(|c| c.completed_at_ms <= cutoff)
                .cloned()
            {
                let delta_gap = deltas
                    .iter()
                    .any(|d| d.completed_at_ms > cp.completed_at_ms && d.completed_at_ms <= cutoff);
                if delta_gap {
                    inv_unavailable = true;
                    (Vec::new(), 0, 0, None, None, None)
                } else {
                    let ret_ticks = deltas
                        .iter()
                        .filter(|d| d.completed_at_ms <= cutoff)
                        .count() as u64;
                    (
                        cp.resources,
                        cp.tick_seq,
                        ret_ticks,
                        Some(cp.tick_id),
                        Some(cp.completed_at_ms),
                        Some(cp.coverage_status),
                    )
                }
            } else {
                let had_ticks_by_cutoff = match meta.first_tick_at_ms {
                    Some(first) => cutoff >= first,
                    None => meta.total_ticks > checkpoints.len() as u64,
                } || deltas.iter().any(|d| d.completed_at_ms <= cutoff);
                if had_ticks_by_cutoff {
                    inv_unavailable = true;
                }
                (Vec::new(), 0, 0, None, None, None)
            }
        } else {
            // Without checkpoints (`sck:`), live inventory cannot reliably reconstruct past
            // state (deleted resources vanish and updated resources overwrite `last_checked_at_ms`).
            let filtered_deltas: Vec<&SyncDelta> = deltas
                .iter()
                .filter(|d| d.completed_at_ms <= cutoff)
                .collect();
            let live_inv = self.get_inventory(source_id)?;
            let had_pre_cutoff_activity = !filtered_deltas.is_empty()
                || meta.first_tick_at_ms.is_some_and(|first| cutoff >= first)
                || (meta.total_ticks > 0 && meta.first_tick_at_ms.is_none())
                || live_inv.iter().any(|r| r.discovered_at_ms <= cutoff);
            if had_pre_cutoff_activity {
                inv_unavailable = true;
            }
            (Vec::new(), 0, 0, None, None, None)
        };

        let mut history_unavailable = spec_unavailable || inv_unavailable;
        let mut inventory_counts = BTreeMap::new();
        for r in &resources {
            *inventory_counts
                .entry(r.state.as_str().to_string())
                .or_insert(0) += 1;
        }

        let gs = self.store.graph_for(self.tenant);
        let mut active_claims = Vec::new();
        if !history_unavailable {
            for r in &resources {
                let res = gs.claims_for_resource_as_of_checked(&r.resource_key, Some(cutoff))?;
                history_unavailable |= res.history_unavailable;
                for c in res.claims {
                    if c.status.is_active_or_unverified() {
                        active_claims.push(c);
                    }
                }
            }
        }
        let (conflicts, fact_clusters, fact_clusters_truncated) =
            gs.build_conflicts_and_clusters_for_lifecycles_checked(&active_claims, Some(cutoff))?;
        let history_status = if history_unavailable {
            SourceHistoryStatus::HistoryUnavailable
        } else {
            SourceHistoryStatus::Complete
        };

        Ok(Some(SourceStateView {
            source_id: current_spec.id,
            spec: spec_at,
            as_of_ms: Some(cutoff),
            last_tick_id,
            last_synced_at_ms,
            total_ticks,
            retained_ticks,
            history_status,
            history_unavailable,
            coverage_status,
            inventory_counts,
            resources,
            active_claims,
            conflicts,
            fact_clusters,
            fact_clusters_truncated,
        }))
    }

    fn build_current_view(
        &self,
        current_spec: DataSourceSpec,
        meta: &SourceMeta,
        deltas: &[SyncDelta],
    ) -> Result<Option<SourceStateView>> {
        let inv = self.get_inventory(&current_spec.id)?;
        let latest_delta = deltas.first();
        let total = meta
            .total_ticks
            .max(latest_delta.map_or(0, |d| d.tick_seq))
            .max(deltas.len() as u64);
        let mut inventory_counts = BTreeMap::new();
        for r in &inv {
            *inventory_counts
                .entry(r.state.as_str().to_string())
                .or_insert(0) += 1;
        }

        let gs = self.store.graph_for(self.tenant);
        let mut active_claims = Vec::new();
        for r in &inv {
            let res = gs.claims_for_resource_as_of_checked(&r.resource_key, None)?;
            for c in res.claims {
                if c.status.is_active_or_unverified() {
                    active_claims.push(c);
                }
            }
        }
        let (conflicts, fact_clusters, fact_clusters_truncated) =
            gs.build_conflicts_and_clusters_for_lifecycles_checked(&active_claims, None)?;

        Ok(Some(SourceStateView {
            source_id: current_spec.id.clone(),
            spec: Some(current_spec),
            as_of_ms: None,
            last_tick_id: latest_delta.map(|d| d.tick_id.clone()),
            last_synced_at_ms: latest_delta.map(|d| d.completed_at_ms),
            total_ticks: total,
            retained_ticks: deltas.len() as u64,
            history_status: SourceHistoryStatus::Complete,
            history_unavailable: false,
            coverage_status: latest_delta.map(|d| d.coverage_status.clone()),
            inventory_counts,
            resources: inv,
            active_claims,
            conflicts,
            fact_clusters,
            fact_clusters_truncated,
        }))
    }

    fn load_checkpoints_sorted(
        &self,
        source_id: &str,
        instance_id: &str,
    ) -> Result<Vec<SourceStateCheckpoint>> {
        let pfx = self.key("sck:", &format!("{source_id}:"));
        let keys = self.store.keys(ns::SYNC_STATE, |k| k.starts_with(&pfx))?;
        let mut out = Vec::new();
        for k in keys {
            if let Some((_, raw)) = self.store.get(ns::SYNC_STATE, &k)? {
                let cp: SourceStateCheckpoint =
                    serde_json::from_slice(&raw).map_err(|e| Error::Corrupt(e.to_string()))?;
                if !cp.instance_id.is_empty()
                    && !instance_id.is_empty()
                    && cp.instance_id != instance_id
                {
                    continue;
                }
                out.push(cp);
            }
        }
        out.sort_by_key(|c| (c.completed_at_ms, c.tick_seq));
        Ok(out)
    }

    fn load_spec_as_of(
        &self,
        source_id: &str,
        cutoff_ms: u64,
        current_spec: &DataSourceSpec,
    ) -> Result<(Option<DataSourceSpec>, bool)> {
        let pfx = self.key("srev:", &format!("{source_id}:"));
        let keys = self.store.keys(ns::SYNC_STATE, |k| k.starts_with(&pfx))?;
        let mut revs = Vec::new();
        for k in keys {
            if let Some((_, raw)) = self.store.get(ns::SYNC_STATE, &k)? {
                let rev: SourceSpecRevision =
                    serde_json::from_slice(&raw).map_err(|e| Error::Corrupt(e.to_string()))?;
                if !rev.spec.instance_id.is_empty()
                    && !current_spec.instance_id.is_empty()
                    && rev.spec.instance_id != current_spec.instance_id
                {
                    continue;
                }
                revs.push(rev);
            }
        }
        revs.sort_by_key(|r| (r.updated_at_ms, r.revision));

        if let Some(best) = revs
            .iter()
            .rfind(|r| r.updated_at_ms <= cutoff_ms && r.spec.created_at_ms <= cutoff_ms)
        {
            return Ok((Some(best.spec.clone()), false));
        }

        // No historical revision `<= cutoff_ms` exists even though `cutoff_ms >= created_at_ms`.
        // Only treat `current_spec` as valid if it was never modified after `cutoff_ms` and no
        // revision history indicates unrecoverable edits.
        if current_spec.created_at_ms <= cutoff_ms
            && current_spec.updated_at_ms <= cutoff_ms
            && revs.is_empty()
        {
            return Ok((Some(current_spec.clone()), false));
        }

        Ok((None, true))
    }
}
