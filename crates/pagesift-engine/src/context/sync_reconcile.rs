//! Incremental reconciliation from 1.5 Loop `SyncStore` & `GraphStore` into 1.6 `ContextStore`
//! executing on `pagesift-flow::Pool`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use pagesift_api::{ApiError, ErrorCode};
use pagesift_extract::context::{
    ContextIndex, UnitBuildInput, extract_units_from_document, reconcile_resource_units,
};
use pagesift_graph::context::{
    AssetSpecStamp, ContextDelta, IndexVisibility, KnowledgeUnit, MaintenanceMode, UnitMatchMode,
};
use pagesift_graph::{
    ClaimRecord, DocNode, ObservationOutcome, SourceKind, TemporalSemantics, host_of, sha256_hex,
};
use pagesift_store::{
    ContextCommitOutcome, ContextCommitRequest, LoadedVersionSnapshot, ResourceLifecycleState,
    ResourceRecord, now_store_ms,
};

use crate::context::ingest::{affinity_key, summarize_batch_outcome};
use crate::engine::{Scraper, cpu_task_keyed};

enum SyncResourceWork {
    Normal {
        rec: Box<ResourceRecord>,
        prev_units: Vec<KnowledgeUnit>,
        snap_md: Option<String>,
        snap_kind: SourceKind,
        claims: Vec<ClaimRecord>,
        origin_root: String,
        origin_indep: bool,
    },
    Reconfirmed304 {
        resource_key: String,
        observed_at_ms: u64,
        prev_units: Vec<KnowledgeUnit>,
    },
    PrunedOutOfScope {
        resource_key: String,
        prev_units: Vec<KnowledgeUnit>,
    },
}

