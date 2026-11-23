//! Core 1.7 Resolve Check pipeline: deterministic seven-layer verification, optional quote-grounded
//! `HybridAssisted` LLM judge, Minimal-Cut-Set + Howard-VOI bounded Gap resolution reusing 1.6
//! `ContextAction` guardrails, active Counter-Evidence probing, and atomic persistence.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;

use pagesift_api::{ApiError, ErrorCode, PageBudget};
use pagesift_extract::resolve::{
    ResolveLearnState, build_conclusion_record, plan_counter_probes, run_local_counter_probes,
    verify_answer_deterministic,
};
use pagesift_graph::context::{
    ActionMode, ContextActionRequest, ContextPackRequest, ContextSearchRequest, UnitMatchMode,
};
use pagesift_graph::resolve::{
    ClaimVerdict, ConclusionRecord, EvidenceBelnap, EvidencePolarity, EvidenceRef, ResolutionState,
    ResolveCheckOutcome, ResolveCheckRequest, ResolveGap, ResolveStoppingReason, VerifierMode,
};
use pagesift_llm::{ChatMessage, ChatRequest, ResponseFormat, Role};
use pagesift_stat::beta::Posterior;
use pagesift_stat::decide::{Cost, Resource, value_of_information};
use pagesift_store::now_store_ms;

use crate::context::action::execute_action_with_budget;
use crate::context::action_support::url_permitted;
use crate::context::ingest::{
    ContextDocumentInput, LoadedContextCorpus, affinity_key, ingest_documents, load_corpus,
};
use crate::engine::{Scraper, cpu_task_keyed};
use crate::resolve::load_resolve_learn_state_with_warning;

// [策略] Resolve 补采动作先验期望发现率 alpha=3.0, beta=2.0 (均值 0.60)
const GAP_ACQ_PRIOR_ALPHA: f64 = 3.0;
const GAP_ACQ_PRIOR_BETA: f64 = 2.0;
// [策略] 单次补采或反证网络请求的基准 CPU 归一化成本 0.05
const RESOLVE_NET_CPU_COST: f64 = 0.05;
// [策略] 单次补采或反证网络请求的基准礼貌配额成本 0.15
const RESOLVE_NET_POLITENESS_COST: f64 = 0.15;
// [推导] 无排队拥塞时的最小动作机会成本底线 0.04（与 Context Action 对齐）
const RESOLVE_MIN_CHARGE_FLOOR: f64 = 0.04;
// [策略] HybridAssisted LLM 核验单条声明最多传入 4 条证据摘录
const HYBRID_MAX_EVIDENCE_PER_CLAIM: usize = 4;
// [策略] HybridAssisted LLM 核验单条证据摘录字符上限 600
const HYBRID_EXCERPT_LIMIT: usize = 600;
// [策略] HybridAssisted LLM 单次判定最大生成 token 数 256
const HYBRID_MAX_TOKENS: u32 = 256;
// [策略] Counter 网络反证最多执行前 2 个最高优先级探针，控制尾延迟与成本
const MAX_COUNTER_NET_PROBES: usize = 2;

