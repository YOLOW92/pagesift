//! Goal-oriented 1.6 Context `Action`: reuses existing Context first, diagnoses gated coverage
//! (Good–Turing / Chapman), and decides bounded Loop/Scrape acquisition via Howard VOI + 4D ShadowPrices.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use pagesift_api::types::{Format, ScrapeOptions, SearchRequest};
use pagesift_api::{ApiError, Engine, ErrorCode, PageBudget};
use pagesift_extract::context::{ContextLearnState, compile_context_pack};
use pagesift_graph::context::{
    AcquisitionStepRecord, ActionMode, ContextActionOutcome, ContextActionRequest,
    ContextSearchReport, CoverageDiagnosis, InformationGap, StoppingReason, UnitMatchMode,
};
use pagesift_graph::{GraphOptions, normalize_resource_key};
use pagesift_stat::Delta;
use pagesift_stat::beta::Posterior;
use pagesift_stat::coverage::{CoverageEstimate, chapman_capture_recapture, good_turing_unseen};
use pagesift_stat::decide::{Cost, Resource, value_of_information};
use pagesift_store::{DataSourceSpec, FreshnessPolicy, SyncBudget, now_store_ms};

use crate::context::action_support::{empty_coverage, execute_changes_mode, url_permitted};
pub use crate::context::action_support::{
    load_learn_state, load_learn_state_with_warning, save_learn_state, try_load_learn_state,
};
use crate::context::ingest::{
    ContextDocumentInput, LoadedContextCorpus, affinity_key, ingest_documents, load_corpus,
    sync_source_context,
};
use crate::engine::{ACTION_ALLOWED_HOSTS, Scraper, cpu_task_keyed};
use crate::sync::SyncRunOptions;

// [策略] 先验期望一次有效网络补采动作的成功发现率 alpha=3.0, beta=2.0 (均值 0.60)
const ACQ_PRIOR_ALPHA: f64 = 3.0;
const ACQ_PRIOR_BETA: f64 = 2.0;
// [策略] 一次轻量 HTTP/Sync 补采动作的基准归一化服务成本（0.05 CPU 秒）
const BASE_HTTP_CPU_COST: f64 = 0.05;
// [策略] 一次轻量 HTTP/Sync 补采动作的基准礼貌配额单位成本 0.15
const BASE_POLITENESS_COST: f64 = 0.15;
// [推导] 无排队拥塞时的最小动作机会成本底线 0.04（避免空闲时对零缺口任务无限发探针）
const MIN_ACTION_CHARGE_FLOOR: f64 = 0.04;

/// Execute a goal-oriented [`ContextActionRequest`] with strict permission and budget guardrails.
pub async fn execute_action(
    scraper: &Scraper,
    tenant: Option<&str>,
    req: ContextActionRequest,
    inline_docs: Vec<ContextDocumentInput>,
    match_mode: UnitMatchMode,
) -> Result<ContextActionOutcome, ApiError> {
    execute_action_with_budget(scraper, tenant, req, inline_docs, match_mode, None).await
}

