//! 持续数据同步 5 步闭环执行器（Sync → Adapt → Capture → Evidence → Feedback）。

use std::collections::{BTreeMap, BTreeSet};

use pagesift_api::{ApiError, ErrorCode};
use pagesift_plan::experience::{FamilyExperienceTable, FamilyStats};
use pagesift_plan::render::family;
use pagesift_plan::strategy::{AcquisitionStrategy, CaptureGoal};
use pagesift_store::{
    MAX_REQUESTS_PER_TICK_CAP, ResourceLifecycleState, ResourceRecord, SyncDelta, SyncTickStats,
};
use tokio::time::Instant;

use crate::engine::Scraper;
use crate::graph::now_ms;
use crate::graph::stage::with_graph_stage;

use super::budget::{TickBudgetLedger, is_budget_exhausted_step, unwrap_tick_step};
use super::capture::{
    assess_capture_reliability, assess_document_capture, build_seeded_learner,
    has_embedded_json_signal, is_barrier_doc, maybe_falsify_rewritten_rules,
    prepare_rewritten_fetch_url, should_adopt_browser_escalation,
};
use super::inventory::{ensure_seeds_in_inventory, prune_out_of_scope_inventory};
use super::reconcile::{
    ExecutionTelemetry, HttpAttemptMeta, reconcile_candidate_failure, reconcile_candidate_success,
};
use super::scheduler::{
    is_record_due, refresh_candidate_decision, select_tick_candidates_with_context,
};

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

fn erase_scrape_fut<'a>(
    fut: impl std::future::Future<Output = Result<pagesift_api::types::Document, ApiError>> + Send + 'a,
) -> std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<pagesift_api::types::Document, ApiError>>
            + Send
            + 'a,
    >,