/// Execute a full Resolve check (`Check` + optional `HybridAssisted` + optional bounded `Gap`
/// resolution + optional `Counter` probing + optional atomic persistence).
pub async fn execute_check_with_budget(
    scraper: &Scraper,
    tenant: Option<&str>,
    req: ResolveCheckRequest,
    inline_docs: Vec<ContextDocumentInput>,
    page_budget: Option<Arc<dyn PageBudget>>,
) -> Result<ResolveCheckOutcome, ApiError> {
    let t0 = Instant::now();
    let checked_at_ms = now_store_ms();

    if req.persist && scraper.store_ref().is_none() {
        return Err(ApiError::invalid(
            "persisting resolve conclusions requires a persistent store configured on Scraper",
        ));
    }

    // 1. Load corpus strictly honoring `as_of_ms` observation cutoff (prevents future leakage!)
    let mut corpus = load_resolve_corpus(scraper, tenant, &req, &inline_docs).await?;
    let mut warnings = corpus.warnings.clone();

    let (mut learn, learn_warn) = load_resolve_learn_state_with_warning(scraper, tenant).await;
    if let Some(w) = learn_warn {
        warnings.push(w);
    }

    // 2. Deterministic seven-layer Check on affinity CPU worker pool
    let mut conclusion = run_deterministic_check(&req, &corpus, &mut learn, checked_at_ms).await?;

    // 3. Optional HybridAssisted LLM judge ONLY for inconclusive claims, strictly quote-grounded!
    let mut llm_calls = 0usize;
    let mut verifier_mode_used = VerifierMode::DeterministicOnly;
    if req.verifier_mode == VerifierMode::HybridAssisted {
        let used = apply_hybrid_assisted_judge(
            scraper,
            &req,
            &corpus,
            &mut conclusion,
            checked_at_ms,
            &mut warnings,
        )
        .await;
        llm_calls = llm_calls.saturating_add(used);
        if used > 0 {
            verifier_mode_used = VerifierMode::HybridAssisted;
        }
    }

    // 4. Gap-driven bounded acquisition strictly reusing 1.6 ContextAction / Loop Sync guardrails
    let mut acquisition_steps = Vec::new();
    let mut network_requests_used = 0usize;
    let mut budget_left = req.max_acquisition_requests;
    let mut stopping_reason = initial_stopping_reason(&conclusion, &req);

    if req.enable_gap_resolution
        && !conclusion.unresolved_gaps.is_empty()
        && req.allow_network_acquisition
    {
        let mut blocked_any = false;
        let mut acquired_any = false;
        let mut voi_stopped = false;
        let mut budget_hit = budget_left == 0;

        // Validate explicit candidate URLs upfront against `allowedHosts` before touching network
        let valid_candidates: Vec<String> = req
            .candidate_urls
            .iter()
            .filter(|u| {
                let ok = url_permitted(u, &req.allowed_hosts);
                if !ok {
                    blocked_any = true;
                }
                ok
            })
            .cloned()
            .collect();

        let prices = scraper.shadow_prices();
        let unit_charge = prices
            .charge(
                &Cost::default()
                    .with(Resource::Cpu, RESOLVE_NET_CPU_COST)
                    .with(Resource::Politeness, RESOLVE_NET_POLITENESS_COST),
            )
            .max(RESOLVE_MIN_CHARGE_FLOOR);

        let gap_snapshot: Vec<ResolveGap> = conclusion.unresolved_gaps.clone();
        let mut attempted_queries: BTreeSet<String> = BTreeSet::new();

        for gap in gap_snapshot {
            if budget_left == 0 {
                budget_hit = true;
                break;
            }
            let target_query = gap.target_query.trim().to_owned();
            if target_query.is_empty() || !attempted_queries.insert(target_query.clone()) {
                continue;
            }

            // Howard VOI gating weighted by Minimal Cut Set criticality!
            let p_post = Posterior {
                alpha: GAP_ACQ_PRIOR_ALPHA,
                beta: GAP_ACQ_PRIOR_BETA,
            };
            let utility = f64::from(gap.criticality).max(0.25); // [策略] 非空缺口基础边际效用底线 0.25
            let voi = value_of_information(&p_post, utility, unit_charge);
            // [策略] 非单点关键缺口（criticality < 0.85）在负净 VOI 时早停
            if voi <= 0.0 && gap.criticality < 0.85 {
                voi_stopped = true;
                continue;
            }

            // Determine per-gap candidate URLs (cited inaccessible URLs or caller candidates)
            let mut gap_urls: Vec<String> = Vec::new();
            if target_query.starts_with("http://") || target_query.starts_with("https://") {
                if url_permitted(&target_query, &req.allowed_hosts) {
                    gap_urls.push(target_query.clone());
                } else {
                    blocked_any = true;
                    continue;
                }
            } else {
                gap_urls.extend(valid_candidates.iter().cloned());
            }

            if gap_urls.is_empty() && !req.allow_external_search && req.source_ids.is_empty() {
                continue;
            }

            // [策略] 单个缺口每次最多分配 2 个网络请求，保留配额给其余关键缺口与反证
            let step_cap = budget_left.min(2);
            let action_req = build_gap_action_request(&req, &target_query, gap_urls, step_cap);
            match execute_action_with_budget(
                scraper,
                tenant,
                action_req,
                Vec::new(),
                UnitMatchMode::BoundedUot,
                page_budget.clone(),
            )
            .await
            {
                Ok(act) => {
                    let spent = act.acquisition_steps.len();
                    network_requests_used = network_requests_used.saturating_add(spent);
                    budget_left = budget_left.saturating_sub(spent.max(1).min(budget_left));
                    for st in &act.acquisition_steps {
                        if st.units_added_or_updated > 0 {
                            acquired_any = true;
                        }
                    }
                    acquisition_steps.extend(act.acquisition_steps);
                    if act.stopping_reason
                        == pagesift_graph::context::StoppingReason::PermissionRestricted
                    {
                        blocked_any = true;
                    } else if act.stopping_reason
                        == pagesift_graph::context::StoppingReason::BudgetExhausted
                    {
                        budget_hit = true;
                        break;
                    }
                }
                Err(e) => {
                    warnings.push(format!(
                        "gap_acquisition_failed:{}: {}",
                        gap.gap_id, e.message
                    ));
                    budget_left = budget_left.saturating_sub(1);
                }
            }
        }

        if acquired_any {
            corpus = load_resolve_corpus(scraper, tenant, &req, &inline_docs).await?;
            conclusion = run_deterministic_check(&req, &corpus, &mut learn, now_store_ms()).await?;
            if req.verifier_mode == VerifierMode::HybridAssisted {
                let used = apply_hybrid_assisted_judge(
                    scraper,
                    &req,
                    &corpus,
                    &mut conclusion,
                    now_store_ms(),
                    &mut warnings,
                )
                .await;
                llm_calls = llm_calls.saturating_add(used);
                if used > 0 {
                    verifier_mode_used = VerifierMode::HybridAssisted;
                }
            }
        }

        stopping_reason = if conclusion.unresolved_gaps.is_empty() {
            initial_stopping_reason(&conclusion, &req)
        } else if blocked_any {
            ResolveStoppingReason::GapsRemainPermissionRestricted
        } else if budget_hit || budget_left == 0 {
            ResolveStoppingReason::BudgetExhausted
        } else if voi_stopped {
            ResolveStoppingReason::VoiBelowCost
        } else {
            ResolveStoppingReason::NoNewEvidence
        };
    }

    // 5. Active Counter-Evidence search & coverage audit (absence of counter-evidence is NOT proof!)
    if req.enable_counter_search {
        let plans = plan_counter_probes(&conclusion, &learn);
        let mut counter_blocked = Vec::new();
        let mut counter_failures = Vec::new();
        let mut counter_net_reqs = 0usize;

        if req.allow_network_acquisition
            && req.allow_external_search
            && budget_left > 0
            && !plans.is_empty()
        {
            let mut acquired_counter = false;
            for plan in plans.iter().take(MAX_COUNTER_NET_PROBES) {
                if budget_left == 0 {
                    break;
                }
                let action_req = build_gap_action_request(&req, &plan.query, Vec::new(), 1);
                match execute_action_with_budget(
                    scraper,
                    tenant,
                    action_req,
                    Vec::new(),
                    UnitMatchMode::BoundedUot,
                    page_budget.clone(),
                )
                .await
                {
                    Ok(act) => {
                        let spent = act.acquisition_steps.len();
                        counter_net_reqs = counter_net_reqs.saturating_add(spent);
                        network_requests_used = network_requests_used.saturating_add(spent);
                        budget_left = budget_left.saturating_sub(spent.max(1).min(budget_left));
                        for st in &act.acquisition_steps {
                            if st.units_added_or_updated > 0 {
                                acquired_counter = true;
                            }
                        }
                        if act.stopping_reason
                            == pagesift_graph::context::StoppingReason::PermissionRestricted
                        {
                            counter_blocked.push(plan.query.clone());
                        }
                        acquisition_steps.extend(act.acquisition_steps);
                    }
                    Err(e) => {
                        counter_failures.push(format!("{}: {}", plan.probe_id, e.message));
                        budget_left = budget_left.saturating_sub(1);
                    }
                }
            }
            if acquired_counter {
                corpus = load_resolve_corpus(scraper, tenant, &req, &inline_docs).await?;
            }
        }

        let mut audit = run_local_counter_probes(
            &mut conclusion,
            &plans,
            &corpus.index,
            req.as_of_ms,
            req.include_unverified,
        );
        if counter_net_reqs > 0 {
            for res in &mut audit.probe_results {
                if res.executed {
                    res.searched_network_requests = counter_net_reqs;
                    break;
                }
            }
        }
        audit.blocked_targets.extend(counter_blocked);
        audit.search_failures.extend(counter_failures);
        conclusion.counter_audit = Some(audit);
    }

    // 6. Optional atomic persistence into `ns::RESOLVE` (with cycle rejection and immutable history)
    let mut persisted = false;
    if req.persist {
        let store = scraper.store_ref().cloned().ok_or_else(|| {
            ApiError::invalid("persisting resolve conclusions requires persistent store")
        })?;
        let lock = scraper.resolve_tenant_lock(tenant);
        let _guard = lock.lock().await;
        let t_owned = tenant.map(str::to_owned);
        let rec_to_save = conclusion.clone();
        let saved = store
            .blocking(move |st| {
                let rs = st.resolve_for(t_owned.as_deref());
                let outcome = rs.commit_conclusion(rec_to_save.clone(), None, "resolve_check")?;
                let latest = rs
                    .get_conclusion(&outcome.conclusion_id)?
                    .unwrap_or(rec_to_save);
                Ok::<_, pagesift_store::Error>(latest)
            })
            .await
            .map_err(map_resolve_store_error)?;
        conclusion = saved;
        persisted = true;
    }

    let elapsed_micros = t0.elapsed().as_micros() as u64;
    Ok(ResolveCheckOutcome {
        conclusion,
        persisted,
        stopping_reason,
        verifier_mode_used,
        llm_calls,
        network_requests: network_requests_used,
        acquisition_steps,
        warnings,
        elapsed_micros,
    })
}

