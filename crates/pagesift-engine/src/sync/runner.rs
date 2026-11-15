//! 持续数据同步 5 步闭环执行器（Sync → Adapt → Capture → Evidence → Feedback）。

use std::collections::BTreeMap;
use std::time::Instant;

use pagesift_api::types::{Format, ScrapeOptions};
use pagesift_api::{ApiError, ErrorCode};
use pagesift_graph::{ClaimStatus, GraphOptions, detect_conflicts};
use pagesift_net::Url;
use pagesift_plan::experience::FamilyExperienceTable;
use pagesift_plan::strategy::{
    AcquisitionStrategy, CaptureGoal, CaptureSignals, evaluate_capture_sufficiency,
    evaluate_coverage_status,
};
use pagesift_store::{
    DataSourceSpec, ResourceLifecycleState, ResourceRecord, SyncDelta, SyncTickStats,
};

use crate::engine::Scraper;
use crate::graph::now_ms;

use super::inventory::ensure_seeds_in_inventory;
use super::reconcile::reconcile_candidate_success;
use super::scheduler::{compute_next_due_at_ms, select_tick_candidates};

/// 触发一次同步周期的运行选项。
#[derive(Debug, Clone, Default)]
pub struct SyncRunOptions {
    pub tenant: Option<String>,
    /// 是否忽略 `next_due_at_ms` 强制检查所有未失联资源。
    pub force_all: bool,
    pub max_requests_override: Option<usize>,
}