> {
    Box::pin(fut)
}

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
        if let Some(exp_bytes) = sync_store.get_experience_bytes(source_id).map_err(|e| {
            ApiError::new(
                ErrorCode::Internal,
                format!("failed to load experience: {e}"),
            )
        })? && !exp_bytes.is_empty()
        {
            let _ = experience.decode_merge_json(&exp_bytes);
        }

        // G4 Reuse：同一租户作用域内若存在冷启动路径族，从同租户成熟兄弟源/族接种受控先验（严格禁止跨租户）
        if let Ok(other_sources) = sync_store.list_sources() {
            let mut peer_stats: Vec<FamilyStats> = Vec::new();
            for other in other_sources {
                if other.id == source_id {
                    continue;
                }
                if let Ok(Some(bytes)) = sync_store.get_experience_bytes(&other.id)
                    && !bytes.is_empty()
                {
                    let peer_tbl = FamilyExperienceTable::new();
                    if peer_tbl.decode_merge_json(&bytes) {
                        for mut st in peer_tbl.list_families() {
                            if st.transfer.tenant_id.is_none() && tenant.is_some() {
                                st.transfer.tenant_id = tenant.map(str::to_string);
                            }
                            peer_stats.push(st);
                        }
                    }
                }
            }
            if !peer_stats.is_empty() {
                for rec in inventory.values() {
                    let fam_name = family(&rec.url);
                    experience.ensure_cold_start_seeded_for_tenant(&fam_name, tenant, &peer_stats);
                }
            }
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
        let mut due_keys_seen: BTreeSet<String> = BTreeSet::new();
        let mut budget_exhausted = false;
        let mut early_stopped_reasons = Vec::new();

        let capture_goal = CaptureGoal {
            min_markdown_chars: 32,
            required_fields: spec.required_fields.clone(),
            require_evidence_grounded: !spec.required_fields.is_empty(),
            expect_links_for_hub: false,
        };

        loop {
            let live_spec = sync_store.get_source(source_id).map_err(|e| {
                ApiError::new(
                    ErrorCode::Internal,
                    format!("failed to verify source '{source_id}': {e}"),
                )
            })?;
            if !live_spec.is_some_and(|s| s.instance_id == spec.instance_id) {
                return Err(ApiError::new(
                    ErrorCode::NotFound,
                    format!("data source '{source_id}' was deleted during sync tick"),
                ));
            }
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
            let (candidates, _, _) = select_tick_candidates_with_context(
                &wave_spec,
                &unvisited_vec,
                &experience,
                wave_now_ms,
                run_opts.force_all,
                ledger.browser_remaining_fraction(),
                self.tab_utilization(),
            );
            for r in &unvisited_vec {
                if is_record_due(r, wave_now_ms, run_opts.force_all) {
                    due_keys_seen.insert(r.resource_key.clone());
                }
            }
            // 跨波次按唯一 resource_key 统计真实到期候选总数，防止多波次重复计入第 1 波未执行的到期项
            stats.candidates_due = due_keys_seen.len();
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

                let live_decision = refresh_candidate_decision(
                    &spec,
                    &cand.record,
                    &cand.decision,
                    &experience,
                    ledger.remaining_request_fraction(),
                    ledger.browser_remaining_fraction(),
                    self.tab_utilization(),
                );
                let mut executed_strategy = live_decision.strategy;
                let mut escalation_allowed = live_decision.browser_escalation_permitted;
                if executed_strategy == AcquisitionStrategy::DeferOrSkip {
                    // 高壁垒/极紧预算在波次中重评为延期时跳过本候选，不强耗预算
                    continue;
                }
                if executed_strategy == AcquisitionStrategy::DirectBrowser
                    && !ledger.can_afford_browser()
                {
                    executed_strategy = AcquisitionStrategy::LightweightHttp;
                    escalation_allowed = false;
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
                let prev_claims = graph_store.get_resource_claims(&rec_key).map_err(|e| {
                    ApiError::new(ErrorCode::Internal, format!("load resource claims: {e}"))
                })?;
                for c in prev_claims {
                    active_subjects.insert(c.claim.subject_key);
                }
                let prev_state = inventory.get(&rec_key).map(|r| r.state);
                let prev_doc_id = inventory
                    .get(&rec_key)
                    .and_then(|r| r.latest_doc_id.clone());

                let (mut fetch_url, applied_rules) =
                    prepare_rewritten_fetch_url(&spec, &experience, &cand.record.url, now_ms());

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
                let (mut step_res, mut staged_map) = with_graph_stage(
                    ledger
                        .run_tick_bounded(erase_scrape_fut(self.scrape(&fetch_url, &scrape_opts))),
                )
                .await;

                if is_budget_exhausted_step(&ledger, &step_res) {
                    budget_exhausted = true;
                    visited_keys.remove(&rec_key);
                    stats.attempted_total = stats.attempted_total.saturating_sub(1);
                    break;
                }

                // 仅确认改写目标明确 404/410 或 canonical/内容背离时才撤销规则；预算/超时/网络失败绝不误伤（R04 / P1-03）
                let mut scrape_res = unwrap_tick_step(step_res);
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
                        let (retry_step, retry_staged) = with_graph_stage(ledger.run_tick_bounded(
                            erase_scrape_fut(self.scrape(&cand.record.url, &scrape_opts)),
                        ))
                        .await;
                        if is_budget_exhausted_step(&ledger, &retry_step) {
                            budget_exhausted = true;
                            visited_keys.remove(&rec_key);
                            stats.attempted_total = stats.attempted_total.saturating_sub(1);
                            break;
                        }
                        step_res = retry_step;
                        scrape_res = unwrap_tick_step(step_res);
                        staged_map = retry_staged;
                        fetch_url = cand.record.url.clone();
                    } else {
                        budget_exhausted = true;
                    }
                }

                let mut http_meta = None;
                let mut browser_gain_obs = None;
                let initial_elapsed_ms = step_start.elapsed().as_millis() as f64;
                let direct_browser_unreliable = executed_strategy
                    == AcquisitionStrategy::DirectBrowser
                    && scrape_res
                        .as_ref()
                        .is_ok_and(|d| assess_capture_reliability(d).is_unreliable());
                let mut exec_telemetry = ExecutionTelemetry {
                    used_browser: executed_strategy == AcquisitionStrategy::DirectBrowser,
                    browser_cost_ms: (executed_strategy == AcquisitionStrategy::DirectBrowser)
                        .then_some(initial_elapsed_ms),
                    browser_failed: (executed_strategy == AcquisitionStrategy::DirectBrowser).then(
                        || {
                            direct_browser_unreliable
                                || scrape_res.as_ref().map_or(true, |d| {
                                    is_barrier_doc(d) || d.metadata.status_code >= 400
                                })
                        },
                    ),
                    browser_unreliable: direct_browser_unreliable,
                    tenant_id: tenant.map(str::to_string),
                };

                if executed_strategy != AcquisitionStrategy::DirectBrowser
                    && let Ok(ref http_doc) = scrape_res
                {
                    let h_unreliable = assess_capture_reliability(http_doc).is_unreliable();
                    let h_assess =
                        assess_document_capture(&spec, &cand.record, http_doc, &capture_goal);
                    let h_is_304 = http_doc
                        .metadata
                        .pagesift
                        .as_ref()
                        .is_some_and(|p| p.not_modified);
                    let h_ok = !is_barrier_doc(http_doc) && http_doc.metadata.status_code < 400;
                    http_meta = Some(HttpAttemptMeta {
                        http_ok: h_ok,
                        not_modified_304: h_is_304,
                        sufficient: h_assess.sufficient && !h_unreliable,
                        has_embedded_json: has_embedded_json_signal(http_doc),
                        http_unreliable: h_unreliable,
                        http_cost_ms: initial_elapsed_ms,
                    });
                    // 影子价格卸载、配额禁止或单源关闭浏览器时，`escalation_allowed == false`，
                    // 严格禁止执行层无条件升阶绕过自适应策略决策。
                    let should_try_browser = escalation_allowed
                        && spec.allow_browser
                        && h_ok
                        && (!h_assess.sufficient
                            || h_assess.needs_browser_escalation
                            || h_unreliable);
                    if should_try_browser {
                        if ledger.try_reserve(true) {
                            let b_start = Instant::now();
                            let mut b_opts = scrape_opts.clone();
                            b_opts.engine = Some("browser".to_string());
                            b_opts.if_none_match = None;
                            b_opts.if_modified_since = None;
                            b_opts.timeout = Some(ledger.remaining_ms().max(1));
                            let (b_step, mut b_staged) = with_graph_stage(ledger.run_tick_bounded(
                                erase_scrape_fut(self.scrape(&fetch_url, &b_opts)),
                            ))
                            .await;
                            let b_elapsed_ms = b_start.elapsed().as_millis() as f64;
                            exec_telemetry.used_browser = true;
                            exec_telemetry.browser_cost_ms = Some(b_elapsed_ms);
                            if is_budget_exhausted_step(&ledger, &b_step) {
                                budget_exhausted = true;
                                browser_gain_obs = None;
                                exec_telemetry.browser_failed = Some(true);
                            } else {
                                match b_step {
                                    Ok(b_doc) => {
                                        let b_unreliable =
                                            assess_capture_reliability(&b_doc).is_unreliable();
                                        let b_assess = assess_document_capture(
                                            &spec,
                                            &cand.record,
                                            &b_doc,
                                            &capture_goal,
                                        );
                                        let b_err = is_barrier_doc(&b_doc)
                                            || b_doc.metadata.status_code >= 400;
                                        exec_telemetry.browser_unreliable = b_unreliable;
                                        exec_telemetry.browser_failed = Some(b_err || b_unreliable);
                                        let (adopt, gained_obs) = should_adopt_browser_escalation(
                                            http_doc, &h_assess, &b_doc, &b_assess,
                                        );
                                        browser_gain_obs = if h_is_304 { None } else { gained_obs };
                                        if adopt {
                                            executed_strategy =
                                                AcquisitionStrategy::HttpVerifyBrowser;
                                            scrape_res = Ok(b_doc);
                                            staged_map.extend(b_staged.drain());
                                        }
                                    }
                                    Err(_) => {
                                        browser_gain_obs = None;
                                        exec_telemetry.browser_failed = Some(true);
                                    }
                                }
                            }
                        } else {
                            budget_exhausted = true;
                        }
                    }
                }

                let step_cost_ms = step_start.elapsed().as_millis() as f64;
                let obs_time_ms = now_ms();
                let mut live_cand = cand.clone();
                live_cand.decision = live_decision.clone();
                let mut recorded_dec = live_decision;
                recorded_dec.strategy = executed_strategy;
                recorded_dec.browser_escalation_permitted = escalation_allowed;
                if let Ok(serde_dec) = serde_json::to_value(&recorded_dec) {
                    adapt_decisions.push(serde_dec);
                }

                match scrape_res {
                    Ok(doc) => {
                        let final_assessment =
                            assess_document_capture(&spec, &live_cand.record, &doc, &capture_goal);
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
                            &live_cand,
                            &doc,
                            staged,
                            executed_strategy,
                            http_meta,
                            &final_assessment,
                            browser_gain_obs,
                            &exec_telemetry,
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
                            &live_cand,
                            executed_strategy,
                            tenant,
                            step_cost_ms,
                            obs_time_ms,
                            &e,
                            &mut inventory,
                            &experience,
                            &graph_store,
                            &mut prepared_bundles,
                            &mut stats,
                        )?;
                    }
                }
                if budget_exhausted || ledger.is_expired() {
                    budget_exhausted = true;
                    break;
                }
            }
            if early_stopped_reasons
                .last()
                .is_some_and(|r| r.contains("targetItems="))
                || budget_exhausted
                || ledger.is_expired()
            {
                break;
            }
        }

        let delta = finalize_and_commit_sync_tick(TickCommitInputs {
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
        })?;

        // Release per-source Sync lock BEFORE optional Context catch-up so `sync_locks` and
        // `context_locks` are never nested, eliminating lock-order inversion deadlocks.
        drop(_source_guard);

        // Opt-in 1.6 Context incremental maintenance: O(1) in-memory hash probe (`has_meta`),
        // zero disk I/O and zero overhead when the source has not activated Context!
        if store
            .context_for(tenant)
            .has_meta(source_id)
            .unwrap_or(false)
        {
            let _ = crate::context::sync_source_context(
                self,
                tenant,
                source_id,
                pagesift_graph::context::UnitMatchMode::BoundedUot,
                None,
            )
            .await;
        }

        Ok(delta)
    }
}