async fn load_resolve_corpus(
    scraper: &Scraper,
    tenant: Option<&str>,
    req: &ResolveCheckRequest,
    inline_docs: &[ContextDocumentInput],
) -> Result<LoadedContextCorpus, ApiError> {
    if !inline_docs.is_empty() {
        let persist_inline = false;
        let sid = req
            .source_ids
            .first()
            .map(String::as_str)
            .unwrap_or("ephemeral");
        let (corpus, _) = ingest_documents(
            scraper,
            tenant,
            sid,
            inline_docs.to_vec(),
            UnitMatchMode::BoundedUot,
            None,
            persist_inline,
        )
        .await?;
        Ok(corpus)
    } else if scraper.store_ref().is_some() {
        load_corpus(scraper, tenant, &req.source_ids, req.as_of_ms).await
    } else {
        Ok(LoadedContextCorpus {
            index: Arc::new(pagesift_extract::context::ContextIndex::default()),
            conflicts: Vec::new(),
            fact_clusters: Vec::new(),
            visibilities: Vec::new(),
            warnings: Vec::new(),
        })
    }
}

fn initial_stopping_reason(
    conclusion: &ConclusionRecord,
    req: &ResolveCheckRequest,
) -> ResolveStoppingReason {
    match conclusion.state {
        ResolutionState::Unverifiable | ResolutionState::CausalUnidentifiable => {
            ResolveStoppingReason::UnverifiableOrCausalBoundary
        }
        ResolutionState::Refuted | ResolutionState::Conflicted => {
            ResolveStoppingReason::RefutedOrConflicted
        }
        ResolutionState::Supported | ResolutionState::QualifiedInObservedScope
            if conclusion.unresolved_gaps.is_empty() =>
        {
            ResolveStoppingReason::Verified
        }
        _ if !conclusion.unresolved_gaps.is_empty()
            && (!req.enable_gap_resolution || !req.allow_network_acquisition) =>
        {
            ResolveStoppingReason::GapsRemainPermissionRestricted
        }
        _ if conclusion.unresolved_gaps.is_empty() => ResolveStoppingReason::Verified,
        _ => ResolveStoppingReason::NoNewEvidence,
    }
}

