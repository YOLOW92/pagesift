//! Scrape-option synthesis and atomic tick-commit finalization for sync ticks.

use std::collections::{BTreeMap, BTreeSet};

use pagesift_api::types::{Format, ScrapeOptions};
use pagesift_api::{ApiError, ErrorCode};
use pagesift_graph::GraphOptions;
use pagesift_plan::experience::FamilyExperienceTable;
use pagesift_plan::strategy::{AcquisitionStrategy, CoverageInput, evaluate_coverage_status};
use pagesift_store::{
    DataSourceSpec, PreparedVersionBundle, ResourceChangeEntry, ResourceLifecycleState,
    ResourceRecord, SyncDelta, SyncStore, SyncTickStats, TickCommitOutcome, TickCommitRequest,
};
use tokio::time::Instant;

use crate::graph::now_ms;

use super::super::budget::TickBudgetLedger;
use super::super::scheduler::select_tick_candidates;

pub(super) struct TickCommitInputs<'a> {
    pub source_id: &'a str,
    pub sync_store: &'a SyncStore<'a>,
    pub spec: &'a DataSourceSpec,
    pub expected_spec_revision: u64,
    pub tick_id: String,
    pub started_at_ms: u64,
    pub wall_start: Instant,
    pub force_all: bool,
    pub inventory: BTreeMap<String, ResourceRecord>,
    pub visited_keys: &'a BTreeSet<String>,
    pub experience: &'a FamilyExperienceTable,
    pub ledger: &'a TickBudgetLedger,
    pub stats: SyncTickStats,
    pub budget_exhausted: bool,
    pub early_stopped_reasons: Vec<String>,
    pub added_resources: Vec<ResourceChangeEntry>,
    pub modified_resources: Vec<ResourceChangeEntry>,
    pub migrated_resources: Vec<ResourceChangeEntry>,
    pub disappeared_resources: Vec<ResourceChangeEntry>,
    pub adapt_decisions: Vec<serde_json::Value>,
    pub prepared_bundles: Vec<PreparedVersionBundle>,
    pub active_subjects: BTreeSet<String>,
}