/// Execute a goal-oriented [`ContextActionRequest`] while also enforcing a caller-level
/// daily [`PageBudget`] across all triggered Loop sync and direct scrape acquisitions.
pub async fn execute_action_with_budget(
    scraper: &Scraper,
    tenant: Option<&str>,
    mut req: ContextActionRequest,
    inline_docs: Vec<ContextDocumentInput>,
    match_mode: UnitMatchMode,
    page_budget: Option<Arc<dyn PageBudget>>,
) -> Result<ContextActionOutcome, ApiError> {
    // Mode `Watch`: ensure a persistent Loop source exists for `candidate_urls` without clobbering existing scope rules
    if req.mode == ActionMode::Watch && !req.candidate_urls.is_empty() {
        let store = scraper.store_ref().cloned().ok_or_else(|| {
            ApiError::invalid("watch mode requires persistent store configured on Scraper")
        })?;
        for u in &req.candidate_urls {
            if !url_permitted(u, &req.allowed_hosts) {
                return Ok(ContextActionOutcome {
                    mode: req.mode,
                    stopping_reason: StoppingReason::PermissionRestricted,
                    reused_existing_context: false,
                    coverage: empty_coverage(req.min_independent_sources, "permission_restricted"),
                    gaps: vec![InformationGap {
                        kind: "host_not_allowed".to_owned(),
                        target: u.clone(),
                        detail: "candidate URL scheme invalid or host outside allowedHosts"
                            .to_owned(),
                    }],
                    acquisition_steps: Vec::new(),
                    pack: None,
                    deltas: Vec::new(),
                });
            }
        }
        let watch_id = req
            .pack
            .search
            .source_ids
            .first()
            .cloned()
            .unwrap_or_else(|| {
                format!(
                    "watch-{}",
                    &pagesift_graph::sha256_hex(req.candidate_urls[0].as_bytes())[..12]
                )
            }); // [协议] 取 12 位 hex 生成确定性 watch 源 ID
        if !req.pack.search.source_ids.contains(&watch_id) {
            req.pack.search.source_ids.push(watch_id.clone());
        }
        let seeds = req.candidate_urls.clone();
        let req_fields = req.pack.search.required_fields.clone();
        let strict_urls = req.strict_constrain_to_urls;
        let max_reqs_tick = req.max_acquisition_requests.max(1);
        let t_owned = tenant.map(str::to_owned);
        let wid = watch_id.clone();
        store
            .blocking(move |st| {
                let now = now_store_ms();
                let sync_st = st.sync_for(t_owned.as_deref());
                // Preserve existing includePaths/excludePaths/freshness/browser scope if source already exists!
                let spec_to_save = match sync_st.get_source(&wid)? {
                    Some(mut existing) => {
                        for s in seeds {
                            if !existing.seeds.contains(&s) {
                                existing.seeds.push(s);
                            }
                        }
                        for rf in req_fields {
                            if !existing.required_fields.contains(&rf) {
                                existing.required_fields.push(rf);
                            }
                        }
                        existing.updated_at_ms = now;
                        existing
                    }
                    None => DataSourceSpec {
                        id: wid,
                        name: Some("Context Watch Source".to_owned()),
                        seeds,
                        include_paths: Vec::new(),
                        exclude_paths: Vec::new(),
                        max_depth: usize::from(!strict_urls),
                        allow_subdomains: false,
                        freshness: FreshnessPolicy::default(),
                        budget: SyncBudget {
                            max_requests_per_tick: max_reqs_tick,
                            ..SyncBudget::default()
                        },
                        extract_schema: None,
                        extract_prompt: None,
                        required_fields: req_fields,
                        allow_browser: false,
                        created_at_ms: now,
                        updated_at_ms: now,
                        revision: 1,
                        instance_id: String::new(),
                    },
                };
                sync_st.upsert_source(spec_to_save)
            })
            .await
            .map_err(|e| ApiError::new(ErrorCode::Internal, format!("upsert watch source: {e}")))?;
    }

    // Mode `Changes`: return unit-level deltas since `since_delta_seq` with retention-gap & ABA detection
    if req.mode == ActionMode::Changes {
        return execute_changes_mode(scraper, tenant, &req).await;
    }

    // 1. Load existing Context (or ephemeral inline docs) and evaluate initial coverage BEFORE touching network!
    let (mut learn, learn_warn) = load_learn_state_with_warning(scraper, tenant).await;
    let mut corpus = if !inline_docs.is_empty() {
        let persist = scraper.store_ref().is_some() && !req.pack.search.source_ids.is_empty();
        let sid = req
            .pack
            .search
            .source_ids
            .first()
            .map(String::as_str)
            .unwrap_or("inline");
        ingest_documents(scraper, tenant, sid, inline_docs, match_mode, None, persist)
            .await?
            .0
    } else {
        load_corpus(
            scraper,
            tenant,
            &req.pack.search.source_ids,
            req.pack.search.as_of_ms,
        )
        .await?
    };

    let had_existing_units = !corpus.index.units.is_empty();
    let closed_scope = !req.allow_external_search
        && (!req.pack.search.source_ids.is_empty() || had_existing_units);

    let (mut report, mut search_us, mut cov, mut gaps) =
        evaluate_corpus_and_gaps(&corpus, &req, closed_scope, &mut learn).await?;
    let reused_existing_context = !report.hits.is_empty();

    let mut steps: Vec<AcquisitionStepRecord> = Vec::new();
    let mut deltas = Vec::new();
    let mut extra_warnings: Vec<String> = Vec::new();
    if let Some(w) = learn_warn {
        extra_warnings.push(w);
    }
    extra_warnings.extend(corpus.warnings.iter().cloned());
    let mut acquired_any = false;
    let mut quota_exhausted = false;
    let mut stopping_reason = if gaps.is_empty() {
        StoppingReason::Sufficient
    } else if !req.allow_network_acquisition {
        StoppingReason::PermissionRestricted
    } else {
        StoppingReason::NoNewEvidence
    };

    // 2. Controlled sequential acquisition when gaps exist and caller explicitly permitted network access
    if !gaps.is_empty() && req.allow_network_acquisition {
        let mut req_budget_left = req.max_acquisition_requests;
        if req_budget_left == 0 {
            stopping_reason = StoppingReason::BudgetExhausted;
        }
        let prices = scraper.shadow_prices();
        let base_charge = prices
            .charge(
                &Cost::default()
                    .with(Resource::Cpu, BASE_HTTP_CPU_COST)
                    .with(Resource::Politeness, BASE_POLITENESS_COST),
            )
            .max(MIN_ACTION_CHARGE_FLOOR);

        // Track URLs already freshly synced in 2A so 2B/2C never double-scrapes them in the same Action call!
        let mut seen_url_keys: BTreeSet<String> = BTreeSet::new();
        let allowed_hosts_arc =
            (!req.allowed_hosts.is_empty()).then(|| Arc::new(req.allowed_hosts.clone()));

        // 2A: Refresh managed Loop sources first when stale or empty (skip pure manual-ingest Context sources)
        for sid in req.pack.search.source_ids.clone() {
            let is_loop_source = scraper.store_ref().is_some_and(|st| {
                st.context_for(tenant)
                    .has_sync_source(&sid)
                    .unwrap_or(false)
            });
            if !is_loop_source {
                continue;
            }
            if gaps.is_empty() {
                stopping_reason = StoppingReason::Sufficient;
                break;
            }
            if req_budget_left == 0 || quota_exhausted {
                stopping_reason = StoppingReason::BudgetExhausted;
                break;
            }
            let gap_utility = compute_gap_utility(&gaps, &req);
            let voi = value_of_information(
                &Posterior {
                    alpha: ACQ_PRIOR_ALPHA,
                    beta: ACQ_PRIOR_BETA,
                },
                gap_utility,
                base_charge,
            );
            // High-risk Verify / missing required independent sources overrides premature low-VOI stop!
            let high_risk_must_continue = req.mode == ActionMode::Verify
                || cov.independent_sources_observed < req.min_independent_sources
                || cov.required_field_coverage < 1.0;

            if voi <= 0.0 && !high_risk_must_continue {
                steps.push(AcquisitionStepRecord {
                    kind: "sync_source".to_owned(),
                    target: sid,
                    expected_voi: voi,
                    shadow_cost: base_charge,
                    executed: false,
                    units_added_or_updated: 0,
                    note: Some("skipped_voi_below_shadow_cost".to_owned()),
                });
                stopping_reason = StoppingReason::VoiBelowCost;
                break;
            }

            let step_reqs = req_budget_left.min(8); // [策略] 单源单步最多分配 8 个请求预算防单源耗尽全局预算
            match scraper
                .run_sync_tick(
                    &sid,
                    SyncRunOptions {
                        tenant: tenant.map(str::to_owned),
                        force_all: req.max_staleness_ms.is_some(),
                        max_requests_override: Some(step_reqs),
                        page_budget: page_budget.clone(),
                        allowed_hosts: req.allowed_hosts.clone(),
                    },
                )
                .await
            {
                Ok(sync_d) => {
                    let filtered_by_hosts = sync_d
                        .coverage_reasons
                        .iter()
                        .any(|r| r.starts_with("allowed_hosts_filtered:"));
                    let hit_daily_quota = sync_d
                        .coverage_reasons
                        .iter()
                        .any(|r| r == "daily_page_quota_exceeded");
                    if sync_d.stats.requests_executed > 0 {
                        acquired_any = true;
                        req_budget_left =
                            req_budget_left.saturating_sub(sync_d.stats.requests_executed);
                    }
                    let (ctx_d, _) =
                        sync_source_context(scraper, tenant, &sid, match_mode, None).await?;
                    if let Some(store) = scraper.store_ref().cloned() {
                        let t_seen = tenant.map(str::to_owned);
                        let sid_seen = sid.clone();
                        let synced_keys = store
                            .blocking(move |st| -> BTreeSet<String> {
                                let mut ks = BTreeSet::new();
                                if let Ok(Some(m)) =
                                    st.context_for(t_seen.as_deref()).get_meta(&sid_seen)
                                {
                                    ks.extend(m.resource_keys);
                                }
                                if let Ok(Some(spec)) =
                                    st.sync_for(t_seen.as_deref()).get_source(&sid_seen)
                                {
                                    for seed in &spec.seeds {
                                        ks.insert(normalize_resource_key(seed));
                                    }
                                }
                                ks
                            })
                            .await;
                        seen_url_keys.extend(synced_keys);
                    }
                    let changed_n = ctx_d.unit_transitions.len();
                    deltas.push(ctx_d);
                    let step_note = if hit_daily_quota {
                        quota_exhausted = true;
                        stopping_reason = StoppingReason::BudgetExhausted;
                        Some("QUOTA_EXCEEDED:daily_page_quota_exceeded".to_owned())
                    } else if filtered_by_hosts && sync_d.stats.requests_executed == 0 {
                        gaps.push(InformationGap {
                            kind: "permission_blocked_host".to_owned(),
                            target: sid.clone(),
                            detail:
                                "managed Loop source URLs excluded by Action allowedHosts policy"
                                    .to_owned(),
                        });
                        stopping_reason = StoppingReason::PermissionRestricted;
                        Some("blocked_by_allowed_hosts".to_owned())
                    } else if filtered_by_hosts {
                        Some("partial_filtered_by_allowed_hosts".to_owned())
                    } else {
                        None
                    };
                    steps.push(AcquisitionStepRecord {
                        kind: "sync_source".to_owned(),
                        target: sid,
                        expected_voi: voi,
                        shadow_cost: base_charge,
                        executed: sync_d.stats.requests_executed > 0,
                        units_added_or_updated: changed_n,
                        note: step_note,
                    });
                }
                Err(err) if err.code == ErrorCode::QuotaExceeded => {
                    if !acquired_any && !reused_existing_context {
                        return Err(err);
                    }
                    quota_exhausted = true;
                    steps.push(AcquisitionStepRecord {
                        kind: "sync_source".to_owned(),
                        target: sid,
                        expected_voi: voi,
                        shadow_cost: base_charge,
                        executed: false,
                        units_added_or_updated: 0,
                        note: Some(format!("{}:{}", err.code.as_str(), err.message)),
                    });
                    stopping_reason = StoppingReason::BudgetExhausted;
                    break;
                }
                Err(err) => {
                    let perm = matches!(
                        err.code,
                        ErrorCode::SsrfBlocked | ErrorCode::RobotsDisallowed
                    );
                    steps.push(AcquisitionStepRecord {
                        kind: "sync_source".to_owned(),
                        target: sid,
                        expected_voi: voi,
                        shadow_cost: base_charge,
                        executed: false,
                        units_added_or_updated: 0,
                        note: Some(format!("{}:{}", err.code.as_str(), err.message)),
                    });
                    stopping_reason = if perm {
                        StoppingReason::PermissionRestricted
                    } else {
                        StoppingReason::SourceUnavailable
                    };
                }
            }

            let prev_perm_gaps: Vec<InformationGap> = gaps
                .iter()
                .filter(|g| g.kind == "permission_blocked_host")
                .cloned()
                .collect();
            corpus = load_corpus(
                scraper,
                tenant,
                &req.pack.search.source_ids,
                req.pack.search.as_of_ms,
            )
            .await?;
            for w in &corpus.warnings {
                if !extra_warnings.contains(w) {
                    extra_warnings.push(w.clone());
                }
            }
            let eval = evaluate_corpus_and_gaps(&corpus, &req, closed_scope, &mut learn).await?;
            report = eval.0;
            search_us = eval.1;
            cov = eval.2;
            gaps = eval.3;
            if !gaps.is_empty() {
                gaps.extend(prev_perm_gaps);
            }
        }

        // 2B: Deduplicate candidate URLs (excluding those just synced in 2A) and optionally run budgeted external SearchBackend
        let mut extra_urls = Vec::new();
        for u in &req.candidate_urls {
            let rk = normalize_resource_key(u);
            if seen_url_keys.insert(rk) {
                extra_urls.push(u.clone());
            }
        }
        if !gaps.is_empty()
            && !quota_exhausted
            && req_budget_left > 0
            && req.allow_external_search
            && !req.strict_constrain_to_urls
            && !req.pack.search.query.trim().is_empty()
            && page_budget.as_ref().and_then(|b| b.remaining()) == Some(0)
        {
            if !acquired_any && !reused_existing_context {
                return Err(ApiError::new(
                    ErrorCode::QuotaExceeded,
                    "daily page quota exceeded before external search acquisition",
                ));
            }
            quota_exhausted = true;
            stopping_reason = StoppingReason::BudgetExhausted;
        }
        if !gaps.is_empty()
            && !quota_exhausted
            && req_budget_left > 0
            && req.allow_external_search
            && !req.strict_constrain_to_urls
            && !req.pack.search.query.trim().is_empty()
        {
            let gap_utility = compute_gap_utility(&gaps, &req);
            let voi = value_of_information(
                &Posterior {
                    alpha: ACQ_PRIOR_ALPHA,
                    beta: ACQ_PRIOR_BETA,
                },
                gap_utility,
                base_charge,
            );
            let must_continue = req.mode == ActionMode::Verify
                || cov.independent_sources_observed < req.min_independent_sources
                || cov.required_field_coverage < 1.0;
            if voi > 0.0 || must_continue {
                req_budget_left -= 1;
                let sreq = SearchRequest {
                    query: req.pack.search.query.clone(),
                    limit: req_budget_left.clamp(1, 4), // [策略] 单轮外部搜索最多拉取前 4 条高相关候选
                    scrape_options: None,
                    lang: None,
                    country: None,
                    tbs: None,
                    timeout: None,
                };
                match <Scraper as Engine>::search(scraper, &sreq).await {
                    Ok(val) => {
                        let mut discovered = 0usize;
                        if let Some(arr) = val.get("web").and_then(|w| w.as_array()) {
                            for item in arr {
                                if let Some(u) = item.get("url").and_then(|x| x.as_str()) {
                                    let rk = normalize_resource_key(u);
                                    if seen_url_keys.insert(rk) {
                                        extra_urls.push(u.to_owned());
                                        discovered += 1;
                                    }
                                }
                            }
                        }
                        steps.push(AcquisitionStepRecord {
                            kind: "external_search".to_owned(),
                            target: req.pack.search.query.clone(),
                            expected_voi: voi,
                            shadow_cost: base_charge,
                            executed: true,
                            units_added_or_updated: 0,
                            note: Some(format!("discovered_candidates={discovered}")),
                        });
                    }
                    Err(err) => {
                        steps.push(AcquisitionStepRecord {
                            kind: "external_search".to_owned(),
                            target: req.pack.search.query.clone(),
                            expected_voi: voi,
                            shadow_cost: base_charge,
                            executed: false,
                            units_added_or_updated: 0,
                            note: Some(format!("search_unavailable:{}", err.message)),
                        });
                    }
                }
            } else {
                stopping_reason = StoppingReason::VoiBelowCost;
            }
        }

        // Never write ad-hoc scraped URLs directly into a managed Loop source's inventory namespace
        // (doing so would get pruned on next sync tick or corrupt Loop source invariants); use a non-Loop
        // Context source if one was requested, otherwise isolate into `"context-acquired"`.
        let target_sid = req
            .pack
            .search
            .source_ids
            .iter()
            .find(|sid| {
                !scraper
                    .store_ref()
                    .is_some_and(|st| st.context_for(tenant).has_sync_source(sid).unwrap_or(false))
            })
            .cloned()
            .unwrap_or_else(|| "context-acquired".to_owned());

        let mut action_gaps: Vec<InformationGap> = Vec::new();
        for url in extra_urls {
            if gaps.is_empty() {
                stopping_reason = StoppingReason::Sufficient;
                break;
            }
            if !url_permitted(&url, &req.allowed_hosts) {
                action_gaps.push(InformationGap {
                    kind: "permission_blocked_host".to_owned(),
                    target: url.clone(),
                    detail: "target URL scheme invalid or host excluded by allowedHosts policy"
                        .to_owned(),
                });
                steps.push(AcquisitionStepRecord {
                    kind: "scrape_url".to_owned(),
                    target: url,
                    expected_voi: 0.0,
                    shadow_cost: base_charge,
                    executed: false,
                    units_added_or_updated: 0,
                    note: Some("blocked_by_allowed_hosts".to_owned()),
                });
                stopping_reason = StoppingReason::PermissionRestricted;
                continue;
            }
            if req_budget_left == 0 || quota_exhausted {
                stopping_reason = StoppingReason::BudgetExhausted;
                break;
            }

            let gap_utility = compute_gap_utility(&gaps, &req);
            let voi = value_of_information(
                &Posterior {
                    alpha: ACQ_PRIOR_ALPHA,
                    beta: ACQ_PRIOR_BETA,
                },
                gap_utility,
                base_charge,
            );
            let must_continue = req.mode == ActionMode::Verify
                || cov.independent_sources_observed < req.min_independent_sources
                || cov.required_field_coverage < 1.0;
            if voi <= 0.0 && !must_continue {
                steps.push(AcquisitionStepRecord {
                    kind: "scrape_url".to_owned(),
                    target: url,
                    expected_voi: voi,
                    shadow_cost: base_charge,
                    executed: false,
                    units_added_or_updated: 0,
                    note: Some("skipped_voi_below_shadow_cost".to_owned()),
                });
                stopping_reason = StoppingReason::VoiBelowCost;
                break;
            }

            let page_lease = if let Some(ref budget) = page_budget {
                match budget.reserve().await {
                    Ok(lease) => Some(lease),
                    Err(err) => {
                        if !acquired_any && !reused_existing_context {
                            return Err(err);
                        }
                        quota_exhausted = true;
                        action_gaps.push(InformationGap {
                            kind: "daily_page_quota_exceeded".to_owned(),
                            target: url.clone(),
                            detail: err.message.clone(),
                        });
                        steps.push(AcquisitionStepRecord {
                            kind: "scrape_url".to_owned(),
                            target: url,
                            expected_voi: voi,
                            shadow_cost: base_charge,
                            executed: false,
                            units_added_or_updated: 0,
                            note: Some(format!("{}:{}", err.code.as_str(), err.message)),
                        });
                        stopping_reason = StoppingReason::BudgetExhausted;
                        break;
                    }
                }
            } else {
                None
            };

            req_budget_left -= 1;
            let sopts = ScrapeOptions {
                formats: vec![Format::new("markdown"), Format::new("graph")],
                graph: Some(GraphOptions::enabled()),
                max_age: Some(req.max_staleness_ms.unwrap_or(0)),
                owner: tenant.map(str::to_owned),
                ..ScrapeOptions::default()
            };
            let scrape_res = if let Some(ref hosts) = allowed_hosts_arc {
                ACTION_ALLOWED_HOSTS
                    .scope(
                        hosts.clone(),
                        <Scraper as Engine>::scrape(scraper, &url, &sopts),
                    )
                    .await
            } else {
                <Scraper as Engine>::scrape(scraper, &url, &sopts).await
            };
            match scrape_res {
                Ok(doc) => {
                    if let Some(lease) = page_lease {
                        lease.commit().await;
                    }
                    acquired_any = true;
                    let doc_in = ContextDocumentInput::from_api_document(&doc, &url);
                    let persist = scraper.store_ref().is_some();
                    let (new_corp, d_opt) = ingest_documents(
                        scraper,
                        tenant,
                        &target_sid,
                        vec![doc_in],
                        match_mode,
                        None,
                        persist,
                    )
                    .await?;
                    let changed_n = d_opt.as_ref().map_or(0, |d| d.unit_transitions.len());
                    if let Some(d) = d_opt {
                        deltas.push(d);
                    }
                    if persist {
                        if !req.pack.search.source_ids.contains(&target_sid) {
                            req.pack.search.source_ids.push(target_sid.clone());
                        }
                        corpus = load_corpus(
                            scraper,
                            tenant,
                            &req.pack.search.source_ids,
                            req.pack.search.as_of_ms,
                        )
                        .await?;
                        for w in &corpus.warnings {
                            if !extra_warnings.contains(w) {
                                extra_warnings.push(w.clone());
                            }
                        }
                    } else {
                        Arc::make_mut(&mut corpus.index).apply_updates(
                            &new_corp.index.units.values().cloned().collect::<Vec<_>>(),
                            &[],
                            &Default::default(),
                        );
                    }
                    steps.push(AcquisitionStepRecord {
                        kind: "scrape_url".to_owned(),
                        target: url,
                        expected_voi: voi,
                        shadow_cost: base_charge,
                        executed: true,
                        units_added_or_updated: changed_n,
                        note: None,
                    });
                }
                Err(err) => {
                    drop(page_lease);
                    let policy_blocked = matches!(
                        err.code,
                        ErrorCode::SsrfBlocked | ErrorCode::RobotsDisallowed
                    );
                    if policy_blocked {
                        action_gaps.push(InformationGap {
                            kind: "policy_blocked_url".to_owned(),
                            target: url.clone(),
                            detail: format!("{}: {}", err.code.as_str(), err.message),
                        });
                    }
                    steps.push(AcquisitionStepRecord {
                        kind: "scrape_url".to_owned(),
                        target: url,
                        expected_voi: voi,
                        shadow_cost: base_charge,
                        executed: false,
                        units_added_or_updated: 0,
                        note: Some(format!("{}:{}", err.code.as_str(), err.message)),
                    });
                    stopping_reason = if policy_blocked {
                        StoppingReason::PermissionRestricted
                    } else {
                        StoppingReason::SourceUnavailable
                    };
                }
            }

            let eval = evaluate_corpus_and_gaps(&corpus, &req, closed_scope, &mut learn).await?;
            report = eval.0;
            search_us = eval.1;
            cov = eval.2;
            gaps = eval.3;
        }

        if gaps.is_empty() {
            stopping_reason = StoppingReason::Sufficient;
        } else if (req_budget_left == 0 || quota_exhausted)
            && stopping_reason == StoppingReason::NoNewEvidence
        {
            stopping_reason = StoppingReason::BudgetExhausted;
        }
        gaps.extend(action_gaps);
    }

    // 3. Compile final constrained ContextPack on the resident CPU pool
    for w in extra_warnings {
        if !report.warnings.contains(&w) {
            report.warnings.push(w);
        }
    }
    let pack_req = req.pack.clone();
    let key = affinity_key(&pack_req.search.query);
    let pack = cpu_task_keyed(key, move || {
        compile_context_pack(&pack_req, &report, search_us)
    })
    .await?;

    Ok(ContextActionOutcome {
        mode: req.mode,
        stopping_reason,
        reused_existing_context,
        coverage: cov,
        gaps,
        acquisition_steps: steps,
        pack: Some(pack),
        deltas,
    })
}