async fn run_deterministic_check(
    req: &ResolveCheckRequest,
    corpus: &LoadedContextCorpus,
    learn: &mut ResolveLearnState,
    checked_at_ms: u64,
) -> Result<ConclusionRecord, ApiError> {
    let key = affinity_key(req.query.as_deref().unwrap_or(&req.answer));
    let req_owned = req.clone();
    let idx = corpus.index.clone();
    let conflicts = corpus.conflicts.clone();
    let mut learn_owned = learn.clone();
    let (conclusion, updated_learn) = cpu_task_keyed(key, move || {
        let c = verify_answer_deterministic(
            &req_owned,
            &idx,
            &conflicts,
            &mut learn_owned,
            checked_at_ms,
        );
        (c, learn_owned)
    })
    .await?;
    *learn = updated_learn;
    Ok(conclusion)
}

fn build_gap_action_request(
    req: &ResolveCheckRequest,
    query: &str,
    candidate_urls: Vec<String>,
    max_requests: usize,
) -> ContextActionRequest {
    let search = ContextSearchRequest {
        query: query.to_owned(),
        source_ids: req.source_ids.clone(),
        as_of_ms: req.as_of_ms,
        effective_at_ms: req.effective_at_ms,
        include_unverified: req.include_unverified,
        ..ContextSearchRequest::default()
    };
    ContextActionRequest {
        mode: ActionMode::Verify,
        pack: ContextPackRequest {
            search,
            ..ContextPackRequest::default()
        },
        candidate_urls,
        allowed_hosts: req.allowed_hosts.clone(),
        allow_network_acquisition: req.allow_network_acquisition,
        allow_external_search: req.allow_external_search,
        strict_constrain_to_urls: req.strict_constrain_to_urls,
        max_acquisition_requests: max_requests,
        min_independent_sources: req.min_independent_sources,
        max_staleness_ms: None,
        since_delta_seq: None,
    }
}

