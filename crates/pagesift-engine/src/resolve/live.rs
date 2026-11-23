//! 1.7 Resolve Live engine: ABA-safe `ContextDelta` reconciliation with Salsa Early Cutoff,
//! negative/scope-dependency invalidation, conservative fallback on retention gaps or instance
//! replacement, `ObservationLost` separation, and incremental vs full revalidation.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use pagesift_api::{ApiError, ErrorCode, PageBudget};
use pagesift_extract::resolve::unit_scope_trigger_predicates;
use pagesift_graph::context::UnitChangeKind;
use pagesift_graph::resolve::{
    ConclusionRecord, ReconcileImpactReport, ReconcileMode, ResolutionState, ResolveCheckRequest,
    ResolveEventKind, RevalidateMode, RevalidateOutcome, VerifierMode,
};
use pagesift_store::{DeltaWindowBatch, ResolveSourceCheckpoint, now_store_ms};

use crate::engine::Scraper;
use crate::resolve::pipeline::{execute_check_with_budget, map_resolve_store_error};

// [策略] 单次 reconcile 每源单批消费最多 128 条 ContextDelta，最多翻页 16 批（2048 条）避免单次阻塞过久
const RECONCILE_DELTA_BATCH_LIMIT: usize = 128;
// [策略] 单次 reconcile 翻页硬上限 16 批
const RECONCILE_MAX_BATCH_PAGES: usize = 16;