fn build_scrape_options_for_strategy(
    spec: &DataSourceSpec,
    rec: &ResourceRecord,
    strategy: AcquisitionStrategy,
    tenant: Option<&str>,
) -> ScrapeOptions {
    let mut formats = vec![Format::new("markdown"), Format::new("links")];
    if spec.extract_schema.is_some() || spec.extract_prompt.is_some() {
        let mut fmt = Format::new("json");
        if let Some(ref schema) = spec.extract_schema {
            fmt.options.insert("schema".to_string(), schema.clone());
        }
        if let Some(ref prompt) = spec.extract_prompt {
            fmt.options.insert(
                "prompt".to_string(),
                serde_json::Value::String(prompt.clone()),
            );
        }
        formats.push(fmt);
    }

    let engine = match strategy {
        AcquisitionStrategy::DirectBrowser => Some("browser".to_string()),
        AcquisitionStrategy::ConditionalHttp
        | AcquisitionStrategy::LightweightHttp
        | AcquisitionStrategy::HttpWithEmbeddedJson
        | AcquisitionStrategy::HttpVerifyBrowser
        | AcquisitionStrategy::DeferOrSkip => Some("http".to_string()),
    };

    let (if_none_match, if_modified_since) = if strategy == AcquisitionStrategy::ConditionalHttp {
        (rec.etag.clone(), rec.last_modified.clone())
    } else {
        (None, None)
    };

    ScrapeOptions {
        formats,
        engine,
        max_age: Some(0),
        store_in_cache: true,
        store_snapshot: true,
        if_none_match,
        if_modified_since,
        owner: tenant.map(str::to_string),
        graph: Some(GraphOptions::enabled()),
        ..Default::default()
    }
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

        if let Some(max_req) = run_opts.max_requests_override {
            spec.budget.max_requests_per_tick = max_req.max(1);
        }

        let started_at_ms = now_ms();
        let start_inst = Instant::now();
        let tick_id = format!("tick-{started_at_ms}");

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
        ensure_seeds_in_inventory(&spec, &mut inventory, started_at_ms);

        let experience = FamilyExperienceTable::new();
        if let Ok(Some(exp_bytes)) = sync_store.get_experience_bytes(source_id) {
            let _ = experience.decode_merge_json(&exp_bytes);
        }

        let inv_vec: Vec<ResourceRecord> = inventory.values().cloned().collect();
        let (candidates, mut deferred_count, candidates_due) = select_tick_candidates(
            &spec,
            &inv_vec,
            &experience,
            started_at_ms,
            run_opts.force_all,
        );

        let mut stats = SyncTickStats {
            discovered_total: inventory.len(),
            candidates_due,
            ..Default::default()
        };
        let mut added_resources = Vec::new();
        let mut modified_resources = Vec::new();
        let mut migrated_resources = Vec::new();
        let mut disappeared_resources = Vec::new();
        let mut claim_transitions = Vec::new();
        let mut adapt_decisions = Vec::new();
        let mut budget_exhausted = deferred_count > 0;

        let capture_goal = CaptureGoal {
            min_markdown_chars: 48,
            required_fields: spec.required_fields.clone(),
            require_evidence_grounded: !spec.required_fields.is_empty(),
            expect_links_for_hub: false,
        };

        for (idx, cand) in candidates.iter().enumerate() {
            if start_inst.elapsed().as_millis() as u64 >= spec.budget.max_wall_clock_ms {
                deferred_count += candidates.len().saturating_sub(idx);
                budget_exhausted = true;
                break;
            }
            if let Some(target_items) = spec.budget.target_items {
                let active_count = inventory
                    .values()
                    .filter(|r| r.state == ResourceLifecycleState::Active)
                    .count();
                if active_count >= target_items && idx > 0 {
                    break;
                }
            }

            let rec_key = cand.record.resource_key.clone();
            let prev_state = inventory.get(&rec_key).map(|r| r.state);
            let prev_doc_id = inventory
                .get(&rec_key)
                .and_then(|r| r.latest_doc_id.clone());

            let mut fetch_url = cand.record.url.clone();
            let mut applied_rules = Vec::new();
            if let Ok(mut parsed_u) = Url::parse(&fetch_url) {
                applied_rules = experience.rewrite_url(&mut parsed_u);
                if !applied_rules.is_empty() {
                    fetch_url = parsed_u.to_string();
                }
            }

            let mut strategy_used = cand.decision.strategy;
            if strategy_used == AcquisitionStrategy::DirectBrowser
                && stats.browser_renders_used >= spec.budget.max_browser_renders_per_tick
            {
                strategy_used = AcquisitionStrategy::LightweightHttp;
            }

            let mut scrape_opts =
                build_scrape_options_for_strategy(&spec, &cand.record, strategy_used, tenant);
            let step_start = Instant::now();
            let mut scrape_res = self.scrape(&fetch_url, &scrape_opts).await;
            stats.requests_executed += 1;
            if strategy_used == AcquisitionStrategy::DirectBrowser {
                stats.browser_renders_used += 1;
            }

            if !applied_rules.is_empty() {
                let bad_rewrite = match &scrape_res {
                    Ok(doc) => doc.metadata.status_code >= 400,
                    Err(_) => true,
                };
                if bad_rewrite {
                    if let Ok(u) = Url::parse(&cand.record.url)
                        && let Some(host) = u.host_str()
                    {
                        let t = now_ms();
                        for r in &applied_rules {
                            experience.falsify_url_rule(host, r, t);
                        }
                    }
                    fetch_url = cand.record.url.clone();
                    scrape_res = self.scrape(&fetch_url, &scrape_opts).await;
                    stats.requests_executed += 1;
                }
            }

            let mut browser_gain_obs = None;
            if strategy_used == AcquisitionStrategy::HttpVerifyBrowser
                && let Ok(ref http_doc) = scrape_res
            {
                let mut goal_for_rec = capture_goal.clone();
                goal_for_rec.expect_links_for_hub = cand.record.role.is_hub_like();
                let md_chars = http_doc.markdown.as_deref().map_or(0, str::len);
                let not_mod = http_doc
                    .metadata
                    .pagesift
                    .as_ref()
                    .is_some_and(|p| p.not_modified);
                let extracted_keys: Vec<String> = http_doc
                    .extract
                    .as_ref()
                    .and_then(|v| v.as_object())
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                let grounded_count = http_doc.graph.as_ref().map_or(0, |g| g.evidence.len());
                let assessment = evaluate_capture_sufficiency(
                    &goal_for_rec,
                    &CaptureSignals {
                        status_code: http_doc.metadata.status_code,
                        not_modified_304: not_mod,
                        markdown_chars: md_chars,
                        has_embedded_json: grounded_count > 0,
                        has_client_shell_markers: md_chars < 48,
                        extracted_fields_present: extracted_keys,
                        grounded_evidence_count: grounded_count,
                        unaligned_claim_count: 0,
                        outgoing_links_count: http_doc.links.as_ref().map_or(0, Vec::len),
                        is_barrier: matches!(http_doc.metadata.status_code, 403 | 429),
                    },
                );
                if assessment.needs_browser_escalation
                    && stats.browser_renders_used < spec.budget.max_browser_renders_per_tick
                {
                    let http_claims = http_doc.graph.as_ref().map_or(0, |g| g.claims.len());
                    scrape_opts.engine = Some("browser".to_string());
                    scrape_opts.if_none_match = None;
                    scrape_opts.if_modified_since = None;
                    if let Ok(b_doc) = self.scrape(&fetch_url, &scrape_opts).await {
                        stats.requests_executed += 1;
                        stats.browser_renders_used += 1;
                        let b_chars = b_doc.markdown.as_deref().map_or(0, str::len);
                        let b_claims = b_doc.graph.as_ref().map_or(0, |g| g.claims.len());
                        browser_gain_obs = Some(b_chars > md_chars + 32 || b_claims > http_claims);
                        scrape_res = Ok(b_doc);
                    }
                }
            }

            let step_cost_ms = step_start.elapsed().as_millis() as f64;
            let obs_time_ms = now_ms();
            if let Ok(serde_dec) = serde_json::to_value(&cand.decision) {
                adapt_decisions.push(serde_dec);
            }

            match scrape_res {
                Ok(doc) => {
                    reconcile_candidate_success(
                        &spec,
                        cand,
                        &doc,
                        strategy_used,
                        browser_gain_obs,
                        step_cost_ms,
                        obs_time_ms,
                        prev_state,
                        prev_doc_id,
                        &mut inventory,
                        &experience,
                        &graph_store,
                        &mut stats,
                        &mut added_resources,
                        &mut modified_resources,
                        &mut migrated_resources,
                        &mut disappeared_resources,
                        &mut claim_transitions,
                    );
                }
                Err(_) => {
                    stats.transient_failures += 1;
                    if let Some(rec_mut) = inventory.get_mut(&rec_key) {
                        rec_mut.consecutive_failures =
                            rec_mut.consecutive_failures.saturating_add(1);
                        rec_mut.state = ResourceLifecycleState::TransientFailure;
                        rec_mut.last_checked_at_ms = Some(obs_time_ms);
                        rec_mut.next_due_at_ms =
                            compute_next_due_at_ms(&spec, rec_mut, None, obs_time_ms, false);
                    }
                }
            }
        }

        let completed_at_ms = now_ms().max(started_at_ms + 1);
        stats.discovered_total = inventory.len();
        stats.deferred_count = deferred_count;
        stats.elapsed_ms = start_inst.elapsed().as_millis() as u64;

        let target_reached = spec.budget.target_items.is_some_and(|t| {
            inventory
                .values()
                .filter(|r| r.state == ResourceLifecycleState::Active)
                .count()
                >= t
        });
        let cov = evaluate_coverage_status(
            stats.discovered_total,
            stats.requests_executed,
            stats.deferred_count,
            stats.transient_failures,
            stats.access_restricted_count,
            target_reached,
            budget_exhausted,
        );
        let cov_status_str = serde_json::to_value(cov.status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "complete_within_scope".to_string());

        let inv_final: Vec<ResourceRecord> = inventory.into_values().collect();
        sync_store
            .put_inventory(source_id, &inv_final, completed_at_ms)
            .map_err(|e| {
                ApiError::new(
                    ErrorCode::Internal,
                    format!("failed to persist inventory: {e}"),
                )
            })?;
        let exp_bytes = experience.encode_json();
        sync_store
            .put_experience_bytes(source_id, &exp_bytes, completed_at_ms)
            .map_err(|e| {
                ApiError::new(
                    ErrorCode::Internal,
                    format!("failed to persist experience: {e}"),
                )
            })?;

        let mut active_raw_claims = Vec::new();
        for r in &inv_final {
            if r.state == ResourceLifecycleState::Active
                && let Ok(claims) = graph_store.get_resource_claims(&r.resource_key)
            {
                for c in claims {
                    if matches!(
                        c.status,
                        ClaimStatus::Active | ClaimStatus::Unverified { .. }
                    ) {
                        active_raw_claims.push(c.claim);
                    }
                }
            }
        }
        let conflicts = detect_conflicts(&active_raw_claims);

        let delta = SyncDelta {
            source_id: source_id.to_string(),
            tick_id,
            started_at_ms,
            completed_at_ms,
            coverage_status: cov_status_str,
            coverage_reasons: cov.reasons,
            stats,
            added_resources,
            modified_resources,
            migrated_resources,
            disappeared_resources,
            claim_transitions,
            conflicts,
            adapt_decisions,
        };
        sync_store.append_delta(&delta).map_err(|e| {
            ApiError::new(
                ErrorCode::Internal,
                format!("failed to persist sync delta: {e}"),
            )
        })?;

        Ok(delta)
    }
}
