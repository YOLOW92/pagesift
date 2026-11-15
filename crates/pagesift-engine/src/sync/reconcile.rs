//! 单页同步采集结果的证据与清单状态调和（Step 4 & Step 5）。

use std::collections::BTreeMap;

use pagesift_api::ApiError;
use pagesift_api::types::Document;
use pagesift_graph::{
    ClaimTransitionKind, DocNode, ObservationOutcome, ProvenanceEdge, RedirectKind,
};
use pagesift_plan::experience::{FamilyExperienceTable, FamilyObservation};
use pagesift_plan::render::family;
use pagesift_plan::strategy::{AcquisitionStrategy, CaptureAssessment};
use pagesift_store::{
    DataSourceSpec, GraphStore, PreparedVersionBundle, ResourceChangeEntry, ResourceLifecycleState,
    ResourceRecord, SyncTickStats,
};

use super::capture::{has_embedded_json_signal, is_barrier_doc, sync_learner_into_experience};
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

#[allow(clippy::too_many_arguments)]
pub(super) fn reconcile_candidate_success(
    spec: &DataSourceSpec,
    cand: &ScheduledCandidate,
    doc: &Document,
    staged: Option<StagedGraphCommit>,
    strategy_used: AcquisitionStrategy,
    http_assessment: Option<&CaptureAssessment>,
    final_assessment: &CaptureAssessment,
    browser_gain_obs: Option<bool>,
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
    let not_modified_304 = doc
        .metadata
        .pagesift
        .as_ref()
        .is_some_and(|p| p.not_modified);
    let is_barrier = is_barrier_doc(doc);
    let http_ok = !is_barrier && status_code < 400;
    let capture_sufficient = final_assessment.sufficient;

    if not_modified_304 && capture_sufficient {
        stats.conditional_304_hits += 1;
    }
    if is_barrier {
        stats.access_restricted_count += 1;
    } else if status_code >= 500 {
        stats.transient_failures += 1;
    } else if !not_modified_304 && status_code < 400 && !capture_sufficient {
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
    let substantive_changed = if not_modified_304 {
        capture_sufficient && sub_blocks > 0
    } else if http_ok && capture_sufficient {
        prev_doc_id.is_none() || sub_blocks > 0 || non_confirm_transition
    } else {
        false
    };
    let noise_only = !not_modified_304
        && http_ok
        && capture_sufficient
        && !substantive_changed
        && noise_blocks > 0;

    if !not_modified_304 && http_ok && capture_sufficient {
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
        .last_checked_at_ms
        .map(|last| (obs_time_ms.saturating_sub(last) as f64 / 1000.0).max(1.0));
    let has_links = doc.links.as_ref().is_some_and(|l| !l.is_empty());
    let has_embedded = has_embedded_json_signal(doc);
    let md_str = doc.markdown.as_deref().unwrap_or_default();
    let text_sig = (http_ok && capture_sufficient && !not_modified_304)
        .then(|| Learner::content_signature(md_str));

    if http_ok && capture_sufficient && !not_modified_304 {
        sync_learner_into_experience(learner, experience, &cand.record.url, md_str, obs_time_ms);
    }

    experience.record_observation(
        &fam_name,
        &FamilyObservation {
            observed_at_ms: obs_time_ms,
            http_sufficient: (http_ok && strategy_used != AcquisitionStrategy::DirectBrowser)
                .then(|| http_assessment.is_some_and(|a| a.sufficient)),
            browser_gain: browser_gain_obs,
            conditional_hit: (strategy_used == AcquisitionStrategy::ConditionalHttp && http_ok)
                .then_some(not_modified_304 && capture_sufficient),
            embedded_json_observed: (http_ok
                && strategy_used != AcquisitionStrategy::DirectBrowser)
                .then_some(has_embedded),
            substantive_change: (http_ok && capture_sufficient && prev_doc_id.is_some())
                .then(|| dt_secs.map(|dt| (dt, substantive_changed)))
                .flatten(),
            yielded_claims_or_links: (http_ok && capture_sufficient)
                .then_some(claim_count > 0 || has_links),
            encountered_barrier: is_barrier,
            cost_ms: step_cost_ms,
        },
    );

    let fam_stats = experience.get_family(&fam_name);
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

    let mut migrated_to_url = None;
    let mut current_state = cand.record.state;
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
        // C19: 状态、连续失败与权重迁移完毕后，再基于最新状态计算 next_due_at_ms
        rec_mut.next_due_at_ms = compute_next_due_at_ms(
            spec,
            rec_mut,
            fam_stats.as_ref(),
            obs_time_ms,
            substantive_changed,
        );
        current_state = rec_mut.state;
    }

    let obs_outcome = if not_modified_304 && capture_sufficient {
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
        for link in doc.links.as_deref().unwrap_or(&[]) {
            if let Some(norm_link) = normalize_in_scope_url(spec, link) {
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