pub(super) fn build_scrape_options_for_strategy(
    spec: &DataSourceSpec,
    rec: &ResourceRecord,
    strategy: AcquisitionStrategy,
    tenant: Option<&str>,
    contract_id: &str,
    has_llm: bool,
) -> ScrapeOptions {
    let mut formats = vec![Format::new("markdown"), Format::new("links")];
    if has_llm && (spec.extract_schema.is_some() || spec.extract_prompt.is_some()) {
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

    let validator_eligible = strategy == AcquisitionStrategy::ConditionalHttp
        && rec.last_validator_contract_id.as_deref() == Some(contract_id)
        && rec.baseline_sufficient;
    let (if_none_match, if_modified_since) = if validator_eligible {
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
        // Sync Tick 统一在末尾通过 AtomicTx 提交图版本、声明与库存（C06）
        graph: Some(GraphOptions::enabled().as_attempt()),
        ..Default::default()
    }
}

pub(super) fn finalize_and_commit_sync_tick(
    inputs: TickCommitInputs<'_>,
) -> Result<SyncDelta, ApiError> {
    let source_id = inputs.source_id;
    let sync_store = inputs.sync_store;
    let spec = inputs.spec;
    let expected_spec_revision = inputs.expected_spec_revision;
    let tick_id = inputs.tick_id;
    let started_at_ms = inputs.started_at_ms;
    let wall_start = inputs.wall_start;
    let inventory = inputs.inventory;
    let visited_keys = inputs.visited_keys;
    let experience = inputs.experience;
    let ledger = inputs.ledger;
    let mut stats = inputs.stats;
    let mut budget_exhausted = inputs.budget_exhausted;
    let early_stopped_reasons = inputs.early_stopped_reasons;
    let added_resources = inputs.added_resources;
    let modified_resources = inputs.modified_resources;
    let migrated_resources = inputs.migrated_resources;
    let disappeared_resources = inputs.disappeared_resources;
    let adapt_decisions = inputs.adapt_decisions;
    let prepared_bundles = inputs.prepared_bundles;
    let active_subjects = inputs.active_subjects;

    let completed_at_ms = now_ms().max(started_at_ms + 1);
    let remaining_unvisited: Vec<ResourceRecord> = inventory
        .values()
        .filter(|r| !visited_keys.contains(&r.resource_key))
        .cloned()
        .collect();
    let (_, _, remaining_due) = select_tick_candidates(
        spec,
        &remaining_unvisited,
        experience,
        completed_at_ms,
        inputs.force_all,
    );
    let remaining_discovered = remaining_unvisited
        .iter()
        .filter(|r| r.state == ResourceLifecycleState::Discovered)
        .count();
    let deferred_count = remaining_due.max(remaining_discovered);
    if deferred_count > 0 && (ledger.requests_used >= ledger.max_requests || ledger.is_expired()) {
        budget_exhausted = true;
    }

    stats.requests_executed = ledger.requests_used;
    stats.browser_renders_used = ledger.browser_renders_used;
    stats.discovered_total = inventory.len();
    stats.deferred_count = deferred_count;
    stats.unchecked_discovered = inventory
        .values()
        .filter(|r| r.state == ResourceLifecycleState::Discovered)
        .count();
    stats.elapsed_ms = wall_start.elapsed().as_millis() as u64;

    let suspected_gone_count = inventory
        .values()
        .filter(|r| r.state == ResourceLifecycleState::SuspectedGone)
        .count();
    let fresh_unexpired = inventory
        .values()
        .filter(|r| {
            !matches!(
                r.state,
                ResourceLifecycleState::Discovered
                    | ResourceLifecycleState::TransientFailure
                    | ResourceLifecycleState::SuspectedGone
            ) && r.next_due_at_ms > completed_at_ms
        })
        .count();
    let target_reached = spec.budget.target_items.is_some_and(|t| {
        inventory
            .values()
            .filter(|r| r.state == ResourceLifecycleState::Active)
            .count()
            >= t
    });
    let browser_budget_hit =
        stats.browser_renders_used >= ledger.max_browser_renders && stats.insufficient_captures > 0;
    let budget_exhausted = budget_exhausted || browser_budget_hit;
    let cov = evaluate_coverage_status(&CoverageInput {
        discovered_total: stats.discovered_total,
        authoritative_successes: stats.authoritative_successes,
        attempted_total: stats.attempted_total,
        deferred_count: stats.deferred_count,
        unchecked_total: stats.unchecked_discovered,
        transient_failures: stats.transient_failures,
        access_restricted_count: stats.access_restricted_count,
        insufficient_captures: stats.insufficient_captures,
        pending_confirmation: suspected_gone_count,
        fresh_unexpired,
        target_goal_reached: target_reached,
        budget_exhausted,
    });
    let cov_status_str = serde_json::to_value(cov.status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "complete_within_scope".to_string());
    let mut coverage_reasons = cov.reasons;
    if browser_budget_hit {
        coverage_reasons.push("browser_budget_exhausted".to_string());
    }
    coverage_reasons.extend(early_stopped_reasons);

    let mut inv_final: Vec<ResourceRecord> = inventory.into_values().collect();
    let exp_bytes = experience.encode_json();

    let mut delta = SyncDelta {
        source_id: source_id.to_string(),
        instance_id: spec.instance_id.clone(),
        tick_id,
        tick_seq: 0,
        started_at_ms,
        completed_at_ms,
        coverage_status: cov_status_str,
        coverage_reasons,
        stats,
        added_resources,
        modified_resources,
        migrated_resources,
        disappeared_resources,
        claim_transitions: Vec::new(),
        conflicts: Vec::new(),
        adapt_decisions,
    };

    let commit_out = sync_store
        .commit_tick_atomic(TickCommitRequest {
            source_id,
            expected_instance_id: &spec.instance_id,
            expected_spec_revision,
            spec_revision_used: spec.revision,
            inventory: &mut inv_final,
            experience_bytes: &exp_bytes,
            delta: &mut delta,
            graph_bundles: prepared_bundles,
            conflict_subjects: &active_subjects,
            conflicts: &[],
        })
        .map_err(|e| {
            ApiError::new(
                ErrorCode::Internal,
                format!("failed to commit sync tick atomically: {e}"),
            )
        })?;

    match commit_out {
        TickCommitOutcome::Committed { .. } => Ok(delta),
        TickCommitOutcome::SourceDeleted => Err(ApiError::new(
            ErrorCode::NotFound,
            format!("data source '{source_id}' was deleted during sync tick"),
        )),
        TickCommitOutcome::StaleSpec { .. } => Err(ApiError::new(
            ErrorCode::Busy,
            format!("data source '{source_id}' spec changed concurrently during sync tick"),
        )),
    }
}
