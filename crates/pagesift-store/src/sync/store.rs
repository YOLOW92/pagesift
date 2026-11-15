//! `SyncStore` 存储读写与历史时刻状态重建。

use std::collections::BTreeMap;

use pagesift_graph::{ClaimStatus, detect_conflicts};

use crate::engine::ns;
use crate::{Error, Result, Store};

use super::{DataSourceSpec, MAX_DELTAS_PER_SOURCE, ResourceRecord, SourceStateView, SyncDelta};

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

    pub fn put_source(&self, spec: &DataSourceSpec) -> Result<()> {
        let bytes = serde_json::to_vec(spec).map_err(|e| Error::Corrupt(e.to_string()))?;
        self.store.put(
            ns::SYNC_STATE,
            &self.key("src:", &spec.id),
            &bytes,
            spec.updated_at_ms.max(1),
        )?;
        Ok(())
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

    pub fn delete_source(&self, source_id: &str) -> Result<bool> {
        let existed = self
            .store
            .delete(ns::SYNC_STATE, &self.key("src:", source_id))?;
        let _ = self
            .store
            .delete(ns::SYNC_STATE, &self.key("inv:", source_id));
        let _ = self
            .store
            .delete(ns::SYNC_STATE, &self.key("exp:", source_id));
        let delta_pfx = self.key("dlt:", &format!("{source_id}:"));
        if let Ok(keys) = self
            .store
            .keys(ns::SYNC_STATE, |k| k.starts_with(&delta_pfx))
        {
            for k in keys {
                let _ = self.store.delete(ns::SYNC_STATE, &k);
            }
        }
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
        let bytes = serde_json::to_vec(delta).map_err(|e| Error::Corrupt(e.to_string()))?;
        let key = self.key(
            "dlt:",
            &format!(
                "{}:{:020}:{}",
                delta.source_id, delta.completed_at_ms, delta.tick_id
            ),
        );
        self.store
            .put(ns::SYNC_STATE, &key, &bytes, delta.completed_at_ms.max(1))?;

        let pfx = self.key("dlt:", &format!("{}:", delta.source_id));
        let mut all_keys = self.store.keys(ns::SYNC_STATE, |k| k.starts_with(&pfx))?;
        if all_keys.len() > MAX_DELTAS_PER_SOURCE {
            all_keys.sort();
            let remove_count = all_keys.len() - MAX_DELTAS_PER_SOURCE;
            for old_key in all_keys.into_iter().take(remove_count) {
                let _ = self.store.delete(ns::SYNC_STATE, &old_key);
            }
        }
        Ok(())
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

    pub fn build_state_view(
        &self,
        source_id: &str,
        as_of_ms: Option<u64>,
    ) -> Result<Option<SourceStateView>> {
        let Some(spec) = self.get_source(source_id)? else {
            return Ok(None);
        };
        let mut resources = self.get_inventory(source_id)?;
        if let Some(cutoff) = as_of_ms {
            resources.retain(|r| r.discovered_at_ms <= cutoff);
        }
        let mut inventory_counts = BTreeMap::new();
        for r in &resources {
            *inventory_counts
                .entry(r.state.as_str().to_string())
                .or_insert(0) += 1;
        }

        let gs = self.store.graph_for(self.tenant);
        let mut active_claims = Vec::new();
        for r in &resources {
            if let Ok(claims) = gs.claims_for_resource_as_of(&r.resource_key, as_of_ms) {
                for c in claims {
                    if matches!(
                        c.status,
                        ClaimStatus::Active | ClaimStatus::Unverified { .. }
                    ) {
                        active_claims.push(c);
                    }
                }
            }
        }
        let raw_claims: Vec<_> = active_claims.iter().map(|c| c.claim.clone()).collect();
        let conflicts = detect_conflicts(&raw_claims);

        let deltas = self.list_deltas(source_id, MAX_DELTAS_PER_SOURCE)?;
        let filtered_deltas: Vec<&SyncDelta> = deltas
            .iter()
            .filter(|d| as_of_ms.is_none_or(|cutoff| d.completed_at_ms <= cutoff))
            .collect();
        let latest_delta = filtered_deltas.first().copied();

        Ok(Some(SourceStateView {
            spec,
            as_of_ms,
            last_tick_id: latest_delta.map(|d| d.tick_id.clone()),
            last_synced_at_ms: latest_delta.map(|d| d.completed_at_ms),
            total_ticks: filtered_deltas.len() as u64,
            coverage_status: latest_delta.map(|d| d.coverage_status.clone()),
            inventory_counts,
            resources,
            active_claims,
            conflicts,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{
        FreshnessPolicy, ResourceLifecycleState, ResourceRole, SyncBudget, SyncTickStats,
    };

    #[test]
    fn sync_store_persists_sources_inventory_deltas_and_as_of_view() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_dir(dir.path()).unwrap();
        let ss = store.sync();

        let spec = DataSourceSpec {
            id: "docs-src".to_string(),
            name: Some("Docs".to_string()),
            seeds: vec!["https://docs.example.com/".to_string()],
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            max_depth: 2,
            allow_subdomains: false,
            freshness: FreshnessPolicy::default(),
            budget: SyncBudget::default(),
            extract_schema: None,
            extract_prompt: None,
            required_fields: Vec::new(),
            allow_browser: true,
            created_at_ms: 1000,
            updated_at_ms: 1000,
        };
        ss.put_source(&spec).unwrap();
        assert_eq!(ss.list_sources().unwrap().len(), 1);

        let rec = ResourceRecord {
            resource_key: "rk1".to_string(),
            url: "https://docs.example.com/".to_string(),
            canonical_url: None,
            migrated_to: None,
            parent_url: None,
            depth: 0,
            role: ResourceRole::Seed,
            state: ResourceLifecycleState::Active,
            discovered_at_ms: 1000,
            last_checked_at_ms: Some(1500),
            last_substantive_change_at_ms: Some(1500),
            next_due_at_ms: 5100,
            etag: Some("\"v1\"".to_string()),
            last_modified: None,
            content_hash: Some("h1".to_string()),
            latest_doc_id: Some("doc1".to_string()),
            last_status_code: Some(200),
            consecutive_failures: 0,
            consecutive_404s: 0,
            importance_weight: 1.0,
            last_strategy: Some("lightweight_http".to_string()),
            title: Some("Home".to_string()),
            claim_count: 1,
        };
        ss.put_inventory("docs-src", &[rec], 1500).unwrap();

        let delta = SyncDelta {
            source_id: "docs-src".to_string(),
            tick_id: "tick-1500".to_string(),
            started_at_ms: 1400,
            completed_at_ms: 1500,
            coverage_status: "complete_within_scope".to_string(),
            coverage_reasons: vec!["all_in_scope_resources_verified".to_string()],
            stats: SyncTickStats::default(),
            added_resources: Vec::new(),
            modified_resources: Vec::new(),
            migrated_resources: Vec::new(),
            disappeared_resources: Vec::new(),
            claim_transitions: Vec::new(),
            conflicts: Vec::new(),
            adapt_decisions: Vec::new(),
        };
        ss.append_delta(&delta).unwrap();
        assert_eq!(
            ss.get_delta("docs-src", "tick-1500")
                .unwrap()
                .unwrap()
                .tick_id,
            "tick-1500"
        );

        let view = ss.build_state_view("docs-src", None).unwrap().unwrap();
        assert_eq!(view.total_ticks, 1);
        assert_eq!(view.inventory_counts.get("active"), Some(&1));

        let early_view = ss.build_state_view("docs-src", Some(500)).unwrap().unwrap();
        assert_eq!(early_view.total_ticks, 0);
        assert!(early_view.resources.is_empty());

        assert!(ss.delete_source("docs-src").unwrap());
        assert!(ss.get_source("docs-src").unwrap().is_none());
    }
}
