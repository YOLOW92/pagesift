//! 单页同步采集结果的证据与清单状态调和（Step 4 & Step 5）。

use std::collections::BTreeMap;

use pagesift_api::ApiError;
use pagesift_api::types::Document;
use pagesift_graph::{
    ClaimTransitionKind, DocNode, ObservationOutcome, ProvenanceEdge, RedirectKind,
    parse_timestamp_ms,
};
use pagesift_plan::experience::{FamilyExperienceTable, FamilyObservation};
use pagesift_plan::render::family;
use pagesift_plan::strategy::{AcquisitionStrategy, CaptureAssessment};
use pagesift_store::{
    DataSourceSpec, GraphStore, PreparedVersionBundle, ResourceChangeEntry, ResourceLifecycleState,
    ResourceRecord, SyncTickStats,
};

use super::capture::{
    assess_capture_reliability, has_embedded_json_signal, is_barrier_doc,
    sync_learner_into_experience,
};
use super::inventory::{InventoryObservation, apply_observation_to_record, normalize_in_scope_url};
use super::scheduler::{ScheduledCandidate, compute_next_due_at_ms};
use crate::frontier::Learner;
use crate::graph::stage::StagedGraphCommit;

#[path = "reconcile_stage.rs"]
mod stage;
pub(super) use stage::reconcile_candidate_failure;
use stage::{insert_discovered_resource, stage_bundle_for_candidate};

/// 沿重定向尾部检查仅当通向 `final_url` 的连续永久重定向链成立时返回迁移目标（C22）。
pub(super) fn permanent_redirect_target_from_provenance(
    provenance: &[ProvenanceEdge],
    requested_url: &str,
    final_url: &str,
) -> Option<String> {
    let mut expected_to = DocNode::derive_resource_key(None, final_url);
    let mut found_perm_for_requested = false;
    let req_key = DocNode::derive_resource_key(None, requested_url);

    let redirects: Vec<(&str, &str, RedirectKind)> = provenance
        .iter()
        .filter_map(|e| match e {
            ProvenanceEdge::Redirect {
                from_url,
                to_url,
                redirect_kind,
                ..
            } => Some((from_url.as_str(), to_url.as_str(), *redirect_kind)),
            _ => None,
        })
        .collect();

    for (from_u, to_u, kind) in redirects.into_iter().rev() {
        let to_key = DocNode::derive_resource_key(None, to_u);
        if to_key != expected_to || kind != RedirectKind::HttpPermanent {
            break;
        }
        let from_key = DocNode::derive_resource_key(None, from_u);
        if from_key == req_key {
            found_perm_for_requested = true;
        }
        expected_to = from_key;
    }
    found_perm_for_requested.then(|| final_url.to_string())
}

