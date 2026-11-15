//! Candidate failure reconciliation and storage staging helpers for sync ticks.

use pagesift_graph::VersionRecord;
use pagesift_store::{VersionBundleRequest, VersionCommitRequest};

use super::super::inventory::new_discovered_record;
use super::*;

#[allow(clippy::too_many_arguments)]
pub(in crate::sync) fn reconcile_candidate_failure(
    spec: &DataSourceSpec,
    cand: &ScheduledCandidate,
    strategy_used: AcquisitionStrategy,
    browser_attempted: bool,
    step_cost_ms: f64,
    obs_time_ms: u64,
    error_msg: &str,
    inventory: &mut BTreeMap<String, ResourceRecord>,
    experience: &FamilyExperienceTable,
    graph_store: &GraphStore<'_>,
    prepared_bundles: &mut Vec<PreparedVersionBundle>,
    stats: &mut SyncTickStats,
) -> Result<(), ApiError> {
    stats.transient_failures += 1;
    let fam_name = family(&cand.record.url);
    let is_barrier = error_msg.to_ascii_lowercase().contains("barrier");
    experience.record_observation(
        &fam_name,
        &FamilyObservation {
            observed_at_ms: obs_time_ms,
            http_sufficient: (strategy_used != AcquisitionStrategy::DirectBrowser && !is_barrier)
                .then_some(false),
            browser_gain: browser_attempted.then_some(false),
            conditional_hit: None,
            embedded_json_observed: None,
            substantive_change: None, // Censored on error (C16)
            yielded_claims_or_links: None,
            encountered_barrier: is_barrier,
            cost_ms: step_cost_ms,
        },
    );

    let fam_stats = experience.get_family(&fam_name);
    let contract_id = spec.contract_id();
    let rec_key = cand.record.resource_key.clone();
    if let Some(rec_mut) = inventory.get_mut(&rec_key) {
        apply_observation_to_record(
            spec,
            rec_mut,
            &InventoryObservation {
                now_ms: obs_time_ms,
                status_code: 0,
                not_modified_304: false,
                is_barrier_or_error: is_barrier,
                had_transport_error: true,
                final_url: rec_mut.url.clone(),
                canonical_url: None,
                permanent_redirect_target: None,
                etag: None,
                last_modified: None,
                contract_id: contract_id.clone(),
                capture_sufficient: false,
                has_embedded_json: false,
                text_signature: None,
                content_hash: None,
                doc_id: None,
                title: None,
                claim_count: 0,
                substantive_changed: false,
                strategy_used: strategy_used.as_str().to_string(),
            },
        );
        rec_mut.next_due_at_ms =
            compute_next_due_at_ms(spec, rec_mut, fam_stats.as_ref(), obs_time_ms, false);
    }

    let obs_outcome = ObservationOutcome::TransientFetchError {
        error: error_msg.to_string(),
    };
    stage_bundle_for_candidate(
        graph_store,
        prepared_bundles,
        &rec_key,
        &contract_id,
        &obs_outcome,
        None,
        None,
        &Document::default(),
        obs_time_ms,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn stage_bundle_for_candidate(
    graph_store: &GraphStore<'_>,
    prepared_bundles: &mut Vec<PreparedVersionBundle>,
    rec_key: &str,
    contract_id: &str,
    outcome: &ObservationOutcome,
    slice: Option<&pagesift_graph::GraphSlice>,
    staged: Option<&StagedGraphCommit>,
    doc: &Document,
    obs_time_ms: u64,
) -> Result<(), ApiError> {
    let authoritative = crate::graph::stage::is_authoritative_outcome(outcome);
    let mut bundle_req = VersionBundleRequest {
        version: None,
        url_or_key: rec_key,
        contract_id,
        observation_outcome: outcome,
        claims: &[],
        observed_at_ms: obs_time_ms,
        doc_id: None,
        doc_node: None,
        provenance: &[],
    };

    if authoritative && let (Some(slice), Some(st)) = (slice, staged) {
        let md_str = st
            .markdown
            .as_deref()
            .or(doc.markdown.as_deref())
            .unwrap_or_default();
        let curr_record = VersionRecord {
            resource_key: st.node.resource_key.clone(),
            doc_id: st.node.doc_id.clone(),
            url: st.node.final_url.clone(),
            canonical_url: st.node.canonical_url.clone(),
            observed_at_ms: st.node.observed_at_ms,
            content_hash: st.node.content_hash.clone(),
            text_hash: st.node.text_hash.clone(),
            status_code: st.node.status_code,
            etag: st.node.etag.clone(),
            last_modified: st.node.last_modified.clone(),
        };
        let (snapshot_bytes, snapshot_unavail) = if st.not_modified {
            if let Some(b304) = st.matched_304_md.as_deref() {
                (Some(b304.as_bytes()), None)
            } else {
                (
                    None,
                    Some(
                        st.not_modified_unavail_reason
                            .as_deref()
                            .unwrap_or("validator_mismatch"),
                    ),
                )
            }
        } else if !md_str.is_empty() {
            (Some(md_str.as_bytes()), None)
        } else {
            (None, Some("empty_markdown"))
        };
        bundle_req.version = Some(VersionCommitRequest {
            expected_prev_doc_id: st.previous_version.as_ref().map(|p| p.doc_id.as_str()),
            alias_keys: &st.alias_keys,
            record: &curr_record,
            edge: slice.version.as_ref(),
            snapshot_bytes,
            snapshot_unavailable_reason: snapshot_unavail,
        });
        bundle_req.claims = &slice.claims;
        bundle_req.observed_at_ms = slice.observed_at_ms;
        bundle_req.doc_id = Some(&slice.doc_id);
        bundle_req.doc_node = Some(&st.node);
        bundle_req.provenance = &slice.provenance;
        let prep = graph_store
            .prepare_version_bundle(&bundle_req)
            .map_err(|e| {
                pagesift_api::ApiError::new(
                    pagesift_api::ErrorCode::Internal,
                    format!("prepare bundle: {e}"),
                )
            })?;
        prepared_bundles.push(prep);
    } else {
        let prep = graph_store
            .prepare_version_bundle(&bundle_req)
            .map_err(|e| {
                pagesift_api::ApiError::new(
                    pagesift_api::ErrorCode::Internal,
                    format!("prepare bundle: {e}"),
                )
            })?;
        prepared_bundles.push(prep);
    }
    Ok(())
}

pub(super) fn insert_discovered_resource(
    inventory: &mut BTreeMap<String, ResourceRecord>,
    norm_url: String,
    parent_url: Option<String>,
    depth: usize,
    importance_weight: f64,
    now_ms: u64,
) {
    let key = DocNode::derive_resource_key(None, &norm_url);
    inventory.entry(key).or_insert_with(|| {
        new_discovered_record(
            norm_url,
            parent_url,
            depth,
            false,
            importance_weight,
            now_ms,
        )
    });
}