/// Optional `HybridAssisted` LLM judge for uncertain claims ONLY:
/// - Never overrides hard deterministic vetoes (quantitative/period/temporal/polarity/causal/scope).
/// - Requires the LLM to cite a real `unit_id` and a verbatim `quote` substring actually present
///   inside that evidence unit; ungrounded quotes or invalid JSON fall back cleanly to deterministic!
async fn apply_hybrid_assisted_judge(
    scraper: &Scraper,
    req: &ResolveCheckRequest,
    corpus: &LoadedContextCorpus,
    conclusion: &mut ConclusionRecord,
    checked_at_ms: u64,
    warnings: &mut Vec<String>,
) -> usize {
    let Some(llm) = scraper.llm_client() else {
        warnings.push("llm_verifier_unavailable_fallback_to_deterministic".to_owned());
        return 0;
    };
    let model_name = llm.config().model.clone();
    let mut llm_calls = 0usize;
    let mut modified_any = false;
    let mut updated_verdicts: Vec<ClaimVerdict> = conclusion.claims.clone();

    for verdict in &mut updated_verdicts {
        // Only assist genuinely inconclusive claims with no deterministic hard veto
        if verdict.state != ResolutionState::Insufficient
            || !verdict.claim.verifiable
            || verdict.temporal_valid == Some(false)
            || verdict.quantitative_valid == Some(false)
            || verdict.polarity_consistent == Some(false)
            || verdict.causal_identifiable == Some(false)
            || verdict.claim.quantifier.is_scope_sensitive()
        {
            continue;
        }
        // Gather top candidate units from local corpus
        let mut search_req = ContextSearchRequest {
            query: verdict.claim.text.clone(),
            source_ids: req.source_ids.clone(),
            as_of_ms: req.as_of_ms,
            effective_at_ms: req.effective_at_ms,
            include_unverified: req.include_unverified,
            limit: HYBRID_MAX_EVIDENCE_PER_CLAIM,
            ..ContextSearchRequest::default()
        };
        let mut neutral_learn = pagesift_extract::context::ContextLearnState::default();
        neutral_learn.enabled = false;
        let rep = corpus.index.search(
            &search_req,
            &corpus.conflicts,
            &corpus.fact_clusters,
            Vec::new(),
            &mut neutral_learn,
        );
        search_req.limit = HYBRID_MAX_EVIDENCE_PER_CLAIM;
        if rep.hits.is_empty() {
            continue;
        }

        let mut evidence_prompt = String::new();
        for hit in rep.hits.iter().take(HYBRID_MAX_EVIDENCE_PER_CLAIM) {
            if let Some(u) = corpus.index.units.get(&hit.unit.unit_id) {
                let snippet = pagesift_graph::compact_excerpt(&u.text, HYBRID_EXCERPT_LIMIT);
                evidence_prompt.push_str(&format!("[{}] {}\n", u.unit_id, snippet));
            }
        }
        if evidence_prompt.is_empty() {
            continue;
        }

        let chat_req = ChatRequest {
            messages: vec![
                ChatMessage::new(
                    Role::System,
                    "You are a strict evidence verifier. Return ONLY JSON {\"verdict\":\"supported\"|\"refuted\"|\"insufficient\",\"unit_id\":\"...\",\"quote\":\"exact substring from unit\",\"reason\":\"short\"}. Never guess.",
                ),
                ChatMessage::new(
                    Role::User,
                    format!(
                        "Claim: {}\n\nEvidence:\n{}",
                        verdict.claim.text, evidence_prompt
                    ),
                ),
            ],
            temperature: Some(0.0),
            max_tokens: Some(HYBRID_MAX_TOKENS),
            response_format: Some(ResponseFormat::JsonObject),
        };

        llm_calls = llm_calls.saturating_add(1);
        let Ok(resp) = llm.chat(&chat_req).await else {
            warnings.push(format!("llm_verifier_fallback:{}", verdict.claim.claim_id));
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(resp.content.trim()) else {
            warnings.push(format!(
                "llm_verifier_invalid_json:{}",
                verdict.claim.claim_id
            ));
            continue;
        };
        let judge_label = parsed
            .get("verdict")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("insufficient");
        let cited_uid = parsed
            .get("unit_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let quote = parsed
            .get("quote")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim();
        let Some(unit) = corpus.index.units.get(cited_uid) else {
            warnings.push(format!(
                "llm_verifier_unknown_unit:{}",
                verdict.claim.claim_id
            ));
            continue;
        };
        // Deterministic quote-grounding guard: quote must be non-trivial (>= 8 chars) and verbatim!
        // [策略] 要求 LLM 引用原文子串至少 8 字节且精确命中证据正文，否则判为幻觉并回退
        if quote.len() < 8
            || !unit
                .text
                .to_ascii_lowercase()
                .contains(&quote.to_ascii_lowercase())
        {
            warnings.push(format!(
                "llm_verifier_ungrounded_quote:{}",
                verdict.claim.claim_id
            ));
            continue;
        }

        let polarity = match judge_label {
            "supported" => EvidencePolarity::Supports,
            "refuted" => EvidencePolarity::Refutes,
            _ => continue,
        };
        let root = if unit.origin_root.trim().is_empty() {
            unit.location.resource_key.clone()
        } else {
            unit.origin_root.to_ascii_lowercase()
        };
        let ev_ref = EvidenceRef {
            evidence_id: EvidenceRef::compute_id(&unit.unit_id, &unit.revision_id, polarity),
            epistemic_kind: pagesift_graph::resolve::EpistemicKind::Observation,
            polarity,
            unit_id: unit.unit_id.clone(),
            revision_id: unit.revision_id.clone(),
            content_hash: unit.content_hash.clone(),
            doc_id: unit.location.doc_id.clone(),
            resource_key: unit.location.resource_key.clone(),
            source_url: unit.location.source_url.clone(),
            origin_root: root.clone(),
            origin_independent: unit.origin_independent,
            byte_start: unit.location.byte_start,
            byte_end: unit.location.byte_end,
            page_number: unit.location.page_number,
            heading_path: unit.location.heading_path.clone(),
            observed_at_ms: unit.observed_at_ms,
            valid_from: unit.temporal.as_ref().and_then(|t| t.valid_from.clone()),
            valid_until: unit.temporal.as_ref().and_then(|t| t.valid_until.clone()),
            unit_status: unit.status.clone(),
            excerpt: pagesift_graph::compact_excerpt(quote, 220), // [策略] 引文摘录上限 220 字符
            explicitly_cited: false,
            match_reason: format!("hybrid_llm_quote_grounded:{model_name}"),
        };

        if polarity == EvidencePolarity::Supports {
            if !verdict.independent_support_roots.contains(&root) {
                verdict.independent_support_roots.push(root);
            }
            verdict.supporting_evidence.push(ev_ref);
            verdict.total_support_hits = verdict.supporting_evidence.len();
            verdict.belnap = EvidenceBelnap {
                has_support: true,
                has_refutation: verdict.belnap.has_refutation,
            };
            if verdict.independent_support_roots.len() >= req.min_independent_sources.max(1) {
                verdict.state = ResolutionState::Supported;
                verdict.semantic_supported = true;
            }
        } else {
            if !verdict.independent_refute_roots.contains(&root) {
                verdict.independent_refute_roots.push(root);
            }
            verdict.refuting_evidence.push(ev_ref);
            verdict.total_refute_hits = verdict.refuting_evidence.len();
            verdict.belnap = EvidenceBelnap {
                has_support: verdict.belnap.has_support,
                has_refutation: true,
            };
            verdict.state = ResolutionState::Refuted;
            verdict.semantic_supported = false;
        }
        verdict
            .reasons
            .push(format!("hybrid_llm_assisted:{model_name}"));
        modified_any = true;
    }

    if modified_any {
        *conclusion = build_conclusion_record(
            req,
            updated_verdicts,
            &corpus.index,
            checked_at_ms,
            Some(model_name),
        );
    }
    llm_calls
}

pub(crate) fn map_resolve_store_error(err: pagesift_store::Error) -> ApiError {
    let msg = err.to_string();
    if msg.contains("resolve_cycle_detected:") {
        ApiError::invalid(msg)
    } else {
        ApiError::new(ErrorCode::Internal, format!("resolve store error: {msg}"))
    }
}