#[derive(Debug, Clone, Copy)]
pub(super) struct HttpAttemptMeta {
    pub http_ok: bool,
    pub not_modified_304: bool,
    pub sufficient: bool,
    pub has_embedded_json: bool,
    pub http_unreliable: bool,
    pub http_cost_ms: f64,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ExecutionTelemetry {
    pub used_browser: bool,
    pub browser_cost_ms: Option<f64>,
    pub browser_failed: Option<bool>,
    pub browser_unreliable: bool,
    pub tenant_id: Option<String>,
}

/// 解析并校验服务端 `Last-Modified` 响应头的可信度，严格区分真实内容更新时间与动态伪造/滞后头。
fn validate_credible_last_modified(
    rec: &ResourceRecord,
    lm_raw: Option<&str>,
    obs_time_ms: u64,
    substantive_changed: bool,
    not_modified_304: bool,
) -> (Option<u64>, bool) {
    if rec.last_modified_dynamic {
        return (None, false);
    }
    let Some(raw) = lm_raw else {
        return if not_modified_304 {
            (rec.last_modified_at_ms, false)
        } else {
            (None, false)
        };
    };
    let Some(parsed) = parse_timestamp_ms(raw) else {
        return (None, false);
    };
    if parsed > obs_time_ms.saturating_add(5_000) {
        return (None, false);
    }
    let bounded = parsed.min(obs_time_ms);
    if not_modified_304 {
        return (Some(bounded), false);
    }
    if !substantive_changed {
        let hugs_now = obs_time_ms.saturating_sub(bounded) <= 1_500 && rec.latest_doc_id.is_some();
        let drifted_without_content_change = rec
            .last_modified_at_ms
            .is_some_and(|prev_lm| bounded > prev_lm.saturating_add(2_000));
        if hugs_now || drifted_without_content_change {
            return (None, true);
        }
    } else {
        let froze_across_edit = rec
            .last_modified_at_ms
            .is_some_and(|prev_lm| bounded <= prev_lm);
        let older_than_unchanged = rec
            .learn
            .last_confirmed_unchanged_at_ms
            .is_some_and(|unch| bounded < unch);
        if froze_across_edit || older_than_unchanged {
            return (None, false);
        }
    }
    (Some(bounded), false)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn reconcile_candidate_success(
    spec: &DataSourceSpec,
    cand: &ScheduledCandidate,
    doc: &Document,
    staged: Option<StagedGraphCommit>,
    strategy_used: AcquisitionStrategy,
    http_meta: Option<HttpAttemptMeta>,
    final_assessment: &CaptureAssessment,
    browser_gain_obs: Option<bool>,
    exec: &ExecutionTelemetry,
    step_cost_ms: f64,
    obs_time_ms: u64,
    prev_state: Option<ResourceLifecycleState>,
    prev_doc_id: Option<String>,
    inventory: &mut BTreeMap<String, ResourceRecord>,
    experience: &FamilyExperienceTable,
    learner: &mut Learner,
    graph_store: &GraphStore<'_>,
    prepared_bundles: &mut Vec<PreparedVersionBundle>,
    stats: &mut SyncTickStats,
    added_resources: &mut Vec<ResourceChangeEntry>,
    modified_resources: &mut Vec<ResourceChangeEntry>,
    migrated_resources: &mut Vec<ResourceChangeEntry>,
    disappeared_resources: &mut Vec<ResourceChangeEntry>,
) -> Result<(), ApiError> {
    let rec_key = cand.record.resource_key.clone();
    let status_code = doc.metadata.status_code;
    let reliability = assess_capture_reliability(doc);
    let not_modified_304 = doc
        .metadata
        .pagesift
        .as_ref()
        .is_some_and(|p| p.not_modified)
        && !reliability.invalid_304;
    let is_barrier = is_barrier_doc(doc);
    let unreliable = reliability.is_unreliable();
    let http_ok = !is_barrier && status_code < 400;
    let capture_sufficient = final_assessment.sufficient && !unreliable;

    if unreliable {
        stats.uncertain_observations += 1;
    }
    if cand.decision.exploration_probe && !exec.used_browser && http_ok && capture_sufficient {
        stats.probe_recoveries += 1;
    }
    if not_modified_304 && capture_sufficient {
        stats.conditional_304_hits += 1;
    }
    if is_barrier {
        stats.access_restricted_count += 1;
    } else if status_code >= 500 {
        stats.transient_failures += 1;
    } else if !unreliable && !not_modified_304 && status_code < 400 && !capture_sufficient {
        stats.insufficient_captures += 1;
    }

    let slice = doc.graph.as_ref();
    let (sub_blocks, noise_blocks) = slice
        .and_then(|g| g.version.as_ref())
        .and_then(|v| v.diff_stats.as_ref())
        .map_or((0, 0), |d| (d.substantive, d.noise));
    let page_transitions = slice.map(|g| &g.claim_transitions[..]).unwrap_or(&[]);
    let claim_count = slice.map_or(0, |g| g.claims.len());

    let non_confirm_transition = page_transitions
        .iter()
        .any(|t| !matches!(t.kind, ClaimTransitionKind::Confirmed));
    let substantive_changed = if unreliable {
        false
    } else if not_modified_304 {
        capture_sufficient && sub_blocks > 0
    } else if http_ok && capture_sufficient {
        prev_doc_id.is_none() || sub_blocks > 0 || non_confirm_transition
    } else {
        false
    };
    let noise_only = !unreliable
        && !not_modified_304
        && http_ok
        && capture_sufficient
        && !substantive_changed
        && noise_blocks > 0;

    if !unreliable && !not_modified_304 && http_ok && capture_sufficient {
        stats.authoritative_successes += 1;
        if prev_doc_id.is_some() {
            if substantive_changed {
                stats.substantive_updates += 1;
            } else if noise_only {
                stats.noise_only_updates += 1;
            } else {
                stats.unchanged_200_hits += 1;
            }
        }
    } else if not_modified_304 && capture_sufficient {
        stats.authoritative_successes += 1;
    }

    let fam_name = family(&cand.record.url);
    let dt_secs = cand
        .record
        .valid_observation_anchor_ms()
        .map(|last| (obs_time_ms.saturating_sub(last) as f64 / 1000.0).max(1.0));
    let has_links = doc.links.as_ref().is_some_and(|l| !l.is_empty());
    let has_embedded = has_embedded_json_signal(doc);
    let md_str = doc.markdown.as_deref().unwrap_or_default();
    let text_sig = (!unreliable && http_ok && capture_sufficient && !not_modified_304)
        .then(|| Learner::content_signature(md_str));

    if !unreliable && http_ok && capture_sufficient && !not_modified_304 {
        sync_learner_into_experience(learner, experience, &cand.record.url, md_str, obs_time_ms);
    }

    let final_url = doc
        .metadata
        .url
        .clone()
        .unwrap_or_else(|| cand.record.url.clone());
    let perm_target = slice.and_then(|g| {
        permanent_redirect_target_from_provenance(&g.provenance, &cand.record.url, &final_url)
            .and_then(|t| normalize_in_scope_url(spec, &t))
    });
    let ps_meta = doc.metadata.pagesift.as_ref();
    let doc_id = slice.map(|g| g.doc_id.clone());
    let contract_id = spec.contract_id();

    let (trusted_lm_ms, mark_lm_dynamic) = if !unreliable && http_ok && capture_sufficient {
        validate_credible_last_modified(
            &cand.record,
            ps_meta.and_then(|p| p.last_modified.as_deref()),
            obs_time_ms,
            substantive_changed,
            not_modified_304,
        )
    } else {
        (None, false)
    };

    let mut migrated_to_url = None;
    let mut current_state = cand.record.state;
    let mut detected_res_period = None;
    if let Some(rec_mut) = inventory.get_mut(&rec_key) {
        migrated_to_url = apply_observation_to_record(
            spec,
            rec_mut,
            &InventoryObservation {
                now_ms: obs_time_ms,
                status_code,
                not_modified_304,
                is_barrier_or_error: is_barrier,
                had_transport_error: false,
                unreliable_observation: unreliable,
                last_modified_at_ms: trusted_lm_ms,
                mark_last_modified_dynamic: mark_lm_dynamic,
                final_url: final_url.clone(),
                canonical_url: doc.metadata.canonical_url.clone(),
                permanent_redirect_target: perm_target,
                etag: ps_meta.and_then(|p| p.etag.clone()),
                last_modified: ps_meta.and_then(|p| p.last_modified.clone()),
                contract_id: contract_id.clone(),
                capture_sufficient,
                has_embedded_json: has_embedded,
                text_signature: text_sig,
                content_hash: ps_meta.and_then(|p| p.content_hash.clone()),
                doc_id: doc_id.clone(),
                title: doc.metadata.title.clone(),
                claim_count,
                substantive_changed,
                strategy_used: strategy_used.as_str().to_string(),
            },
        );
        if !unreliable && http_ok && capture_sufficient {
            detected_res_period = rec_mut
                .learn
                .detected_period()
                .map(|p| (p.mean_secs, p.std_secs));
        }
    }

    let encountered_barrier = if is_barrier {
        Some(true)
    } else if status_code < 400 && !unreliable {
        Some(false)
    } else {
        None
    };

    // 独立判断 HTTP 模态与 Browser 模态可靠性：
    // - HTTP 可靠信号由 http_meta.http_unreliable 单独控制，不因 Browser 成功/失败或不可靠而连坐或误信任；
    // - Browser 增益仅在 HTTP 与 Browser 均可靠时进入统计。
    experience.record_observation_for_tenant(
        &fam_name,
        exec.tenant_id.as_deref(),
        &FamilyObservation {
            observed_at_ms: obs_time_ms,
            http_sufficient: http_meta
                .filter(|m| m.http_ok && !m.not_modified_304 && !m.http_unreliable)
                .map(|m| m.sufficient),
            browser_gain: browser_gain_obs.filter(|_| !exec.browser_unreliable),
            conditional_hit: (!unreliable
                && strategy_used == AcquisitionStrategy::ConditionalHttp
                && http_ok)
                .then_some(not_modified_304 && capture_sufficient),
            embedded_json_observed: http_meta
                .filter(|m| m.http_ok && !m.not_modified_304 && !m.http_unreliable)
                .map(|m| m.has_embedded_json),
            substantive_change: (!unreliable
                && http_ok
                && capture_sufficient
                && prev_doc_id.is_some())
            .then(|| dt_secs.map(|dt| (dt, substantive_changed)))
            .flatten(),
            yielded_claims_or_links: (!unreliable
                && http_ok
                && capture_sufficient
                && !not_modified_304)
                .then_some(claim_count > 0 || has_links),
            encountered_barrier,
            cost_ms: step_cost_ms,
            unreliable_observation: unreliable,
            used_browser: exec.used_browser,
            is_304: not_modified_304 && capture_sufficient,
            http_cost_ms: http_meta
                .filter(|m| !m.http_unreliable)
                .map(|m| m.http_cost_ms),
            browser_cost_ms: exec.browser_cost_ms,
            browser_failed: exec.browser_failed.map(|f| f || exec.browser_unreliable),
            last_modified_at_ms: (!unreliable && http_ok && capture_sufficient)
                .then_some(trusted_lm_ms)
                .flatten(),
            resource_detected_period_secs: detected_res_period,
        },
    );

    let fam_stats = experience.get_family(&fam_name);
    if let Some(rec_mut) = inventory.get_mut(&rec_key) {
        // C19: 状态、连续失败与权重迁移完毕并更新族经验后，再基于最新状态计算 next_due_at_ms
        rec_mut.next_due_at_ms = compute_next_due_at_ms(
            spec,
            rec_mut,
            fam_stats.as_ref(),
            obs_time_ms,
            substantive_changed,
        );
        current_state = rec_mut.state;
    }

    let obs_outcome = if unreliable {
        // 不确定解码或截断观测不具备权威性，既有 Active 声明转为 Unverified 而非误撤销（G1/G5）
        ObservationOutcome::TransientFetchError {
            error: "unreliable_capture: uncertain_decode_or_truncated".to_string(),
        }
    } else if not_modified_304 && capture_sufficient {
        ObservationOutcome::NotModified304
    } else if status_code == 410 || current_state == ResourceLifecycleState::ConfirmedGone {
        ObservationOutcome::ConfirmedGone {
            reason: format!("HTTP {status_code}"),
        }
    } else if status_code == 404 {
        ObservationOutcome::SuspectedGone {
            status_code: Some(404),
        }
    } else if is_barrier || status_code >= 400 || !capture_sufficient {
        ObservationOutcome::TransientFetchError {
            error: if !capture_sufficient {
                format!("capture_insufficient: {:?}", final_assessment.reasons)
            } else {
                format!("HTTP {status_code}")
            },
        }
    } else {
        ObservationOutcome::VerifiedContent
    };

    stage_bundle_for_candidate(
        graph_store,
        prepared_bundles,
        &rec_key,
        &contract_id,
        &obs_outcome,
        slice,
        staged.as_ref(),
        doc,
        obs_time_ms,
    )?;

    let change_entry = ResourceChangeEntry {
        resource_key: rec_key,
        url: cand.record.url.clone(),
        previous_state: prev_state,
        current_state,
        migrated_to: migrated_to_url.clone(),
        doc_id,
        prev_doc_id: prev_doc_id.clone(),
        substantive_blocks_changed: sub_blocks,
        noise_blocks_changed: noise_blocks,
        strategy_used: strategy_used.as_str().to_string(),
        not_modified_304: not_modified_304 && capture_sufficient,
    };

    match current_state {
        ResourceLifecycleState::Migrated
            if prev_state != Some(ResourceLifecycleState::Migrated) =>
        {
            migrated_resources.push(change_entry);
        }
        ResourceLifecycleState::SuspectedGone | ResourceLifecycleState::ConfirmedGone
            if prev_state != Some(current_state) =>
        {
            disappeared_resources.push(change_entry);
        }
        ResourceLifecycleState::Active if http_ok && capture_sufficient => {
            if prev_state == Some(ResourceLifecycleState::Discovered) || prev_doc_id.is_none() {
                added_resources.push(change_entry);
            } else if substantive_changed
                || matches!(
                    prev_state,
                    Some(
                        ResourceLifecycleState::ConfirmedGone
                            | ResourceLifecycleState::SuspectedGone
                            | ResourceLifecycleState::Migrated
                    )
                )
            {
                modified_resources.push(change_entry);
            }
        }
        _ => {}
    }

    if let Some(target_u) = migrated_to_url
        && let Some(norm_target) = normalize_in_scope_url(spec, &target_u)
    {
        insert_discovered_resource(
            inventory,
            norm_target,
            Some(cand.record.url.clone()),
            cand.record.depth,
            cand.record.importance_weight,
            obs_time_ms,
        );
    }

    if cand.record.depth < spec.max_depth && http_ok && capture_sufficient {
        let next_depth = cand.record.depth + 1;
        let hub_propagates = cand.record.role.is_hub_like() && substantive_changed && !unreliable;
        let min_interval_ms = spec.freshness.min_interval_secs.max(1).saturating_mul(1000);
        for link in doc.links.as_deref().unwrap_or(&[]) {
            if let Some(norm_link) = normalize_in_scope_url(spec, link) {
                if hub_propagates {
                    let child_key = DocNode::derive_resource_key(None, &norm_link);
                    if let Some(child) = inventory.get_mut(&child_key)
                        && child.state == ResourceLifecycleState::Active
                        && child.next_due_at_ms > obs_time_ms.saturating_add(min_interval_ms)
                    {
                        let remaining = child.next_due_at_ms.saturating_sub(obs_time_ms);
                        let pulled = obs_time_ms.saturating_add(min_interval_ms.max(remaining / 4));
                        if pulled < child.next_due_at_ms {
                            child.next_due_at_ms = pulled;
                            stats.co_changed_triggered += 1;
                        }
                    }
                }
                insert_discovered_resource(
                    inventory,
                    norm_link,
                    Some(cand.record.url.clone()),
                    next_depth,
                    1.0,
                    obs_time_ms,
                );
            }
        }
    }
    Ok(())
}