/// Consume pending `ContextDelta` streams for `source_ids` (or all indexed Context sources if empty),
/// apply Salsa Early Cutoff and ScopeDependency negative-dependency propagation, mark impacted
/// conclusions `StalePendingRevalidation`, and advance ABA-safe source checkpoints.
pub async fn reconcile_changes(
    scraper: &Scraper,
    tenant: Option<&str>,
    source_ids: Vec<String>,
    mode: ReconcileMode,
) -> Result<ReconcileImpactReport, ApiError> {
    let store = scraper.store_ref().cloned().ok_or_else(|| {
        ApiError::invalid("resolve reconcile requires a persistent store configured on Scraper")
    })?;
    let lock = scraper.resolve_tenant_lock(tenant);
    let _guard = lock.lock().await;
    let t_owned = tenant.map(str::to_owned);

    store
        .blocking(move |st| -> Result<ReconcileImpactReport, ApiError> {
            let ctx_st = st.context_for(t_owned.as_deref());
            let res_st = st.resolve_for(t_owned.as_deref());

            let targets: Vec<String> = if source_ids.is_empty() {
                let mut set = BTreeSet::new();
                for m in ctx_st
                    .list_indexed_sources()
                    .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
                {
                    set.insert(m.source_id);
                }
                // Also include any source_ids referenced by existing persisted conclusions so deleted
                // or ABA-replaced sources are still detected!
                let (_, all_concs) = res_st
                    .list_conclusions(None, None, usize::MAX)
                    .map_err(map_resolve_store_error)?;
                for c in all_concs {
                    for sid in c.scope.source_ids {
                        set.insert(sid);
                    }
                }
                set.into_iter().collect()
            } else {
                source_ids
            };

            let mut report = ReconcileImpactReport {
                source_ids: targets.clone(),
                ..ReconcileImpactReport::default()
            };
            let mut impacted_all = BTreeSet::new();
            let mut scope_impacted_all = BTreeSet::new();
            let mut obs_lost_all = BTreeSet::new();
            let mut early_cutoff_all = BTreeSet::new();

            for sid in &targets {
                let cp_opt = res_st
                    .get_source_checkpoint(sid)
                    .map_err(map_resolve_store_error)?;
                let mut after_seq = cp_opt.as_ref().map(|c| c.consumed_delta_seq);
                let prev_instance = cp_opt.as_ref().map(|c| c.instance_id.clone());

                if mode == ReconcileMode::FullSource {
                    let meta_opt = ctx_st
                        .get_meta(sid)
                        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
                    let affected = expand_source_conclusions(&res_st, sid)?;
                    mark_conclusions_stale(
                        &res_st,
                        &affected,
                        &format!("full_source_reconcile:{sid}"),
                        ResolveEventKind::ConclusionMarkedStale,
                        Vec::new(),
                        Vec::new(),
                    )?;
                    impacted_all.extend(affected);
                    report.full_fallback_sources.push(sid.clone());
                    if let Some(m) = meta_opt {
                        res_st
                            .put_source_checkpoint(ResolveSourceCheckpoint {
                                source_id: sid.clone(),
                                instance_id: m.instance_id,
                                consumed_delta_seq: m.latest_delta_seq,
                                updated_at_ms: now_store_ms(),
                            })
                            .map_err(map_resolve_store_error)?;
                    }
                    continue;
                }

                // Paginate delta consumption safely
                for _ in 0..RECONCILE_MAX_BATCH_PAGES {
                    let batch = ctx_st
                        .list_deltas_checked(sid, after_seq, RECONCILE_DELTA_BATCH_LIMIT)
                        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;

                    match batch {
                        DeltaWindowBatch::Missing => {
                            if prev_instance.is_some() {
                                let affected = expand_source_conclusions(&res_st, sid)?;
                                if !affected.is_empty() {
                                    mark_conclusions_stale(
                                        &res_st,
                                        &affected,
                                        &format!("source_missing_or_deleted:{sid}"),
                                        ResolveEventKind::SourceInstanceSuperseded,
                                        Vec::new(),
                                        Vec::new(),
                                    )?;
                                    impacted_all.extend(affected);
                                    report.aba_superseded_sources.push(sid.clone());
                                    report.full_fallback_sources.push(sid.clone());
                                }
                            }
                            break;
                        }
                        DeltaWindowBatch::InstanceSuperseded { instance_id } => {
                            let affected = expand_source_conclusions(&res_st, sid)?;
                            mark_conclusions_stale(
                                &res_st,
                                &affected,
                                &format!("source_instance_superseded:{sid}:{instance_id}"),
                                ResolveEventKind::SourceInstanceSuperseded,
                                Vec::new(),
                                Vec::new(),
                            )?;
                            impacted_all.extend(affected);
                            report.aba_superseded_sources.push(sid.clone());
                            report.full_fallback_sources.push(sid.clone());
                            if let Some(m) = ctx_st
                                .get_meta(sid)
                                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
                            {
                                res_st
                                    .put_source_checkpoint(ResolveSourceCheckpoint {
                                        source_id: sid.clone(),
                                        instance_id: m.instance_id,
                                        consumed_delta_seq: m.latest_delta_seq,
                                        updated_at_ms: now_store_ms(),
                                    })
                                    .map_err(map_resolve_store_error)?;
                            }
                            break;
                        }
                        DeltaWindowBatch::RetentionGap {
                            instance_id,
                            latest_seq,
                            ..
                        }
                        | DeltaWindowBatch::SequenceAheadOrReset {
                            instance_id,
                            latest_seq,
                            ..
                        } => {
                            let affected = expand_source_conclusions(&res_st, sid)?;
                            let is_aba = prev_instance
                                .as_deref()
                                .is_some_and(|p| p != instance_id.as_str());
                            let kind = if is_aba {
                                report.aba_superseded_sources.push(sid.clone());
                                ResolveEventKind::SourceInstanceSuperseded
                            } else {
                                ResolveEventKind::ConclusionMarkedStale
                            };
                            mark_conclusions_stale(
                                &res_st,
                                &affected,
                                &format!("delta_gap_or_reset_conservative_fallback:{sid}"),
                                kind,
                                Vec::new(),
                                Vec::new(),
                            )?;
                            impacted_all.extend(affected);
                            report.full_fallback_sources.push(sid.clone());
                            res_st
                                .put_source_checkpoint(ResolveSourceCheckpoint {
                                    source_id: sid.clone(),
                                    instance_id,
                                    consumed_delta_seq: latest_seq,
                                    updated_at_ms: now_store_ms(),
                                })
                                .map_err(map_resolve_store_error)?;
                            break;
                        }
                        DeltaWindowBatch::Continuous {
                            instance_id,
                            latest_seq,
                            truncated,
                            deltas,
                        } => {
                            // Detect ABA instance_id transition even if seq happened to align!
                            if let Some(ref prev_inst) = prev_instance
                                && prev_inst != &instance_id
                            {
                                let affected = expand_source_conclusions(&res_st, sid)?;
                                mark_conclusions_stale(
                                    &res_st,
                                    &affected,
                                    &format!(
                                        "aba_instance_replaced:{sid}:{prev_inst}->{instance_id}"
                                    ),
                                    ResolveEventKind::SourceInstanceSuperseded,
                                    Vec::new(),
                                    Vec::new(),
                                )?;
                                impacted_all.extend(affected);
                                report.aba_superseded_sources.push(sid.clone());
                                report.full_fallback_sources.push(sid.clone());
                                res_st
                                    .put_source_checkpoint(ResolveSourceCheckpoint {
                                        source_id: sid.clone(),
                                        instance_id,
                                        consumed_delta_seq: latest_seq,
                                        updated_at_ms: now_store_ms(),
                                    })
                                    .map_err(map_resolve_store_error)?;
                                break;
                            }

                            if deltas.is_empty() {
                                if cp_opt.is_none() && latest_seq > 0 {
                                    res_st
                                        .put_source_checkpoint(ResolveSourceCheckpoint {
                                            source_id: sid.clone(),
                                            instance_id,
                                            consumed_delta_seq: latest_seq,
                                            updated_at_ms: now_store_ms(),
                                        })
                                        .map_err(map_resolve_store_error)?;
                                }
                                break;
                            }

                            // Per-batch propagation sets
                            let mut direct_stale_units: Vec<String> = Vec::new();
                            let mut direct_stale_resources: Vec<String> = Vec::new();
                            let mut obs_lost_units: Vec<String> = Vec::new();
                            let mut obs_lost_resources: Vec<String> = Vec::new();
                            let mut cutoff_units: Vec<String> = Vec::new();
                            let mut appeared_or_mod_resources: BTreeSet<String> = BTreeSet::new();
                            let mut delta_seqs: Vec<u64> = Vec::new();
                            let mut max_seq = after_seq.unwrap_or(0);

                            for d in &deltas {
                                report.deltas_consumed += 1;
                                delta_seqs.push(d.delta_seq);
                                max_seq = max_seq.max(d.delta_seq);
                                for tr in &d.unit_transitions {
                                    match tr.change_kind {
                                        UnitChangeKind::RelocatedOnly
                                        | UnitChangeKind::Reconfirmed => {
                                            cutoff_units.push(tr.unit_id.clone());
                                        }
                                        UnitChangeKind::ObservationLost => {
                                            obs_lost_units.push(tr.unit_id.clone());
                                            obs_lost_resources.push(tr.resource_key.clone());
                                        }
                                        UnitChangeKind::Appeared => {
                                            appeared_or_mod_resources
                                                .insert(tr.resource_key.clone());
                                        }
                                        UnitChangeKind::ContentModified => {
                                            direct_stale_units.push(tr.unit_id.clone());
                                            direct_stale_resources.push(tr.resource_key.clone());
                                            appeared_or_mod_resources
                                                .insert(tr.resource_key.clone());
                                        }
                                        UnitChangeKind::ConfirmedWithdrawn
                                        | UnitChangeKind::Expired
                                        | UnitChangeKind::Split
                                        | UnitChangeKind::Merged
                                        | UnitChangeKind::EvidenceStateChanged
                                        | UnitChangeKind::AmbiguousLineage
                                        | UnitChangeKind::SourceAnomaly => {
                                            direct_stale_units.push(tr.unit_id.clone());
                                            direct_stale_resources.push(tr.resource_key.clone());
                                            for p in &tr.parent_unit_ids {
                                                direct_stale_units.push(p.clone());
                                            }
                                        }
                                    }
                                }
                            }

                            // 1. Direct substantive unit/resource modifications + downstream conclusions
                            let mut stale_seeds = BTreeSet::new();
                            stale_seeds.extend(
                                res_st
                                    .find_by_units(&direct_stale_units)
                                    .map_err(map_resolve_store_error)?,
                            );
                            stale_seeds.extend(
                                res_st
                                    .find_by_resources(&direct_stale_resources)
                                    .map_err(map_resolve_store_error)?,
                            );
                            let stale_expanded = res_st
                                .expand_downstream_conclusions(&stale_seeds)
                                .map_err(map_resolve_store_error)?;
                            if !stale_expanded.is_empty() {
                                mark_conclusions_stale(
                                    &res_st,
                                    &stale_expanded,
                                    &format!("upstream_evidence_changed:{sid}"),
                                    ResolveEventKind::ConclusionMarkedStale,
                                    direct_stale_units.clone(),
                                    delta_seqs.clone(),
                                )?;
                                impacted_all.extend(stale_expanded.iter().cloned());
                            }

                            // 2. ObservationLost => StalePendingRevalidation (NEVER directly Refuted!)
                            let mut obs_seeds = BTreeSet::new();
                            obs_seeds.extend(
                                res_st
                                    .find_by_units(&obs_lost_units)
                                    .map_err(map_resolve_store_error)?,
                            );
                            obs_seeds.extend(
                                res_st
                                    .find_by_resources(&obs_lost_resources)
                                    .map_err(map_resolve_store_error)?,
                            );
                            let obs_expanded = res_st
                                .expand_downstream_conclusions(&obs_seeds)
                                .map_err(map_resolve_store_error)?;
                            if !obs_expanded.is_empty() {
                                mark_conclusions_stale(
                                    &res_st,
                                    &obs_expanded,
                                    "observation_lost_pending_confirmation",
                                    ResolveEventKind::ConclusionMarkedStale,
                                    obs_lost_units.clone(),
                                    delta_seqs.clone(),
                                )?;
                                obs_lost_all.extend(obs_expanded.iter().cloned());
                                impacted_all.extend(obs_expanded.iter().cloned());
                            }

                            // 3. Scope/negative-dependency propagation for newly appearing or modified units
                            // (catches new competitors / incidents breaking universal, superlative, or negative-existential claims!)
                            let mut scope_preds = BTreeSet::new();
                            for rk in &appeared_or_mod_resources {
                                let units = ctx_st.load_resource_units(sid, rk).map_err(|e| {
                                    ApiError::new(ErrorCode::Internal, e.to_string())
                                })?;
                                for u in &units {
                                    for pred in unit_scope_trigger_predicates(sid, u) {
                                        scope_preds.insert(pred);
                                    }
                                }
                            }
                            if !scope_preds.is_empty() {
                                let pred_vec: Vec<String> = scope_preds.into_iter().collect();
                                let scope_seeds = res_st
                                    .find_by_scope_predicates(&pred_vec)
                                    .map_err(map_resolve_store_error)?;
                                let scope_expanded = res_st
                                    .expand_downstream_conclusions(&scope_seeds)
                                    .map_err(map_resolve_store_error)?;
                                if !scope_expanded.is_empty() {
                                    mark_conclusions_stale(
                                        &res_st,
                                        &scope_expanded,
                                        &format!("scope_expanded_new_record:{sid}"),
                                        ResolveEventKind::ScopeExpanded,
                                        Vec::new(),
                                        delta_seqs.clone(),
                                    )?;
                                    scope_impacted_all.extend(scope_expanded.iter().cloned());
                                    impacted_all.extend(scope_expanded.iter().cloned());
                                }
                            }

                            // 4. Salsa Early Cutoff: conclusions touching ONLY RelocatedOnly / Reconfirmed units stay clean!
                            if !cutoff_units.is_empty() {
                                let cutoff_candidates = res_st
                                    .find_by_units(&cutoff_units)
                                    .map_err(map_resolve_store_error)?;
                                for cid in cutoff_candidates {
                                    if !impacted_all.contains(&cid) {
                                        early_cutoff_all.insert(cid);
                                    }
                                }
                            }

                            res_st
                                .put_source_checkpoint(ResolveSourceCheckpoint {
                                    source_id: sid.clone(),
                                    instance_id,
                                    consumed_delta_seq: max_seq,
                                    updated_at_ms: now_store_ms(),
                                })
                                .map_err(map_resolve_store_error)?;
                            after_seq = Some(max_seq);

                            if !truncated {
                                break;
                            }
                        }
                    }
                }
            }

            report.impacted_conclusion_ids = impacted_all.into_iter().collect();
            report.scope_impacted_conclusion_ids = scope_impacted_all.into_iter().collect();
            report.observation_lost_conclusion_ids = obs_lost_all.into_iter().collect();
            report.early_cutoff_conclusion_ids = early_cutoff_all.into_iter().collect();
            Ok(report)
        })
        .await
}

