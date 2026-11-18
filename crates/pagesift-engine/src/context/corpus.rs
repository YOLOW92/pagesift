//! Multi-source [`LoadedContextCorpus`] assembly with zero-copy in-process cache hits,
//! exact `as_of_ms` point-in-time historical replay, and ABA/deleted-source ghost filtering.

use std::sync::Arc;

use pagesift_api::{ApiError, ErrorCode};
use pagesift_extract::context::{ContextIndex, UnitBuildInput, extract_units_from_document};
use pagesift_graph::context::{AssetSpecStamp, IndexVisibility, KnowledgeUnit, UnitMatchMode};
use pagesift_graph::{
    ClaimRecord, ConflictRecord, DocNode, FactCluster, SourceKind, TemporalSemantics, host_of,
    sha256_hex,
};
use pagesift_store::{LoadedVersionSnapshot, ResourceLifecycleState};

use crate::context::ingest::affinity_key;
use crate::context::sync_reconcile::{infer_sync_source_kind, sync_source_context};
use crate::engine::{Scraper, cpu_task_keyed};

struct HistoricalSnapshotDoc {
    resource_key: String,
    url: String,
    observed_at_ms: u64,
    status_code: u16,
    markdown: String,
    source_kind: SourceKind,
    claims: Vec<ClaimRecord>,
    origin_root: String,
    origin_indep: bool,
}

struct BlockingCorpusLoad {
    maybe_index_arc: Option<Arc<ContextIndex>>,
    multi_units: Vec<KnowledgeUnit>,
    fallback_hist_docs: Vec<HistoricalSnapshotDoc>,
    conflicts: Vec<ConflictRecord>,
    fact_clusters: Vec<FactCluster>,
    visibilities: Vec<IndexVisibility>,
    newly_loaded_single: Option<(pagesift_store::ContextSourceMeta, Arc<ContextIndex>)>,
    invalidated_single: Option<String>,
}

/// Loaded multi-source bundle ready for Search and Pack (`Arc<ContextIndex>` avoids deep clones).
pub struct LoadedContextCorpus {
    pub index: Arc<ContextIndex>,
    pub conflicts: Vec<ConflictRecord>,
    pub fact_clusters: Vec<FactCluster>,
    pub visibilities: Vec<IndexVisibility>,
}