/// Incrementally catch up `ContextStore` for a persistent Loop `source_id` from `SyncStore` & `GraphStore`.
pub async fn sync_source_context(
    scraper: &Scraper,
    tenant: Option<&str>,
    source_id: &str,
    match_mode: UnitMatchMode,
    spec_override: Option<AssetSpecStamp>,
) -> Result<(ContextDelta, IndexVisibility), ApiError> {
    let store = scraper
        .store_ref()
        .cloned()
        .ok_or_else(|| ApiError::invalid("context sync requires persistent store configured"))?;

    // Per-source single-flight mutex prevents concurrent sync/ingest TOCTOU overwrites
    let src_lock = scraper.context_source_lock(tenant, source_id);
    let _src_guard = src_lock.lock().await;

    let tenant_owned = tenant.map(str::to_owned);
    let sid = source_id.to_owned();
    let target_spec = spec_override.unwrap_or_default();

    let st_read = store.clone();
    let t_read = tenant_owned.clone();
    let sid_read = sid.clone();
    let spec_for_read = target_spec.clone();

    // Zero-I/O in-memory cache probe validated inside `Store::blocking` against committed metadata
    let cached_probe = scraper.context_cache().peek(tenant, &sid);

    let prep = st_read
        .blocking(move |st| -> Result<_, ApiError> {
            let sync_st = st.sync_for(t_read.as_deref());
            let graph_st = st.graph_for(t_read.as_deref());
            let ctx_st = st.context_for(t_read.as_deref());

            let state_view = sync_st
                .build_state_view(&sid_read, None)
                .map_err(|e| ApiError::new(ErrorCode::Internal, format!("sync view: {e}")))?
                .ok_or_else(|| {
                    ApiError::new(
                        ErrorCode::NotFound,
                        format!("data source '{sid_read}' not found"),
                    )
                })?;
            let spec = state_view.spec.clone().ok_or_else(|| {
                ApiError::new(
                    ErrorCode::Internal,
                    "missing current source spec".to_owned(),
                )
            })?;
            let vis = ctx_st
                .visibility(&sid_read)
                .map_err(|e| ApiError::new(ErrorCode::Internal, format!("visibility: {e}")))?;

            let existing_meta = ctx_st
                .get_meta(&sid_read)
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;

            let (existing_index, has_persisted_index) = if vis.instance_matched {
                if let (Some(m), Some((c_inst, c_dseq, c_tick, c_spec, arc_idx))) =
                    (&existing_meta, cached_probe)
                    && c_inst == m.instance_id
                    && c_dseq == m.latest_delta_seq
                    && c_tick == m.indexed_tick_seq
                    && c_spec == m.spec_stamp
                {
                    ((*arc_idx).clone(), true)
                } else {
                    let loaded_idx = match ctx_st.load_index_blob(&sid_read) {
                        Ok(Some(bytes)) => match serde_json::from_slice::<ContextIndex>(&bytes) {
                            Ok(parsed) => Some(parsed),
                            Err(e) => {
                                tracing::warn!(
                                    source_id = %sid_read,
                                    error = %e,
                                    "corrupt context index JSON before sync reconcile; rebuilding baseline from authoritative units"
                                );
                                None
                            }
                        },
                        Ok(None) => None,
                        Err(e) => {
                            tracing::warn!(
                                source_id = %sid_read,
                                error = %e,
                                "corrupt context index blob before sync reconcile; rebuilding baseline from authoritative units"
                            );
                            None
                        }
                    };
                    match loaded_idx {
                        Some(parsed) => (parsed, true),
                        None => {
                            let base_spec = existing_meta
                                .as_ref()
                                .map(|m| m.spec_stamp.clone())
                                .unwrap_or_else(|| spec_for_read.clone());
                            let mut rebuilt = ContextIndex::new(base_spec.clone());
                            if existing_meta
                                .as_ref()
                                .is_some_and(|m| !m.resource_keys.is_empty())
                            {
                                let all_units = ctx_st
                                    .load_all_live_units(&sid_read)
                                    .map_err(|e| {
                                        ApiError::new(ErrorCode::Internal, e.to_string())
                                    })?;
                                if !all_units.is_empty() {
                                    rebuilt.apply_updates(&all_units, &[], &base_spec);
                                }
                            }
                            (rebuilt, false)
                        }
                    }
                }
            } else {
                (ContextIndex::new(spec_for_read.clone()), false)
            };

            // Short-circuit if already caught up on the exact same instance, tick, spec, and intact index!
            if has_persisted_index
                && existing_meta.as_ref().is_some_and(|m| {
                    m.instance_id == spec.instance_id
                        && m.indexed_tick_seq == state_view.total_ticks
                        && m.spec_stamp == spec_for_read
                })
            {
                let latest_delta = ctx_st
                    .list_deltas(&sid_read, None, 1)
                    .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
                    .pop()
                    .unwrap_or(ContextDelta {
                        source_id: sid_read.clone(),
                        instance_id: spec.instance_id.clone(),
                        delta_seq: vis.delta_seq,
                        store_tick_seq: state_view.total_ticks,
                        created_at_ms: now_store_ms(),
                        observation_outcome: ObservationOutcome::NotModified304,
                        unit_transitions: Vec::new(),
                        invalidated_assets: Vec::new(),
                        // [推导] 每个存活单元挂载 4 类核心派生资产（LexicalIndex/SemanticSketch/StructureIndex/GraphIndex）
                        reused_asset_count: existing_index.units.len() * 4,
                        recomputed_asset_count: 0,
                        maintenance_mode: MaintenanceMode::Incremental,
                        knowledge_delta: None,
                    });
                return Ok(Err((latest_delta, vis)));
            }

            let (lineage_map, _) = graph_st
                .load_lineage_and_citations(None)
                .map_err(|e| ApiError::new(ErrorCode::Internal, format!("lineage: {e}")))?;

            let mut work_items: Vec<SyncResourceWork> = Vec::new();
            let mut active_inv_keys: BTreeSet<String> = BTreeSet::new();

            for rec in &state_view.resources {
                active_inv_keys.insert(rec.resource_key.clone());
                let prev_units = if vis.instance_matched {
                    ctx_st
                        .load_resource_units(&sid_read, &rec.resource_key)
                        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
                } else {
                    Vec::new()
                };

                let already_indexed_doc = prev_units.first().map(|u| u.location.doc_id.as_str());
                let prev_obs_ms = prev_units
                    .iter()
                    .map(|u| u.observed_at_ms)
                    .max()
                    .unwrap_or(0);
                let latest_obs_ms = rec.last_valid_observed_at_ms.unwrap_or(0);

                if matches!(
                    rec.state,
                    ResourceLifecycleState::Active | ResourceLifecycleState::Migrated
                ) && !rec.learn.last_observation_unreliable
                    && rec.latest_doc_id.as_deref().is_some()
                    && already_indexed_doc == rec.latest_doc_id.as_deref()
                    && prev_units.iter().all(|u| u.status.is_verified_active())
                {
                    if latest_obs_ms <= prev_obs_ms {
                        continue;
                    }
                    // Resource hit 304 Not Modified in a newer tick: refresh `observed_at_ms` without reading snapshot!
                    work_items.push(SyncResourceWork::Reconfirmed304 {
                        resource_key: rec.resource_key.clone(),
                        observed_at_ms: latest_obs_ms,
                        prev_units,
                    });
                    continue;
                }

                let mut snap_md: Option<String> = None;
                if matches!(
                    rec.state,
                    ResourceLifecycleState::Active | ResourceLifecycleState::Migrated
                ) && !rec.learn.last_observation_unreliable
                {
                    let hist = graph_st
                        .history_as_of(&rec.resource_key, None, 1)
                        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
                    if let Some(vrec) = hist.versions.first()
                        && let LoadedVersionSnapshot::Markdown(md) = graph_st
                            .load_version_snapshot(vrec)
                            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
                    {
                        snap_md = Some(md);
                    }
                }
                let snap_kind = infer_sync_source_kind(&rec.url, snap_md.as_deref());

                let claims_for_res: Vec<_> = state_view
                    .active_claims
                    .iter()
                    .filter(|c| c.claim.resource_key == rec.resource_key)
                    .map(|c| c.claim.clone())
                    .collect();

                let lin = lineage_map.get(&rec.resource_key);
                let origin_root = lin.map(|l| l.origin_root_key.clone()).unwrap_or_else(|| {
                    let h = host_of(&rec.url);
                    if h.is_empty() {
                        rec.resource_key.clone()
                    } else {
                        h
                    }
                });
                let origin_indep = lin.is_none_or(|l| l.independent);

                work_items.push(SyncResourceWork::Normal {
                    rec: Box::new(rec.clone()),
                    prev_units,
                    snap_md,
                    snap_kind,
                    claims: claims_for_res,
                    origin_root,
                    origin_indep,
                });
            }

            // Purge any previously indexed resources that were pruned out of scope from SyncStore inventory!
            if vis.instance_matched
                && let Some(meta) = &existing_meta
            {
                for old_rk in &meta.resource_keys {
                    if !active_inv_keys.contains(old_rk) {
                        let prev_units = ctx_st
                            .load_resource_units(&sid_read, old_rk)
                            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
                        if !prev_units.is_empty() {
                            work_items.push(SyncResourceWork::PrunedOutOfScope {
                                resource_key: old_rk.clone(),
                                prev_units,
                            });
                        }
                    }
                }
            }

            Ok(Ok((
                spec,
                state_view,
                existing_meta,
                existing_index,
                has_persisted_index,
                vis.instance_matched,
                work_items,
            )))
        })
        .await?;

    let (
        spec,
        state_view,
        existing_meta,
        mut index,
        has_persisted_index,
        instance_matched,
        work_items,
    ) = match prep {
        Err(already_done) => return Ok(already_done),
        Ok(tuple) => tuple,
    };

    // Run CPU-intensive structured scanning, UOT matching, and index update on resident Pool!
    let key = affinity_key(&sid);
    let target_spec_cpu = target_spec.clone();
    let sid_cpu = sid.clone();
    let inst_cpu = spec.instance_id.clone();
    let total_ticks = state_view.total_ticks;

    let (updated_res_map, all_retired, maybe_index_bytes, index_arc, delta) =
        cpu_task_keyed(key, move || {
            let now = now_store_ms();
            let prev_spec_same = index.spec_stamp == target_spec_cpu;
            let mut updated_res_map: BTreeMap<String, Vec<KnowledgeUnit>> = BTreeMap::new();
            let mut all_upserts: Vec<KnowledgeUnit> = Vec::new();
            let mut reconfirmed_only: Vec<KnowledgeUnit> = Vec::new();
            let mut all_retired: Vec<KnowledgeUnit> = Vec::new();
            let mut all_transitions = Vec::new();
            let mut seen_outcomes = Vec::new();

            for item in work_items {
                match item {
                    SyncResourceWork::Reconfirmed304 {
                        resource_key,
                        observed_at_ms,
                        prev_units,
                    } => {
                        let outcome = ObservationOutcome::NotModified304;
                        let rec_out = reconcile_resource_units(
                            &prev_units,
                            Vec::new(),
                            &outcome,
                            false,
                            observed_at_ms,
                            match_mode,
                        );
                        seen_outcomes.push(outcome);
                        updated_res_map.insert(resource_key, rec_out.live_units.clone());
                        reconfirmed_only.extend(rec_out.live_units);
                        all_transitions.extend(rec_out.transitions);
                    }
                    SyncResourceWork::PrunedOutOfScope {
                        resource_key,
                        prev_units,
                    } => {
                        let outcome = ObservationOutcome::ConfirmedGone {
                            reason: "resource_pruned_out_of_scope".to_owned(),
                        };
                        let rec_out = reconcile_resource_units(
                            &prev_units,
                            Vec::new(),
                            &outcome,
                            false,
                            now,
                            match_mode,
                        );
                        seen_outcomes.push(outcome);
                        updated_res_map.insert(resource_key, Vec::new());
                        all_retired.extend(rec_out.retired_units);
                        all_transitions.extend(rec_out.transitions);
                    }
                    SyncResourceWork::Normal {
                        rec,
                        prev_units,
                        snap_md,
                        snap_kind,
                        claims,
                        origin_root,
                        origin_indep,
                    } => {
                        let outcome = match rec.state {
                            ResourceLifecycleState::ConfirmedGone => {
                                ObservationOutcome::ConfirmedGone {
                                    reason: "resource_confirmed_gone".to_owned(),
                                }
                            }
                            ResourceLifecycleState::SuspectedGone => {
                                ObservationOutcome::SuspectedGone {
                                    status_code: rec.last_status_code,
                                }
                            }
                            ResourceLifecycleState::TransientFailure => {
                                ObservationOutcome::TransientFetchError {
                                    error: "transient_failure".to_owned(),
                                }
                            }
                            ResourceLifecycleState::Discovered => continue,
                            ResourceLifecycleState::Active | ResourceLifecycleState::Migrated => {
                                if rec.learn.last_observation_unreliable {
                                    ObservationOutcome::SuspectEmptyOrBarrier {
                                        reason: "unreliable_observation".to_owned(),
                                    }
                                } else if snap_md.is_some() {
                                    ObservationOutcome::VerifiedContent
                                } else if !prev_units.is_empty() {
                                    ObservationOutcome::NotModified304
                                } else {
                                    continue;
                                }
                            }
                        };

                        let obs_ms = rec.last_valid_observed_at_ms.unwrap_or(now);
                        let fresh_units = if let Some(md) = &snap_md {
                            let doc_id = rec.latest_doc_id.clone().unwrap_or_else(|| {
                                format!("doc_{}", &sha256_hex(md.as_bytes())[..16])
                            });
                            let node = DocNode {
                                doc_id,
                                resource_key: rec.resource_key.clone(),
                                requested_url: rec.url.clone(),
                                final_url: rec.url.clone(),
                                canonical_url: rec.canonical_url.clone(),
                                source_kind: snap_kind,
                                observed_at_ms: obs_ms,
                                status_code: rec.last_status_code.unwrap_or(200), // [协议] HTTP 200 OK 默认状态码
                                engine: rec
                                    .last_strategy
                                    .clone()
                                    .unwrap_or_else(|| "http".to_owned()),
                                content_hash: rec.content_hash.clone(),
                                raw_hash: None,
                                text_hash: None,
                                etag: rec.etag.clone(),
                                last_modified: rec.last_modified.clone(),
                                title: rec.title.clone(),
                            };
                            let temp = TemporalSemantics {
                                observed_at_ms: obs_ms,
                                published_at: None,
                                published_at_ms: None,
                                modified_at: rec.last_modified.clone(),
                                modified_at_ms: rec.last_modified_at_ms,
                                valid_from: None,
                                valid_until: None,
                            };
                            extract_units_from_document(&UnitBuildInput {
                                doc_node: &node,
                                markdown: md,
                                subject_key: None,
                                temporal: Some(&temp),
                                blocks: &[],
                                claims: &claims,
                                evidence: &[],
                                origin_root: &origin_root,
                                origin_independent: origin_indep,
                                max_table_unit_tokens: 512, // [策略] 默认大表格行分片软上限 512 tokens
                            })
                        } else {
                            Vec::new()
                        };

                        let reconcile_ts = if matches!(outcome, ObservationOutcome::NotModified304)
                        {
                            obs_ms
                        } else {
                            now
                        };
                        let rec_out = reconcile_resource_units(
                            &prev_units,
                            fresh_units,
                            &outcome,
                            rec.learn.last_observation_unreliable,
                            reconcile_ts,
                            match_mode,
                        );
                        seen_outcomes.push(outcome);
                        updated_res_map
                            .insert(rec.resource_key.clone(), rec_out.live_units.clone());
                        all_upserts.extend(rec_out.live_units);
                        all_retired.extend(rec_out.retired_units);
                        all_transitions.extend(rec_out.transitions);
                    }
                }
            }

            // Apply 304 timestamp/status refreshes in-place when unit already exists in `index`
            // so `304 Not Modified` never inflates churn ratio or falsely triggers `FullRebuild`!
            let mut touched_304_in_place = false;
            for u in reconfirmed_only {
                if prev_spec_same && let Some(existing) = index.units.get_mut(&u.unit_id) {
                    existing.observed_at_ms = u.observed_at_ms;
                    existing.status = u.status;
                    touched_304_in_place = true;
                } else {
                    all_upserts.push(u);
                }
            }

            // Skip full-index JSON serialization & zstd disk rewrite when zero units/specs changed!
            let no_substantive_index_mutation = instance_matched
                && has_persisted_index
                && prev_spec_same
                && all_upserts.is_empty()
                && all_retired.is_empty();

            let (reused_assets, recomputed_assets, invalidated_assets, mode, maybe_index_bytes) =
                if no_substantive_index_mutation {
                    let bytes = if touched_304_in_place {
                        Some(serde_json::to_vec(&index).map_err(|e| {
                            ApiError::new(
                                ErrorCode::Internal,
                                format!("serialize context index: {e}"),
                            )
                        })?)
                    } else {
                        None
                    };
                    (
                        // [协议] PageSift 1.6 §5.2：每个单元对应 4 类派生资产（词法倒排、符号锚点、草图向量、证据绑定）
                        index.units.len() * 4,
                        0,
                        Vec::new(),
                        MaintenanceMode::Incremental,
                        bytes,
                    )
                } else {
                    let retired_ids: Vec<String> =
                        all_retired.iter().map(|u| u.unit_id.clone()).collect();
                    let idx_summary =
                        index.apply_updates(&all_upserts, &retired_ids, &target_spec_cpu);
                    let bytes = serde_json::to_vec(&index).map_err(|e| {
                        ApiError::new(ErrorCode::Internal, format!("serialize context index: {e}"))
                    })?;
                    (
                        idx_summary.reused_asset_count,
                        idx_summary.recomputed_asset_count,
                        idx_summary.invalidated_assets,
                        idx_summary.mode,
                        Some(bytes),
                    )
                };

            let batch_outcome = summarize_batch_outcome(&seen_outcomes);
            let delta = ContextDelta {
                source_id: sid_cpu,
                instance_id: inst_cpu,
                delta_seq: 0,
                store_tick_seq: total_ticks,
                created_at_ms: now,
                observation_outcome: batch_outcome,
                unit_transitions: all_transitions,
                invalidated_assets,
                reused_asset_count: reused_assets,
                recomputed_asset_count: recomputed_assets,
                maintenance_mode: mode,
                knowledge_delta: None,
            };
            Ok::<_, ApiError>((
                updated_res_map,
                all_retired,
                maybe_index_bytes,
                Arc::new(index),
                delta,
            ))
        })
        .await??;

    // Atomically commit updated units, optional index blob, and ContextDelta
    let inst_commit = spec.instance_id.clone();
    let allow_zero = total_ticks == 0 && existing_meta.is_none();
    let t_commit = tenant_owned.clone();
    let sid_commit = sid.clone();
    let (committed_delta, vis, saved_meta_opt) = store
        .blocking(move |st| -> Result<_, ApiError> {
            let ctx_st = st.context_for(t_commit.as_deref());
            let mut committed_delta = delta.clone();
            let out = ctx_st
                .commit_delta(ContextCommitRequest {
                    source_id: &sid_commit,
                    instance_id: &inst_commit,
                    store_tick_seq: total_ticks,
                    spec_stamp: target_spec,
                    resource_units: &updated_res_map,
                    retired_units: &all_retired,
                    index_blob: maybe_index_bytes.as_deref(),
                    delta,
                    allow_same_zero_tick: allow_zero,
                })
                .map_err(|e| ApiError::new(ErrorCode::Internal, format!("commit context: {e}")))?;
            let (seq, committed) = match out {
                ContextCommitOutcome::Committed { delta_seq }
                | ContextCommitOutcome::InstanceReplacedAndCommitted { delta_seq } => {
                    (delta_seq, true)
                }
                ContextCommitOutcome::IdempotentReplaySkipped { existing_delta_seq } => {
                    (existing_delta_seq, false)
                }
            };
            committed_delta.delta_seq = seq;
            committed_delta.instance_id = inst_commit;
            let vis = ctx_st
                .visibility(&sid_commit)
                .map_err(|e| ApiError::new(ErrorCode::Internal, format!("visibility: {e}")))?;
            let saved_meta = if committed {
                ctx_st.get_meta(&sid_commit).ok().flatten()
            } else {
                None
            };
            Ok((committed_delta, vis, saved_meta))
        })
        .await?;

    if let Some(meta) = saved_meta_opt {
        scraper.context_cache().put(tenant, &meta, index_arc);
    }

    Ok((committed_delta, vis))
}

pub(crate) fn infer_sync_source_kind(url: &str, snap_md: Option<&str>) -> SourceKind {
    let path_part = url.split(['?', '#']).next().unwrap_or(url);
    let lower = path_part.to_ascii_lowercase();
    if lower.ends_with(".pdf") {
        return SourceKind::Pdf;
    }
    for ext in ["docx", "xlsx", "pptx", "epub", "csv"] {
        if lower.ends_with(&format!(".{ext}")) {
            return SourceKind::Office {
                format: ext.to_owned(),
            };
        }
    }
    if snap_md.is_some_and(|m| m.contains("<!-- page ")) {
        return SourceKind::Pdf;
    }
    SourceKind::Html
}