/// Revalidate impacted (incremental) or all (full) persisted conclusions, preserving immutable
/// version history, tracking early-cutoff unchanged conclusions vs state changes, and honoring
/// network/host/page-budget guardrails.
#[allow(clippy::too_many_arguments)]
pub async fn revalidate_conclusions(
    scraper: &Scraper,
    tenant: Option<&str>,
    conclusion_ids: Vec<String>,
    source_ids: Vec<String>,
    mode: RevalidateMode,
    verifier_mode: VerifierMode,
    allow_network_acquisition: bool,
    allow_external_search: bool,
    allowed_hosts: Vec<String>,
    max_acquisition_requests: usize,
    page_budget: Option<Arc<dyn PageBudget>>,
) -> Result<RevalidateOutcome, ApiError> {
    let t0 = Instant::now();
    let store = scraper.store_ref().cloned().ok_or_else(|| {
        ApiError::invalid("resolve revalidate requires a persistent store configured on Scraper")
    })?;

    // In Incremental mode without explicit conclusion_ids, consume pending deltas first!
    let mut warnings = Vec::new();
    if mode == RevalidateMode::Incremental && conclusion_ids.is_empty() {
        let impact = reconcile_changes(
            scraper,
            tenant,
            source_ids.clone(),
            ReconcileMode::Incremental,
        )
        .await?;
        warnings.extend(impact.warnings);
    }

    // Load target conclusion snapshots in topological order (parents before children!)
    let t_owned = tenant.map(str::to_owned);
    let cids_arg = conclusion_ids.clone();
    let sids_arg = source_ids.clone();
    let targets: Vec<ConclusionRecord> = store
        .blocking(move |st| -> Result<Vec<ConclusionRecord>, ApiError> {
            let res_st = st.resolve_for(t_owned.as_deref());
            let mut records = Vec::new();
            if !cids_arg.is_empty() {
                for cid in &cids_arg {
                    if let Some(rec) = res_st
                        .get_conclusion(cid)
                        .map_err(map_resolve_store_error)?
                    {
                        records.push(rec);
                    }
                }
            } else {
                let state_filter = match mode {
                    RevalidateMode::Incremental => Some(ResolutionState::StalePendingRevalidation),
                    RevalidateMode::Full => None,
                };
                if sids_arg.is_empty() {
                    let (_, all) = res_st
                        .list_conclusions(state_filter, None, usize::MAX)
                        .map_err(map_resolve_store_error)?;
                    records = all;
                } else {
                    let mut seen = BTreeSet::new();
                    for sid in &sids_arg {
                        let (_, batch) = res_st
                            .list_conclusions(state_filter, Some(sid), usize::MAX)
                            .map_err(map_resolve_store_error)?;
                        for rec in batch {
                            if seen.insert(rec.conclusion_id.clone()) {
                                records.push(rec);
                            }
                        }
                    }
                    // Also pick up cross-source conclusions without explicit source_ids that were marked stale
                    if mode == RevalidateMode::Incremental {
                        let (_, stale_all) = res_st
                            .list_conclusions(state_filter, None, usize::MAX)
                            .map_err(map_resolve_store_error)?;
                        for rec in stale_all {
                            if rec.scope.source_ids.is_empty()
                                && seen.insert(rec.conclusion_id.clone())
                            {
                                records.push(rec);
                            }
                        }
                    }
                }
            }
            Ok(toposort_conclusions(records))
        })
        .await?;

    let mut checked_count = 0usize;
    let mut state_changed_count = 0usize;
    let mut unchanged_early_cutoff_count = 0usize;
    let mut updated = Vec::with_capacity(targets.len());

    for prev in targets {
        let baseline_state = prev
            .corrections
            .last()
            .filter(|c| c.to_state == ResolutionState::StalePendingRevalidation)
            .map(|c| c.from_state)
            .unwrap_or(prev.state);

        let check_req = ResolveCheckRequest {
            answer: prev.answer_text.clone(),
            claims: Vec::new(),
            query: prev.query.clone(),
            domain: prev.domain.clone(),
            conclusion_id: Some(prev.conclusion_id.clone()),
            source_ids: prev.scope.source_ids.clone(),
            subject_key: prev.scope.subject_keys.first().cloned(),
            parent_conclusion_ids: prev.parent_conclusion_ids.clone(),
            assumptions: prev.assumptions.clone(),
            domain_kind: prev.scope.domain_kind,
            proof_standard: prev.proof_standard,
            verifier_mode,
            min_independent_sources: match prev.proof_standard {
                pagesift_graph::resolve::ProofStandard::ClearAndConvincing
                | pagesift_graph::resolve::ProofStandard::StrictUniversal => 2,
                _ => 1,
            },
            as_of_ms: prev.scope.as_of_ms,
            effective_at_ms: prev.scope.effective_at_ms,
            include_unverified: false,
            enable_counter_search: prev.counter_audit.as_ref().is_some_and(|a| a.enabled),
            enable_gap_resolution: allow_network_acquisition,
            allow_network_acquisition,
            allow_external_search,
            strict_constrain_to_urls: false,
            candidate_urls: Vec::new(),
            allowed_hosts: allowed_hosts.clone(),
            max_acquisition_requests,
            persist: true,
            learn_enabled: true,
        };

        let out =
            execute_check_with_budget(scraper, tenant, check_req, Vec::new(), page_budget.clone())
                .await?;
        checked_count += 1;
        warnings.extend(out.warnings);

        let next = out.conclusion;
        if next.state == baseline_state && next.content_digest == prev.content_digest {
            unchanged_early_cutoff_count += 1;
        } else if next.state != baseline_state {
            state_changed_count += 1;
        }
        updated.push(next);
    }

    Ok(RevalidateOutcome {
        mode,
        checked_count,
        state_changed_count,
        unchanged_early_cutoff_count,
        conclusions: updated,
        warnings,
        elapsed_micros: t0.elapsed().as_micros() as u64,
    })
}