/// Load a unified [`LoadedContextCorpus`] across requested `source_ids` (or all tenant sources when empty).
pub async fn load_corpus(
    scraper: &Scraper,
    tenant: Option<&str>,
    source_ids: &[String],
    as_of_ms: Option<u64>,
) -> Result<LoadedContextCorpus, ApiError> {
    let Some(store) = scraper.store_ref().cloned() else {
        return Ok(LoadedContextCorpus {
            index: Arc::new(ContextIndex::default()),
            conflicts: Vec::new(),
            fact_clusters: Vec::new(),
            visibilities: Vec::new(),
        });
    };

    // Auto-catchup Loop sources that are behind `SyncStore` when not querying historical `as_of_ms`
    if as_of_ms.is_none() {
        let any_sync = if source_ids.is_empty() {
            true
        } else {
            let ctx_probe = store.context_for(tenant);
            source_ids
                .iter()
                .any(|sid| ctx_probe.has_sync_source(sid).unwrap_or(false))
        };
        let catchup_candidates: Vec<String> = if !any_sync {
            Vec::new()
        } else if source_ids.is_empty() {
            let t_owned = tenant.map(str::to_owned);
            store
                .blocking(move |st| -> Vec<String> {
                    let ctx_st = st.context_for(t_owned.as_deref());
                    let sync_st = st.sync_for(t_owned.as_deref());
                    sync_st
                        .list_sources()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|s| {
                            ctx_st.has_meta(&s.id).unwrap_or(false)
                                && ctx_st.visibility(&s.id).is_ok_and(|v| !v.is_caught_up)
                        })
                        .map(|s| s.id)
                        .collect()
                })
                .await
        } else {
            let t_owned = tenant.map(str::to_owned);
            let sids_chk = source_ids.to_vec();
            store
                .blocking(move |st| -> Vec<String> {
                    let ctx_st = st.context_for(t_owned.as_deref());
                    sids_chk
                        .into_iter()
                        .filter(|sid| {
                            ctx_st.has_sync_source(sid).unwrap_or(false)
                                && ctx_st.visibility(sid).map_or(true, |v| !v.is_caught_up)
                        })
                        .collect()
                })
                .await
        };
        for sid in catchup_candidates {
            let _ =
                sync_source_context(scraper, tenant, &sid, UnitMatchMode::BoundedUot, None).await;
        }
    }

    let tenant_owned = tenant.map(str::to_owned);
    let req_sids = source_ids.to_vec();

    // Zero-I/O in-memory cache probe ONLY when `as_of_ms` is None (historical queries must replay point-in-time state!)
    let single_cached_probe = if as_of_ms.is_none() && req_sids.len() == 1 {
        scraper.context_cache().peek(tenant, &req_sids[0])
    } else {
        None
    };

    let loaded = store
        .blocking(move |st| -> Result<BlockingCorpusLoad, ApiError> {
            let ctx_st = st.context_for(tenant_owned.as_deref());
            let graph_st = st.graph_for(tenant_owned.as_deref());
            let sync_st = st.sync_for(tenant_owned.as_deref());

            let target_sids: Vec<String> = if req_sids.is_empty() {
                let mut sids: Vec<String> = ctx_st
                    .list_indexed_sources()
                    .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
                    .into_iter()
                    .map(|m| m.source_id)
                    .collect();
                if as_of_ms.is_some() {
                    for spec in sync_st.list_sources().unwrap_or_default() {
                        if !sids.contains(&spec.id) {
                            sids.push(spec.id);
                        }
                    }
                }
                sids
            } else {
                req_sids
            };

            let mut visibilities = Vec::with_capacity(target_sids.len());
            let mut fact_clusters = Vec::new();
            let mut newly_loaded_single = None;
            let mut invalidated_single = None;
            let mut fallback_hist_docs = Vec::new();

            let (maybe_index_arc, multi_units) = if let Some(cutoff) = as_of_ms {
                // Point-in-time historical replay (`as_of_ms = Some(cutoff)`): NEVER read latest `cidx` or cache!
                let mut all_units = Vec::new();
                let mut lineage_cache = None;
                for sid in &target_sids {
                    let vis = ctx_st
                        .visibility(sid)
                        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
                    let is_valid = vis.instance_matched;
                    let is_loop = ctx_st.has_sync_source(sid).unwrap_or(false);
                    visibilities.push(vis);
                    let state_view_opt = if is_loop {
                        sync_st.build_state_view(sid, Some(cutoff)).ok().flatten()
                    } else {
                        None
                    };
                    if let Some(ref view) = state_view_opt {
                        fact_clusters.extend(view.fact_clusters.clone());
                    }
                    if !is_valid {
                        continue;
                    }
                    let (has_hist, hist_units) = ctx_st
                        .load_units_as_of(sid, cutoff)
                        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
                    if !hist_units.is_empty() || (has_hist && !is_loop) {
                        all_units.extend(hist_units);
                    } else if let Some(view) = state_view_opt {
                        // Fallback for Loop sources whose earlier ticks predated Context activation:
                        // reconstruct historical units from `GraphStore` version snapshots `<= cutoff`.
                        let lineage_map = lineage_cache.get_or_insert_with(|| {
                            graph_st
                                .load_lineage_and_citations(Some(cutoff))
                                .map(|(m, _)| m)
                                .unwrap_or_default()
                        });
                        for rec in &view.resources {
                            if !matches!(
                                rec.state,
                                ResourceLifecycleState::Active | ResourceLifecycleState::Migrated
                            ) || rec.learn.last_observation_unreliable
                            {
                                continue;
                            }
                            if let Ok(hist) =
                                graph_st.history_as_of(&rec.resource_key, Some(cutoff), 1)
                                && let Some(vrec) = hist.versions.first()
                                && let Ok(LoadedVersionSnapshot::Markdown(md)) =
                                    graph_st.load_version_snapshot(vrec)
                            {
                                let claims: Vec<ClaimRecord> = view
                                    .active_claims
                                    .iter()
                                    .filter(|c| c.claim.resource_key == rec.resource_key)
                                    .map(|c| c.claim.clone())
                                    .collect();
                                let lin = lineage_map.get(&rec.resource_key);
                                let origin_root =
                                    lin.map(|l| l.origin_root_key.clone()).unwrap_or_else(|| {
                                        let h = host_of(&rec.url);
                                        if h.is_empty() {
                                            rec.resource_key.clone()
                                        } else {
                                            h
                                        }
                                    });
                                let origin_indep = lin.is_none_or(|l| l.independent);
                                let source_kind = infer_sync_source_kind(&rec.url, Some(&md));
                                fallback_hist_docs.push(HistoricalSnapshotDoc {
                                    resource_key: rec.resource_key.clone(),
                                    url: rec.url.clone(),
                                    observed_at_ms: vrec.observed_at_ms,
                                    status_code: vrec.status_code,
                                    markdown: md,
                                    source_kind,
                                    claims,
                                    origin_root,
                                    origin_indep,
                                });
                            }
                        }
                    }
                }
                (None, all_units)
            } else if target_sids.len() == 1 {
                let sid = &target_sids[0];
                let vis = ctx_st
                    .visibility(sid)
                    .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
                let is_loop = ctx_st.has_sync_source(sid).unwrap_or(false);
                visibilities.push(vis.clone());
                if is_loop && let Ok(Some(view)) = sync_st.build_state_view(sid, None) {
                    fact_clusters.extend(view.fact_clusters);
                }

                let live_meta = ctx_st
                    .get_meta(sid)
                    .ok()
                    .flatten()
                    .filter(|m| ctx_st.is_meta_live(m).unwrap_or(false));

                if !vis.instance_matched || live_meta.is_none() {
                    invalidated_single = Some(sid.clone());
                    (Some(Arc::new(ContextIndex::default())), Vec::new())
                } else if let (Some(m), Some((c_inst, c_dseq, c_tick, c_spec, arc_idx))) =
                    (&live_meta, single_cached_probe)
                    && c_inst == m.instance_id
                    && c_dseq == m.latest_delta_seq
                    && c_tick == m.indexed_tick_seq
                    && c_spec == m.spec_stamp
                {
                    (Some(arc_idx), Vec::new())
                } else if let Some(raw) = ctx_st
                    .load_index_blob(sid)
                    .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
                    && let Ok(single_idx) = serde_json::from_slice::<ContextIndex>(&raw)
                {
                    let arc = Arc::new(single_idx);
                    if let Some(meta) = live_meta {
                        newly_loaded_single = Some((meta, arc.clone()));
                    }
                    (Some(arc), Vec::new())
                } else {
                    let units = ctx_st
                        .load_all_live_units(sid)
                        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
                    (None, units)
                }
            } else {
                let mut all_units = Vec::new();
                for sid in &target_sids {
                    let vis = ctx_st
                        .visibility(sid)
                        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
                    let is_valid = vis.instance_matched;
                    let is_loop = ctx_st.has_sync_source(sid).unwrap_or(false);
                    visibilities.push(vis);
                    if is_valid {
                        let units = ctx_st
                            .load_all_live_units(sid)
                            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
                        all_units.extend(units);
                    }
                    if is_loop && let Ok(Some(view)) = sync_st.build_state_view(sid, None) {
                        fact_clusters.extend(view.fact_clusters);
                    }
                }
                (None, all_units)
            };

            let (conflicts, _, _, _) = graph_st
                .list_conflicts_as_of_checked(None, as_of_ms, 256, 0) // [策略] Context 单次关联最多加载前 256 条冲突
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;

            Ok(BlockingCorpusLoad {
                maybe_index_arc,
                multi_units,
                fallback_hist_docs,
                conflicts,
                fact_clusters,
                visibilities,
                newly_loaded_single,
                invalidated_single,
            })
        })
        .await?;

    if let Some(dead_sid) = loaded.invalidated_single {
        scraper.context_cache().invalidate(tenant, &dead_sid);
    }

    let index_arc = match loaded.maybe_index_arc {
        Some(arc) => arc,
        None if loaded.multi_units.is_empty() && loaded.fallback_hist_docs.is_empty() => {
            Arc::new(ContextIndex::default())
        }
        None => {
            let mut units = loaded.multi_units;
            let fb_docs = loaded.fallback_hist_docs;
            let key = affinity_key(tenant.unwrap_or("default"));
            cpu_task_keyed(key, move || {
                for hd in fb_docs {
                    let ch = sha256_hex(hd.markdown.as_bytes());
                    let doc_id = format!("doc_{}", &ch[..16]);
                    let node = DocNode {
                        doc_id,
                        resource_key: hd.resource_key,
                        requested_url: hd.url.clone(),
                        final_url: hd.url,
                        canonical_url: None,
                        source_kind: hd.source_kind,
                        observed_at_ms: hd.observed_at_ms,
                        status_code: hd.status_code,
                        engine: "sync_history".to_owned(),
                        content_hash: Some(ch),
                        raw_hash: None,
                        text_hash: None,
                        etag: None,
                        last_modified: None,
                        title: None,
                    };
                    let temp = TemporalSemantics {
                        observed_at_ms: hd.observed_at_ms,
                        published_at: None,
                        published_at_ms: None,
                        modified_at: None,
                        modified_at_ms: None,
                        valid_from: None,
                        valid_until: None,
                    };
                    let built = extract_units_from_document(&UnitBuildInput {
                        doc_node: &node,
                        markdown: &hd.markdown,
                        subject_key: None,
                        temporal: Some(&temp),
                        blocks: &[],
                        claims: &hd.claims,
                        evidence: &[],
                        origin_root: &hd.origin_root,
                        origin_independent: hd.origin_indep,
                        max_table_unit_tokens: 512, // [策略] 默认大表格行分片软上限 512 tokens
                    });
                    units.extend(built);
                }
                let mut merged_index = ContextIndex::default();
                merged_index.apply_updates(&units, &[], &AssetSpecStamp::default());
                Arc::new(merged_index)
            })
            .await?
        }
    };

    if let Some((meta, arc_idx)) = loaded.newly_loaded_single {
        scraper.context_cache().put(tenant, &meta, arc_idx);
    }

    Ok(LoadedContextCorpus {
        index: index_arc,
        conflicts: loaded.conflicts,
        fact_clusters: loaded.fact_clusters,
        visibilities: loaded.visibilities,
    })
}
