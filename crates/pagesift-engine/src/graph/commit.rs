//! Version CAS commit loop and claim lifecycle reconciliation against `GraphStore`.

use std::collections::BTreeSet;

use pagesift_api::types::Document;
use pagesift_graph::{
    ClaimRecord, ClaimTransition, DEFAULT_CONTRACT_ID, DocNode, GraphOptions, GraphSlice,
    ObservationOutcome, PageGraphDelta, ProvenanceEdge, VersionDiffStats, VersionEdge,
    VersionRecord,
};
use pagesift_store::{
    GraphStore, Store, VersionBundleOutcome, VersionBundleRequest, VersionCommitOutcome,
    VersionCommitRequest,
};

use super::stage::{StagedGraphCommit, is_authoritative_outcome, observation_outcome_for_doc};
use super::version_diff::{
    compute_version_diff, load_prev_snapshot_for_version, remap_delta_doc_id, remap_slice_doc_id,
};
use super::{lock_resource_stripe, now_ms, push_doc_warning};

const MAX_VERSION_COMMIT_RETRIES: usize = 4; // [策略] 并发版本提交 CAS 重试上限

fn outcome_from_status(status_code: u16, not_modified: bool) -> ObservationOutcome {
    if not_modified {
        ObservationOutcome::NotModified304
    } else {
        match status_code {
            404 => ObservationOutcome::SuspectedGone {
                status_code: Some(404),
            },
            410 => ObservationOutcome::ConfirmedGone {
                reason: "HTTP 410 Gone".into(),
            },
            s if s >= 400 => ObservationOutcome::TransientFetchError {
                error: format!("HTTP {s}"),
            },
            _ => ObservationOutcome::VerifiedContent,
        }
    }
}

fn resolve_snapshot_payload<'a>(
    markdown: &'a str,
    not_modified: bool,
    matched_304_md: Option<&'a str>,
    not_modified_unavail_reason: Option<&'a str>,
) -> (Option<&'a [u8]>, Option<&'a str>) {
    if !markdown.is_empty() {
        (Some(markdown.as_bytes()), None)
    } else if not_modified {
        if let Some(body_304) = matched_304_md {
            (Some(body_304.as_bytes()), None)
        } else {
            (
                None,
                Some(not_modified_unavail_reason.unwrap_or("validator_mismatch")),
            )
        }
    } else {
        (None, Some("empty_markdown"))
    }
}

enum CasTarget<'a> {
    Delta(&'a mut PageGraphDelta),
    Slice {
        slice: &'a mut GraphSlice,
        node: &'a mut DocNode,
    },
}

impl<'a> CasTarget<'a> {
    fn node(&self) -> &DocNode {
        match self {
            Self::Delta(d) => &d.node,
            Self::Slice { node, .. } => node,
        }
    }

    fn claims(&self) -> &[ClaimRecord] {
        match self {
            Self::Delta(d) => &d.claims,
            Self::Slice { slice, .. } => &slice.claims,
        }
    }

    fn provenance(&self) -> &[ProvenanceEdge] {
        match self {
            Self::Delta(d) => &d.provenance,
            Self::Slice { slice, .. } => &slice.provenance,
        }
    }

    fn version(&self) -> Option<&VersionEdge> {
        match self {
            Self::Delta(d) => d.version.as_ref(),
            Self::Slice { slice, .. } => slice.version.as_ref(),
        }
    }

    fn reject_version(&mut self) {
        match self {
            Self::Delta(d) => {
                d.version = None;
                d.version_rejected = true;
            }
            Self::Slice { slice, .. } => {
                slice.version = None;
                slice.version_rejected = true;
            }
        }
    }

    fn clear_version(&mut self) {
        match self {
            Self::Delta(d) => d.version = None,
            Self::Slice { slice, .. } => slice.version = None,
        }
    }