fn expand_source_conclusions(
    res_st: &pagesift_store::ResolveStore<'_>,
    source_id: &str,
) -> Result<BTreeSet<String>, ApiError> {
    let direct = res_st
        .find_by_source(source_id)
        .map_err(map_resolve_store_error)?;
    let scope_direct = res_st
        .find_by_scope_predicates(&[
            format!("source:{source_id}"),
            format!("scope_source:{source_id}"),
            "scope_source:*".to_owned(),
        ])
        .map_err(map_resolve_store_error)?;
    let mut seeds = direct;
    seeds.extend(scope_direct);
    res_st
        .expand_downstream_conclusions(&seeds)
        .map_err(map_resolve_store_error)
}

fn mark_conclusions_stale(
    res_st: &pagesift_store::ResolveStore<'_>,
    cids: &BTreeSet<String>,
    reason: &str,
    event_kind: ResolveEventKind,
    triggering_unit_ids: Vec<String>,
    triggering_delta_seqs: Vec<u64>,
) -> Result<(), ApiError> {
    for cid in cids {
        let _ = res_st
            .mark_stale(
                cid,
                reason,
                event_kind,
                triggering_unit_ids.clone(),
                triggering_delta_seqs.clone(),
            )
            .map_err(map_resolve_store_error)?;
    }
    Ok(())
}

/// Order conclusions so upstream parent conclusions revalidate before their downstream children.
fn toposort_conclusions(records: Vec<ConclusionRecord>) -> Vec<ConclusionRecord> {
    let by_id: BTreeMap<String, ConclusionRecord> = records
        .into_iter()
        .map(|r| (r.conclusion_id.clone(), r))
        .collect();
    let mut visited = BTreeSet::new();
    let mut order = Vec::with_capacity(by_id.len());

    fn dfs(
        cid: &str,
        by_id: &BTreeMap<String, ConclusionRecord>,
        visited: &mut BTreeSet<String>,
        order: &mut Vec<String>,
    ) {
        if !visited.insert(cid.to_owned()) {
            return;
        }
        if let Some(rec) = by_id.get(cid) {
            for pid in &rec.parent_conclusion_ids {
                if by_id.contains_key(pid) {
                    dfs(pid, by_id, visited, order);
                }
            }
            order.push(cid.to_owned());
        }
    }

    for cid in by_id.keys() {
        dfs(cid, &by_id, &mut visited, &mut order);
    }

    let mut map = by_id;
    order
        .into_iter()
        .filter_map(|cid| map.remove(&cid))
        .collect()
}
