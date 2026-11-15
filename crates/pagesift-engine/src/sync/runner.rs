//! 持续数据同步 5 步闭环执行器（Sync → Adapt → Capture → Evidence → Feedback）。

use std::collections::{BTreeMap, BTreeSet};

use pagesift_api::{ApiError, ErrorCode};
use pagesift_net::Url;
use pagesift_plan::experience::FamilyExperienceTable;
use pagesift_plan::strategy::{AcquisitionStrategy, CaptureGoal};
use pagesift_store::{
    MAX_REQUESTS_PER_TICK_CAP, ResourceLifecycleState, ResourceRecord, SyncDelta, SyncTickStats,
};
use tokio::time::Instant;

use crate::engine::Scraper;
use crate::graph::now_ms;
use crate::graph::stage::with_graph_stage;

use super::budget::TickBudgetLedger;
use super::capture::{
    assess_document_capture, build_seeded_learner, is_barrier_doc, maybe_falsify_rewritten_rules,
    should_adopt_browser_escalation,
};
use super::inventory::{
    ensure_seeds_in_inventory, normalize_in_scope_url, prune_out_of_scope_inventory,
};
use super::reconcile::{reconcile_candidate_failure, reconcile_candidate_success};
use super::scheduler::select_tick_candidates;

/// 触发一次同步周期的运行选项。
#[derive(Debug, Clone, Default)]
pub struct SyncRunOptions {
    pub tenant: Option<String>,
    /// 是否忽略 `next_due_at_ms` 强制检查所有活跃资源。
    pub force_all: bool,
    pub max_requests_override: Option<usize>,
}

#[path = "runner_commit.rs"]
mod commit;
use commit::{TickCommitInputs, build_scrape_options_for_strategy, finalize_and_commit_sync_tick};