    fn set_transitions(&mut self, transitions: Vec<ClaimTransition>) {
        match self {
            Self::Delta(d) => d.claim_transitions = transitions,
            Self::Slice { slice, .. } => slice.claim_transitions = transitions,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn remap(
        &mut self,
        new_doc_id: String,
        observed_at_ms: u64,
        prev_version: Option<&VersionRecord>,
        diff_stats: Option<VersionDiffStats>,
        changed_block_ids: Vec<String>,
        diff_unavailable_reason: Option<String>,
    ) {
        match self {
            Self::Delta(d) => remap_delta_doc_id(
                d,
                new_doc_id,
                observed_at_ms,
                prev_version,
                diff_stats,
                changed_block_ids,
                diff_unavailable_reason,
            ),
            Self::Slice { slice, node } => remap_slice_doc_id(
                slice,
                node,
                new_doc_id,
                observed_at_ms,
                prev_version,
                diff_stats,
                changed_block_ids,
                diff_unavailable_reason,
            ),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_cas_commit_loop(
    gs: &GraphStore<'_>,
    opts: &GraphOptions,
    mut target: CasTarget<'_>,
    alias_keys: &[String],
    markdown: &str,
    not_modified: bool,
    matched_304_version: Option<&VersionRecord>,
    matched_304_md: Option<&str>,
    not_modified_unavail_reason: Option<&str>,
    seed_content_hash: Option<&str>,
    seed_raw_hash: Option<&str>,
    chunk_spans: &[(usize, usize, usize)],
    mut previous_version: Option<VersionRecord>,
    outcome: &ObservationOutcome,
    had_initial_prev: bool,
    warnings_to_push: &mut Vec<String>,
) {
    if !is_authoritative_outcome(outcome) {
        // Non-authoritative error/gone/barrier payloads must never overwrite healthy versions (C02).
        target.clear_version();
        let node = target.node().clone();
        if (opts.facts || opts.conflicts)
            && let Ok((_, transitions)) = gs.reconcile_and_store_resource_claims(
                &node.resource_key,
                outcome,
                &[],
                node.observed_at_ms,
            )
            && (had_initial_prev || !transitions.is_empty())
        {
            target.set_transitions(transitions);
        }
        return;
    }

    if !opts.versions {
        let node = target.node().clone();
        if (opts.facts || opts.conflicts)
            && let Ok((_, transitions)) = gs.reconcile_and_store_resource_claims(
                &node.resource_key,
                outcome,
                target.claims(),
                node.observed_at_ms,
            )
            && (had_initial_prev || !transitions.is_empty())
        {
            target.set_transitions(transitions);
        }
        return;
    }

    let mut prev_md_loaded: Option<String>;
    let mut diff_unavailable_reason: Option<String>;

    for attempt in 0..MAX_VERSION_COMMIT_RETRIES {
        let node = target.node();
        let curr_record = VersionRecord {
            resource_key: node.resource_key.clone(),
            doc_id: node.doc_id.clone(),
            url: node.final_url.clone(),
            canonical_url: node.canonical_url.clone(),
            observed_at_ms: node.observed_at_ms,
            content_hash: node.content_hash.clone(),
            text_hash: node.text_hash.clone(),
            status_code: node.status_code,
            etag: node.etag.clone(),
            last_modified: node.last_modified.clone(),
        };
        let (snapshot_bytes, snapshot_unavail) = resolve_snapshot_payload(
            markdown,
            not_modified,
            matched_304_md,
            not_modified_unavail_reason,
        );
        let ver_req = VersionCommitRequest {
            expected_prev_doc_id: previous_version.as_ref().map(|p| p.doc_id.as_str()),
            alias_keys,
            record: &curr_record,
            edge: target.version(),
            snapshot_bytes,
            snapshot_unavailable_reason: snapshot_unavail,
        };

        let stale = if opts.facts || opts.conflicts {
            let had_prev = had_initial_prev || previous_version.is_some();
            let bundle_req = VersionBundleRequest {
                version: Some(ver_req),
                url_or_key: &node.resource_key,
                contract_id: DEFAULT_CONTRACT_ID,
                observation_outcome: outcome,
                claims: target.claims(),
                observed_at_ms: node.observed_at_ms,
                doc_id: Some(&node.doc_id),
                doc_node: Some(node),
                provenance: target.provenance(),
            };
            match gs.commit_version_bundle(&bundle_req) {
                Ok(VersionBundleOutcome::Committed {
                    snapshot_warning,
                    claim_transitions,
                    ..
                }) => {
                    if let Some(w) = snapshot_warning {
                        warnings_to_push.push(w);
                    }
                    if had_prev {
                        target.set_transitions(claim_transitions);
                    }
                    return;
                }
                Ok(VersionBundleOutcome::StaleParent { current_latest }) => Some(current_latest),
                Err(e) => {
                    warnings_to_push.push(format!("graph_version_persist_failed: {e}"));
                    target.reject_version();
                    return;
                }
            }
        } else {
            match gs.commit_version(ver_req) {
                Ok(VersionCommitOutcome::Committed {
                    snapshot_warning, ..
                }) => {
                    if let Some(w) = snapshot_warning {
                        warnings_to_push.push(w);
                    }
                    return;
                }
                Ok(VersionCommitOutcome::StaleParent { current_latest }) => Some(current_latest),
                Err(e) => {
                    warnings_to_push.push(format!("graph_version_persist_failed: {e}"));
                    target.reject_version();
                    return;
                }
            }
        };

        if attempt + 1 >= MAX_VERSION_COMMIT_RETRIES {
            warnings_to_push.push(
                "graph_version_commit_conflict: concurrent commits exhausted retries".to_string(),
            );
            target.reject_version();
            return;
        }
        previous_version = stale;
        warnings_to_push.clear();
        diff_unavailable_reason = None;
        let mut observed_at_ms = now_ms();
        if let Some(prev) = &previous_version {
            if observed_at_ms <= prev.observed_at_ms {
                observed_at_ms = prev.observed_at_ms + 1;
            }
            let (prev_md, unavail_reason, unavail_warn) = load_prev_snapshot_for_version(gs, prev);
            prev_md_loaded = prev_md;
            if !not_modified {
                diff_unavailable_reason = unavail_reason;
                if let Some(w) = unavail_warn {
                    warnings_to_push.push(w);
                }
            }
        } else {
            prev_md_loaded = None;
        }
        let node = target.node();
        let new_doc_id = DocNode::compute_observation_doc_id(
            node.canonical_url.as_deref(),
            &node.final_url,
            observed_at_ms,
            seed_content_hash,
            seed_raw_hash,
        );
        let (stats, changed) = if let Some(ref prev) = previous_version {
            if not_modified && markdown.is_empty() {
                if let Some(body_304) = matched_304_md {
                    let same_as_prev =
                        matched_304_version.is_some_and(|mv| mv.doc_id == prev.doc_id);
                    if same_as_prev {
                        (Some(VersionDiffStats::default()), Vec::new())
                    } else if prev_md_loaded.is_some() {
                        compute_version_diff(
                            prev_md_loaded.as_deref(),
                            body_304,
                            false,
                            &new_doc_id,
                            chunk_spans,
                            opts,
                        )
                    } else {
                        diff_unavailable_reason = Some("previous_snapshot_unavailable".to_string());
                        (None, Vec::new())
                    }
                } else {
                    let reason = not_modified_unavail_reason
                        .unwrap_or("validator_mismatch")
                        .to_string();
                    warnings_to_push.push(format!("graph_version_diff_unavailable: {reason}"));
                    diff_unavailable_reason = Some(reason);
                    (None, Vec::new())
                }
            } else {
                compute_version_diff(
                    prev_md_loaded.as_deref(),
                    markdown,
                    false,
                    &new_doc_id,
                    chunk_spans,
                    opts,
                )
            }
        } else {
            (None, Vec::new())
        };
        target.remap(
            new_doc_id,
            observed_at_ms,
            previous_version.as_ref(),
            stats,
            changed,
            diff_unavailable_reason,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn commit_and_reconcile_version(
    gs: &GraphStore<'_>,
    opts: &GraphOptions,
    delta: &mut PageGraphDelta,
    resource_key: &str,
    alias_candidates: &BTreeSet<String>,
    _canonical_url: Option<&str>,
    _final_url: &str,
    markdown: &str,
    not_modified: bool,
    matched_304_version: Option<&VersionRecord>,
    matched_304_md: Option<&str>,
    not_modified_unavail_reason: Option<&str>,
    seed_content_hash: Option<&str>,
    seed_raw_hash: Option<&str>,
    chunk_spans: &[(usize, usize, usize)],
    previous_version: Option<VersionRecord>,
    warnings_to_push: &mut Vec<String>,
) {
    let outcome = outcome_from_status(delta.node.status_code, not_modified);
    let had_initial_prev = previous_version.is_some();
    let alias_keys: Vec<String> = alias_candidates
        .iter()
        .filter(|a| *a != resource_key)
        .cloned()
        .collect();
    execute_cas_commit_loop(
        gs,
        opts,
        CasTarget::Delta(delta),
        &alias_keys,
        markdown,
        not_modified,
        matched_304_version,
        matched_304_md,
        not_modified_unavail_reason,
        seed_content_hash,
        seed_raw_hash,
        chunk_spans,
        previous_version,
        &outcome,
        had_initial_prev,
        warnings_to_push,
    );
}

/// Persist the winning non-speculative document graph to `GraphStore` in one `AtomicTx` (C01 / C02 / C06).
pub fn commit_staged_document(
    store: &Store,
    opts: &GraphOptions,
    tenant: Option<&str>,
    doc: &mut Document,
    mut staged: StagedGraphCommit,
) {
    if !opts.persist {
        return;
    }
    let Some(outcome) = doc
        .graph
        .as_ref()
        .map(|slice| observation_outcome_for_doc(doc, slice))
    else {
        return;
    };
    let Some(slice) = doc.graph.as_mut() else {
        return;
    };
    let _stripe_guard = lock_resource_stripe(tenant, &slice.resource_key);
    let gs = store.graph_for(tenant);
    let markdown = staged
        .markdown
        .as_deref()
        .or(doc.markdown.as_deref())
        .unwrap_or_default()
        .to_string();
    let previous_version = staged.previous_version.take();
    let mut warnings_to_push = Vec::new();
    execute_cas_commit_loop(
        &gs,
        opts,
        CasTarget::Slice {
            slice,
            node: &mut staged.node,
        },
        &staged.alias_keys,
        &markdown,
        staged.not_modified,
        staged.matched_304_version.as_ref(),
        staged.matched_304_md.as_deref(),
        staged.not_modified_unavail_reason.as_deref(),
        staged.seed_content_hash.as_deref(),
        staged.seed_raw_hash.as_deref(),
        &staged.chunk_spans,
        previous_version,
        &outcome,
        staged.had_initial_prev,
        &mut warnings_to_push,
    );
    for w in warnings_to_push {
        push_doc_warning(doc, w);
    }
}
