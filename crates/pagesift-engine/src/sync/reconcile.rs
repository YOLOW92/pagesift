//! 单页同步采集结果的证据与清单状态调和（Step 4 & Step 5）。

use std::collections::BTreeMap;

use pagesift_api::types::Document;
use pagesift_graph::{ClaimTransition, DocNode, ObservationOutcome, ProvenanceEdge, RedirectKind};
use pagesift_plan::experience::{FamilyExperienceTable, FamilyObservation};
use pagesift_plan::render::family;
use pagesift_plan::strategy::AcquisitionStrategy;
use pagesift_store::{
    DataSourceSpec, GraphStore, ResourceChangeEntry, ResourceLifecycleState, ResourceRecord,
    SyncTickStats,
};

use super::inventory::{
    InventoryObservation, apply_observation_to_record, classify_resource_role,
    normalize_in_scope_url,
};
use super::scheduler::{ScheduledCandidate, compute_next_due_at_ms};

pub(super) fn permanent_redirect_target_from_provenance(
    provenance: &[ProvenanceEdge],
    final_url: &str,
) -> Option<String> {
    let has_perm = provenance.iter().any(|edge| {
        matches!(
            edge,
            ProvenanceEdge::Redirect {
                redirect_kind: RedirectKind::HttpPermanent,
                ..
            }
        )
    });
    has_perm.then(|| final_url.to_string())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn reconcile_candidate_success(
    spec: &DataSourceSpec,
    cand: &ScheduledCandidate,
    doc: &Document,
    strategy_used: AcquisitionStrategy,
    browser_gain_obs: Option<bool>,
    step_cost_ms: f64,
    obs_time_ms: u64,
    prev_state: Option<ResourceLifecycleState>,
    prev_doc_id: Option<String>,
    inventory: &mut BTreeMap<String, ResourceRecord>,
    experience: &FamilyExperienceTable,
    graph_store: &GraphStore<'_>,
    stats: &mut SyncTickStats,
    added_resources: &mut Vec<ResourceChangeEntry>,
    modified_resources: &mut Vec<ResourceChangeEntry>,
    migrated_resources: &mut Vec<ResourceChangeEntry>,
    disappeared_resources: &mut Vec<ResourceChangeEntry>,
    claim_transitions: &mut Vec<ClaimTransition>,
) {
    let rec_key = cand.record.resource_key.clone();
    let status_code = doc.metadata.status_code;
    let not_modified_304 = doc
        .metadata
        .pagesift
        .as_ref()
        .is_some_and(|p| p.not_modified);
    if not_modified_304 {
        stats.conditional_304_hits += 1;
    }
    let is_barrier = matches!(status_code, 403 | 429);
    if is_barrier {
        stats.access_restricted_count += 1;
    } else if status_code >= 500 {
        stats.transient_failures += 1;
    }

    let slice = doc.graph.as_ref();
    let (sub_blocks, noise_blocks) = slice
        .and_then(|g| g.version.as_ref())
        .and_then(|v| v.diff_stats.as_ref())
        .map_or((0, 0), |d| (d.substantive, d.noise));

    let page_transitions = slice
        .map(|g| g.claim_transitions.clone())
        .unwrap_or_default();
    let claim_count = slice.map_or(0, |g| g.claims.len());
    let substantive_changed = !not_modified_304
        && status_code < 400
        && (sub_blocks > 0 || !page_transitions.is_empty() || prev_doc_id.is_none());
    let noise_only =
        !not_modified_304 && status_code < 400 && !substantive_changed && noise_blocks > 0;

    if !not_modified_304 && status_code < 400 {
        if substantive_changed && prev_doc_id.is_some() {
            stats.substantive_updates += 1;
        } else if noise_only {
            stats.noise_only_updates += 1;
        } else if prev_doc_id.is_some() {
            stats.unchanged_200_hits += 1;
        }
    }

    claim_transitions.extend(page_transitions);

    let fam_name = family(&cand.record.url);
    let dt_secs = cand
        .record
        .last_checked_at_ms
        .map(|last| (obs_time_ms.saturating_sub(last) as f64 / 1000.0).max(1.0));
    let has_links = doc.links.as_ref().is_some_and(|l| !l.is_empty());
    experience.record_observation(
        &fam_name,
        &FamilyObservation {
            observed_at_ms: obs_time_ms,
            http_sufficient: browser_gain_obs != Some(true) && status_code < 400,
            browser_gain: browser_gain_obs,
            conditional_hit: (strategy_used == AcquisitionStrategy::ConditionalHttp)
                .then_some(not_modified_304),
            substantive_change: dt_secs.map(|dt| (dt, substantive_changed)),
            yielded_claims_or_links: claim_count > 0 || has_links,
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
    let perm_target =
        slice.and_then(|g| permanent_redirect_target_from_provenance(&g.provenance, &final_url));
    let doc_id = slice.map(|g| g.doc_id.clone());
    let latest_ver = graph_store
        .latest_version_for_urls(&[rec_key.as_str(), final_url.as_str()])
        .ok()
        .flatten();

    let mut migrated_to_url = None;
    let mut current_state = ResourceLifecycleState::Active;
    if let Some(rec_mut) = inventory.get_mut(&rec_key) {
        let next_due = compute_next_due_at_ms(
            spec,
            rec_mut,
            fam_stats.as_ref(),
            obs_time_ms,
            substantive_changed,
        );
        migrated_to_url = apply_observation_to_record(
            spec,
            rec_mut,
            &InventoryObservation {
                now_ms: obs_time_ms,
                status_code,
                not_modified_304,
                is_barrier_or_error: is_barrier,
                final_url,
                canonical_url: doc.metadata.canonical_url.clone(),
                permanent_redirect_target: perm_target,
                etag: latest_ver.as_ref().and_then(|v| v.etag.clone()),
                last_modified: latest_ver.as_ref().and_then(|v| v.last_modified.clone()),
                content_hash: latest_ver.as_ref().and_then(|v| v.content_hash.clone()),
                doc_id: doc_id.clone(),
                title: doc.metadata.title.clone(),
                claim_count,
                substantive_changed,
                strategy_used: strategy_used.as_str().to_string(),
                next_due_at_ms: next_due,
            },
        );
        current_state = rec_mut.state;
    }

    if current_state == ResourceLifecycleState::ConfirmedGone
        && prev_state != Some(ResourceLifecycleState::ConfirmedGone)
        && let Ok((_, gone_transitions)) = graph_store.reconcile_and_store_resource_claims(
            &rec_key,
            &ObservationOutcome::ConfirmedGone {
                reason: format!("HTTP {status_code}"),
            },
            &[],
            obs_time_ms,
        )
    {
        claim_transitions.extend(gone_transitions);
    }

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
        not_modified_304,
    };

    match current_state {
        ResourceLifecycleState::Migrated => migrated_resources.push(change_entry),
        ResourceLifecycleState::SuspectedGone | ResourceLifecycleState::ConfirmedGone => {
            disappeared_resources.push(change_entry);
        }
        ResourceLifecycleState::Active => {
            if prev_state == Some(ResourceLifecycleState::Discovered) || prev_doc_id.is_none() {
                added_resources.push(change_entry);
            } else if substantive_changed {
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

    if cand.record.depth < spec.max_depth && status_code < 400 {
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
}

fn insert_discovered_resource(
    inventory: &mut BTreeMap<String, ResourceRecord>,
    norm_url: String,
    parent_url: Option<String>,
    depth: usize,
    importance_weight: f64,
    now_ms: u64,
) {
    let key = DocNode::derive_resource_key(None, &norm_url);
    inventory
        .entry(key.clone())
        .or_insert_with(|| ResourceRecord {
            resource_key: key,
            url: norm_url.clone(),
            canonical_url: None,
            migrated_to: None,
            parent_url,
            depth,
            role: classify_resource_role(&norm_url, depth, false),
            state: ResourceLifecycleState::Discovered,
            discovered_at_ms: now_ms,
            last_checked_at_ms: None,
            last_substantive_change_at_ms: None,
            next_due_at_ms: now_ms,
            etag: None,
            last_modified: None,
            content_hash: None,
            latest_doc_id: None,
            last_status_code: None,
            consecutive_failures: 0,
            consecutive_404s: 0,
            importance_weight,
            last_strategy: None,
            title: None,
            claim_count: 0,
        });
}