impl Scraper {
    /// 执行指定数据源的一次完整 1.5 Loop 闭环同步周期（Tick）。
    pub async fn run_sync_tick(
        &self,
        source_id: &str,
        run_opts: SyncRunOptions,
    ) -> Result<SyncDelta, ApiError> {
        let Some(store) = self.store_ref() else {
            return Err(ApiError::invalid(
                "sync requires persistent store enabled on Scraper",
            ));
        };
        let tenant = run_opts.tenant.as_deref();
        // C05: 单数据源单飞互斥锁，防止并发触发互相覆盖库存与 Delta
        let source_lock = self.sync_source_lock(tenant, source_id);
        let _source_guard = source_lock.lock().await;

        let sync_store = store.sync_for(tenant);
        let graph_store = store.graph_for(tenant);

        let Some(mut spec) = sync_store.get_source(source_id).map_err(|e| {
            ApiError::new(
                ErrorCode::Internal,
                format!("failed to load source '{source_id}': {e}"),
            )
        })?
        else {
            return Err(ApiError::new(
                ErrorCode::NotFound,
                format!("data source '{source_id}' not found"),
            ));
        };
        let expected_spec_revision = spec.revision;

        if let Some(max_req) = run_opts.max_requests_override {
            if max_req == 0 || max_req > MAX_REQUESTS_PER_TICK_CAP {
                return Err(ApiError::invalid(format!(
                    "maxRequests must be between 1 and {MAX_REQUESTS_PER_TICK_CAP}"
                )));
            }
            spec.budget.max_requests_per_tick = max_req;
        }
        spec.budget.max_requests_per_tick = spec
            .budget
            .max_requests_per_tick
            .clamp(1, MAX_REQUESTS_PER_TICK_CAP);

        if spec.extract_prompt.is_some() && self.llm_client().is_none() {
            return Err(ApiError::unsupported(
                "extract_prompt requires an LLM: set [llm] in pagesift.toml",
            ));
        }

        let started_at_ms = now_ms();
        let wall_start = Instant::now();
        let tick_id = format!("tick-{started_at_ms}");
        let contract_id = spec.contract_id();
        let has_llm = self.llm_client().is_some();

        let existing_inv = sync_store.get_inventory(source_id).map_err(|e| {
            ApiError::new(
                ErrorCode::Internal,
                format!("failed to load inventory: {e}"),
            )
        })?;
        let mut inventory: BTreeMap<String, ResourceRecord> = existing_inv
            .into_iter()
            .map(|r| (r.resource_key.clone(), r))
            .collect();
        prune_out_of_scope_inventory(&spec, &mut inventory);
        ensure_seeds_in_inventory(&spec, &mut inventory, started_at_ms);

        let experience = FamilyExperienceTable::new();
        if let Ok(Some(exp_bytes)) = sync_store.get_experience_bytes(source_id) {
            let _ = experience.decode_merge_json(&exp_bytes);
        }
        let mut learner = build_seeded_learner(&inventory, &experience);

        let mut ledger = TickBudgetLedger::new(&spec.budget);
        let mut stats = SyncTickStats {
            discovered_total: inventory.len(),
            ..Default::default()
        };
        let mut added_resources = Vec::new();
        let mut modified_resources = Vec::new();
        let mut migrated_resources = Vec::new();
        let mut disappeared_resources = Vec::new();
        let mut adapt_decisions = Vec::new();
        let mut prepared_bundles = Vec::new();
        let mut active_subjects: BTreeSet<String> = BTreeSet::new();
        let mut visited_keys: BTreeSet<String> = BTreeSet::new();
        let mut budget_exhausted = false;
        let mut early_stopped_reasons = Vec::new();

        let capture_goal = CaptureGoal {
            min_markdown_chars: 32,
            required_fields: spec.required_fields.clone(),
            require_evidence_grounded: !spec.required_fields.is_empty(),
            expect_links_for_hub: false,
        };

        loop {
            if ledger.is_expired() || ledger.requests_used >= ledger.max_requests {
                break;
            }
            if let Some(target_items) = spec.budget.target_items {
                let active_count = inventory
                    .values()
                    .filter(|r| r.state == ResourceLifecycleState::Active)
                    .count();
                if active_count >= target_items && stats.attempted_total > 0 {
                    early_stopped_reasons.push(format!(
                        "stopped early after reaching targetItems={target_items}"
                    ));
                    break;
                }
            }

            let unvisited_vec: Vec<ResourceRecord> = inventory
                .values()
                .filter(|r| !visited_keys.contains(&r.resource_key))
                .cloned()
                .collect();
            if unvisited_vec.is_empty() {
                break;
            }
            let mut wave_spec = spec.clone();
            wave_spec.budget.max_requests_per_tick = ledger
                .max_requests
                .saturating_sub(ledger.requests_used)
                .clamp(1, MAX_REQUESTS_PER_TICK_CAP);
            let wave_now_ms = now_ms().max(started_at_ms);
            let (candidates, _, wave_due) = select_tick_candidates(
                &wave_spec,
                &unvisited_vec,
                &experience,
                wave_now_ms,
                run_opts.force_all,
            );
            if stats.candidates_due == 0 {
                stats.candidates_due = wave_due;
            }
            if candidates.is_empty() {
                break;
            }

            for cand in candidates {
                if ledger.is_expired() {
                    budget_exhausted = true;
                    break;
                }
                if let Some(target_items) = spec.budget.target_items {
                    let active_count = inventory
                        .values()
                        .filter(|r| r.state == ResourceLifecycleState::Active)
                        .count();
                    if active_count >= target_items && stats.attempted_total > 0 {
                        early_stopped_reasons.push(format!(
                            "stopped early after reaching targetItems={target_items}"
                        ));
                        break;
                    }
                }

                let mut executed_strategy = cand.decision.strategy;
                if executed_strategy == AcquisitionStrategy::DirectBrowser
                    && !ledger.can_afford_browser()
                {
                    executed_strategy = AcquisitionStrategy::LightweightHttp;
                }
                if executed_strategy == AcquisitionStrategy::ConditionalHttp
                    && (cand.record.last_validator_contract_id.as_deref()
                        != Some(contract_id.as_str())
                        || !cand.record.baseline_sufficient)
                {
                    executed_strategy = AcquisitionStrategy::LightweightHttp;
                }

                if !ledger.try_reserve(executed_strategy == AcquisitionStrategy::DirectBrowser) {
                    budget_exhausted = true;
                    break;
                }
                visited_keys.insert(cand.record.resource_key.clone());
                stats.attempted_total += 1;

                let cand = &cand;

                let rec_key = cand.record.resource_key.clone();
                if let Ok(prev_claims) = graph_store.get_resource_claims(&rec_key) {
                    for c in prev_claims {
                        active_subjects.insert(c.claim.subject_key);
                    }
                }
                let prev_state = inventory.get(&rec_key).map(|r| r.state);
                let prev_doc_id = inventory
                    .get(&rec_key)
                    .and_then(|r| r.latest_doc_id.clone());

                let mut fetch_url = cand.record.url.clone();
                let mut applied_rules = Vec::new();
                if let Ok(mut parsed_u) = Url::parse(&fetch_url) {
                    applied_rules = experience.rewrite_url(&mut parsed_u);
                    if !applied_rules.is_empty() {
                        let cand_rewritten = parsed_u.to_string();
                        if normalize_in_scope_url(&spec, &cand_rewritten).is_some() {
                            fetch_url = cand_rewritten;
                        } else {
                            // URL 重写越出 Scope 时立即废止改写，保留合法原 URL（C21）
                            if let Ok(orig_u) = Url::parse(&cand.record.url)
                                && let Some(h) = orig_u.host_str()
                            {
                                for r in &applied_rules {
                                    experience.falsify_url_rule(h, r, now_ms());
                                }
                            }
                            applied_rules.clear();
                        }
                    }
                }

                let mut scrape_opts = build_scrape_options_for_strategy(
                    &spec,
                    &cand.record,
                    executed_strategy,
                    tenant,
                    &contract_id,
                    has_llm,
                );
                scrape_opts.timeout = Some(ledger.remaining_ms().max(1));

                let step_start = Instant::now();
                let (mut scrape_res, mut staged_map) =
                    with_graph_stage(ledger.run_bounded(self.scrape(&fetch_url, &scrape_opts)))
                        .await;

                // URL 重写命中 4xx/5xx/拦截或内容背离时，受预算约束撤销规则并回退原 URL（C09 / R04）
                if maybe_falsify_rewritten_rules(
                    &mut learner,
                    &experience,
                    &cand.record.url,
                    &applied_rules,
                    scrape_res.as_ref(),
                    &cand.record,
                    now_ms(),
                ) {
                    if ledger.try_reserve(executed_strategy == AcquisitionStrategy::DirectBrowser) {
                        scrape_opts.timeout = Some(ledger.remaining_ms().max(1));
                        let (retry_res, retry_staged) = with_graph_stage(
                            ledger.run_bounded(self.scrape(&cand.record.url, &scrape_opts)),
                        )
                        .await;
                        scrape_res = retry_res;
                        staged_map = retry_staged;
                        fetch_url = cand.record.url.clone();
                    } else {
                        budget_exhausted = true;
                    }
                }

                let mut http_assessment = None;
                let mut browser_gain_obs = None;
                let mut browser_attempted = executed_strategy == AcquisitionStrategy::DirectBrowser;

                if executed_strategy != AcquisitionStrategy::DirectBrowser
                    && let Ok(ref http_doc) = scrape_res
                {
                    let h_assess =
                        assess_document_capture(&spec, &cand.record, http_doc, &capture_goal);
                    let should_try_browser = spec.allow_browser
                        && !is_barrier_doc(http_doc)
                        && http_doc.metadata.status_code < 400
                        && (!h_assess.sufficient || h_assess.needs_browser_escalation);
                    if should_try_browser {
                        if ledger.try_reserve(true) {
                            browser_attempted = true;
                            let mut b_opts = scrape_opts.clone();
                            b_opts.engine = Some("browser".to_string());
                            b_opts.if_none_match = None;
                            b_opts.if_modified_since = None;
                            b_opts.timeout = Some(ledger.remaining_ms().max(1));
                            let (b_res, mut b_staged) = with_graph_stage(
                                ledger.run_bounded(self.scrape(&fetch_url, &b_opts)),
                            )
                            .await;
                            match b_res {
                                Ok(b_doc) => {
                                    let b_assess = assess_document_capture(
                                        &spec,
                                        &cand.record,
                                        &b_doc,
                                        &capture_goal,
                                    );
                                    let (adopt, gained) = should_adopt_browser_escalation(
                                        http_doc, &h_assess, &b_doc, &b_assess,
                                    );
                                    browser_gain_obs = Some(gained);
                                    if adopt {
                                        executed_strategy = AcquisitionStrategy::HttpVerifyBrowser;
                                        scrape_res = Ok(b_doc);
                                        staged_map.extend(b_staged.drain());
                                    }
                                }
                                Err(_) => {
                                    browser_gain_obs = Some(false);
                                }
                            }
                        } else {
                            budget_exhausted = true;
                        }
                    }
                    http_assessment = Some(h_assess);
                }

                let step_cost_ms = step_start.elapsed().as_millis() as f64;
                let obs_time_ms = now_ms();
                let mut recorded_dec = cand.decision.clone();
                recorded_dec.strategy = executed_strategy;
                if let Ok(serde_dec) = serde_json::to_value(&recorded_dec) {
                    adapt_decisions.push(serde_dec);
                }

                match scrape_res {
                    Ok(doc) => {
                        let final_assessment =
                            assess_document_capture(&spec, &cand.record, &doc, &capture_goal);
                        let staged = doc
                            .graph
                            .as_ref()
                            .and_then(|g| staged_map.remove(&g.doc_id));
                        if let Some(g) = doc.graph.as_ref() {
                            active_subjects.insert(g.subject_key.key.clone());
                            for c in &g.claims {
                                active_subjects.insert(c.subject_key.clone());
                            }
                        }
                        reconcile_candidate_success(
                            &spec,
                            cand,
                            &doc,
                            staged,
                            executed_strategy,
                            http_assessment.as_ref(),
                            &final_assessment,
                            browser_gain_obs,
                            step_cost_ms,
                            obs_time_ms,
                            prev_state,
                            prev_doc_id,
                            &mut inventory,
                            &experience,
                            &mut learner,
                            &graph_store,
                            &mut prepared_bundles,
                            &mut stats,
                            &mut added_resources,
                            &mut modified_resources,
                            &mut migrated_resources,
                            &mut disappeared_resources,
                        )?;
                    }
                    Err(e) => {
                        reconcile_candidate_failure(
                            &spec,
                            cand,
                            executed_strategy,
                            browser_attempted,
                            step_cost_ms,
                            obs_time_ms,
                            &e.message,
                            &mut inventory,
                            &experience,
                            &graph_store,
                            &mut prepared_bundles,
                            &mut stats,
                        )?;
                    }
                }
            }
            if early_stopped_reasons
                .last()
                .is_some_and(|r| r.contains("targetItems="))
                || ledger.is_expired()
            {
                break;
            }
        }

        finalize_and_commit_sync_tick(TickCommitInputs {
            source_id,
            sync_store: &sync_store,
            spec: &spec,
            expected_spec_revision,
            tick_id,
            started_at_ms,
            wall_start,
            force_all: run_opts.force_all,
            inventory,
            visited_keys: &visited_keys,
            experience: &experience,
            ledger: &ledger,
            stats,
            budget_exhausted,
            early_stopped_reasons,
            added_resources,
            modified_resources,
            migrated_resources,
            disappeared_resources,
            adapt_decisions,
            prepared_bundles,
            active_subjects,
        })
    }
}