async fn evaluate_corpus_and_gaps(
    corpus: &LoadedContextCorpus,
    req: &ContextActionRequest,
    closed_scope: bool,
    learn: &mut ContextLearnState,
) -> Result<
    (
        ContextSearchReport,
        u64,
        CoverageDiagnosis,
        Vec<InformationGap>,
    ),
    ApiError,
> {
    // O(1) `Arc::clone` — zero deep copy of `ContextIndex` across worker threads!
    let idx = corpus.index.clone();
    let conflicts = corpus.conflicts.clone();
    let clusters = corpus.fact_clusters.clone();
    let vis = corpus.visibilities.clone();
    let sreq = req.pack.search.clone();
    let mut learn_clone = learn.clone();
    let min_indep = req.min_independent_sources;
    let max_stale = req.max_staleness_ms;
    let now = now_store_ms();

    let key = affinity_key(&sreq.query);
    let (report, elapsed_us, cov, gaps, learn_after) = cpu_task_keyed(key, move || {
        let t0 = Instant::now();
        let rep = idx.search(&sreq, &conflicts, &clusters, vis, &mut learn_clone);
        let elapsed_us = t0.elapsed().as_micros() as u64;

        // Evaluate required field coverage
        let req_fields = &sreq.required_fields;
        let mut missing_fields = Vec::new();
        for f in req_fields {
            let low = f.to_ascii_lowercase();
            let covered = rep.hits.iter().any(|h| {
                h.unit.status.is_verified_active()
                    && (h
                        .unit
                        .evidence_spans
                        .iter()
                        .any(|ev| ev.field_path.to_ascii_lowercase().contains(&low))
                        || h.unit.text.to_ascii_lowercase().contains(&low))
            });
            if !covered {
                missing_fields.push(f.clone());
            }
        }
        let field_cov = if req_fields.is_empty() {
            if rep.hits.is_empty() { 0.0 } else { 1.0 }
        } else {
            ((req_fields.len() - missing_fields.len()) as f32 / req_fields.len() as f32)
                .clamp(0.0, 1.0)
        };

        // Gated Good–Turing over independent origin roots & Chapman dual-strategy overlap
        let mut root_counts: BTreeMap<&str, u64> = BTreeMap::new();
        let mut n_lex = 0u64;
        let mut n_sketch = 0u64;
        let mut n_both = 0u64;
        for h in &rep.hits {
            if h.unit.origin_independent {
                *root_counts.entry(h.unit.origin_root.as_str()).or_insert(0) += 1;
            }
            let has_lex = h
                .matched_strategies
                .iter()
                .any(|s| s == "lexical_bm25" || s == "exact_symbol");
            let has_sk = h.matched_strategies.iter().any(|s| s == "semantic_sketch");
            if has_lex {
                n_lex += 1;
            }
            if has_sk {
                n_sketch += 1;
            }
            if has_lex && has_sk {
                n_both += 1;
            }
        }
        let counts_vec: Vec<u64> = root_counts.values().copied().collect();
        let gt = good_turing_unseen(closed_scope, &counts_vec, None, Delta::DEFAULT);
        // Retriever correlation diagnostic: Jaccard overlap ratio between lexical & sketch arms
        let arm_union = (n_lex + n_sketch).saturating_sub(n_both);
        let corr = if arm_union == 0 {
            1.0
        } else {
            n_both as f64 / arm_union as f64
        };
        let cr =
            chapman_capture_recapture(closed_scope, n_lex, n_sketch, n_both, corr, Delta::DEFAULT);

        let (estimable, gt_pt, gt_hi, not_est_reason) = match gt {
            CoverageEstimate::Estimable { point, ci_high, .. } => {
                (true, Some(point), Some(ci_high), None)
            }
            CoverageEstimate::NotEstimable { reason } => {
                (false, None, None, Some(reason.to_owned()))
            }
        };

        let cov = CoverageDiagnosis {
            term_coverage: rep.dimensions.coverage,
            required_field_coverage: field_cov,
            independent_sources_observed: rep.independent_source_count,
            min_independent_sources_required: min_indep,
            estimable,
            good_turing_unseen: gt_pt,
            good_turing_ci_high: gt_hi,
            capture_recapture_total: cr.point(),
            not_estimable_reason: not_est_reason,
        };

        let mut gaps = Vec::new();
        if rep.hits.is_empty() {
            gaps.push(InformationGap {
                kind: "no_matching_units".to_owned(),
                target: sreq.query.clone(),
                detail: "no verified knowledge units matched query".to_owned(),
            });
        // [策略] 查询核心词覆盖率低于 75% 时报告词面覆盖缺口
        } else if rep.dimensions.coverage < 0.75 {
            gaps.push(InformationGap {
                kind: "partial_query_coverage".to_owned(),
                target: sreq.query.clone(),
                detail: format!("query term coverage {:.2} < 0.75", rep.dimensions.coverage),
            });
        }
        for mf in missing_fields {
            gaps.push(InformationGap {
                kind: "missing_required_field".to_owned(),
                target: mf,
                detail: "required field lacks verified unit evidence".to_owned(),
            });
        }
        if rep.independent_source_count < min_indep {
            gaps.push(InformationGap {
                kind: "insufficient_independent_sources".to_owned(),
                target: format!(
                    "observed={},required={min_indep}",
                    rep.independent_source_count
                ),
                detail: "independent origin roots below threshold".to_owned(),
            });
        }
        if let Some(stale_limit) = max_stale {
            let any_stale = rep.hits.is_empty()
                || rep
                    .hits
                    .iter()
                    .any(|h| now.saturating_sub(h.unit.observed_at_ms) > stale_limit);
            if any_stale {
                gaps.push(InformationGap {
                    kind: "stale_evidence".to_owned(),
                    target: format!("maxStalenessMs={stale_limit}"),
                    detail: "existing units exceed requested freshness window".to_owned(),
                });
            }
        }

        (rep, elapsed_us, cov, gaps, learn_clone)
    })
    .await?;

    *learn = learn_after;
    Ok((report, elapsed_us, cov, gaps))
}

fn compute_gap_utility(gaps: &[InformationGap], req: &ContextActionRequest) -> f64 {
    if gaps.is_empty() {
        return 0.0;
    }
    let mut u = 0.0f64;
    for g in gaps {
        u += match g.kind.as_str() {
            "no_matching_units" | "missing_required_field" => 1.0,
            "insufficient_independent_sources" | "stale_evidence" => 0.85, // [策略] 独立信源不足或陈旧证据效用权重 0.85
            _ => 0.45, // [策略] 一般词面覆盖不足效用权重 0.45
        };
    }
    if req.mode == ActionMode::Verify {
        u *= 1.5; // [策略] Verify 核验模式提升信息价值权重 1.5 倍防止过早停止
    }
    u
}
