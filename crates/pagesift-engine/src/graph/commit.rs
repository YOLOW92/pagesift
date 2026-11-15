//! Version CAS commit loop and claim lifecycle reconciliation against `GraphStore`.

use std::collections::BTreeSet;

use pagesift_graph::{
    DocNode, GraphOptions, ObservationOutcome, PageGraphDelta, VersionDiffStats, VersionRecord,
};
use pagesift_store::{GraphStore, VersionCommitOutcome, VersionCommitRequest};

use super::now_ms;
use super::version_diff::{
    compute_version_diff, load_prev_snapshot_for_version, remap_delta_doc_id,
};

const MAX_VERSION_COMMIT_RETRIES: usize = 4; // [策略] 并发版本提交 CAS 重试上限

#[allow(clippy::too_many_arguments)]
pub(super) fn commit_and_reconcile_version(
    gs: &GraphStore<'_>,
    opts: &GraphOptions,
    delta: &mut PageGraphDelta,
    resource_key: &str,
    alias_candidates: &BTreeSet<String>,
    canonical_url: Option<&str>,
    final_url: &str,
    markdown: &str,
    not_modified: bool,
    matched_304_version: Option<&VersionRecord>,
    matched_304_md: Option<&str>,
    not_modified_unavail_reason: Option<&str>,
    seed_content_hash: Option<&str>,
    seed_raw_hash: Option<&str>,
    chunk_spans: &[(usize, usize, usize)],
    mut previous_version: Option<VersionRecord>,
    warnings_to_push: &mut Vec<String>,
) {
    let had_initial_prev = previous_version.is_some();
    let mut prev_md_loaded: Option<String>;
    let mut diff_unavailable_reason: Option<String>;

    for attempt in 0..MAX_VERSION_COMMIT_RETRIES {
        let alias_keys: Vec<String> = alias_candidates
            .iter()
            .filter(|a| *a != resource_key)
            .cloned()
            .collect();

        let curr_record = VersionRecord {
            resource_key: delta.node.resource_key.clone(),
            doc_id: delta.node.doc_id.clone(),
            url: delta.node.final_url.clone(),
            canonical_url: delta.node.canonical_url.clone(),
            observed_at_ms: delta.node.observed_at_ms,
            content_hash: delta.node.content_hash.clone(),
            text_hash: delta.node.text_hash.clone(),
            status_code: delta.node.status_code,
            etag: delta.node.etag.clone(),
            last_modified: delta.node.last_modified.clone(),
        };

        let (snapshot_bytes, snapshot_unavail): (Option<&[u8]>, Option<&str>) =
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
            };

        match gs.commit_version(VersionCommitRequest {
            expected_prev_doc_id: previous_version.as_ref().map(|p| p.doc_id.as_str()),
            alias_keys: &alias_keys,
            record: &curr_record,
            edge: delta.version.as_ref(),
            snapshot_bytes,
            snapshot_unavailable_reason: snapshot_unavail,
        }) {
            Ok(VersionCommitOutcome::Committed {
                snapshot_warning, ..
            }) => {
                if let Some(w) = snapshot_warning {
                    warnings_to_push.push(w);
                }
                break;
            }
            Ok(VersionCommitOutcome::StaleParent { current_latest }) => {
                if attempt + 1 >= MAX_VERSION_COMMIT_RETRIES {
                    warnings_to_push.push(
                        "graph_version_commit_conflict: concurrent commits exhausted retries"
                            .to_string(),
                    );
                    delta.version = None;
                    delta.version_rejected = true;
                    break;
                }
                warnings_to_push.clear();
                previous_version = Some(current_latest);
                diff_unavailable_reason = None;
                let mut observed_at_ms = now_ms();
                if let Some(prev) = &previous_version {
                    if observed_at_ms <= prev.observed_at_ms {
                        observed_at_ms = prev.observed_at_ms + 1;
                    }
                    let (prev_md, unavail_reason, unavail_warn) =
                        load_prev_snapshot_for_version(gs, prev);
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
                let new_doc_id = DocNode::compute_observation_doc_id(
                    canonical_url,
                    final_url,
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
                                diff_unavailable_reason =
                                    Some("previous_snapshot_unavailable".to_string());
                                (None, Vec::new())
                            }
                        } else {
                            let reason = not_modified_unavail_reason
                                .unwrap_or("validator_mismatch")
                                .to_string();
                            warnings_to_push
                                .push(format!("graph_version_diff_unavailable: {reason}"));
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
                remap_delta_doc_id(
                    delta,
                    new_doc_id,
                    observed_at_ms,
                    previous_version.as_ref(),
                    stats,
                    changed,
                    diff_unavailable_reason.clone(),
                );
            }
            Err(e) => {
                warnings_to_push.push(format!("graph_version_persist_failed: {e}"));
                delta.version = None;
                delta.version_rejected = true;
                break;
            }
        }
    }

    if !delta.version_rejected && (opts.facts || opts.conflicts) {
        let outcome = if not_modified {
            ObservationOutcome::NotModified304
        } else {
            match delta.node.status_code {
                404 => ObservationOutcome::SuspectedGone {
                    status_code: Some(404),
                },
                410 => ObservationOutcome::ConfirmedGone {
                    reason: "HTTP 410 Gone".into(),
                },
                429 | 500..=599 => ObservationOutcome::TransientFetchError {
                    error: format!("HTTP {}", delta.node.status_code),
                },
                _ => ObservationOutcome::VerifiedContent,
            }
        };
        let had_prev = had_initial_prev || previous_version.is_some();
        if let Ok((_, transitions)) = gs.reconcile_and_store_resource_claims(
            &delta.node.resource_key,
            &outcome,
            &delta.claims,
            delta.node.observed_at_ms,
        ) && had_prev
        {
            delta.claim_transitions = transitions;
        }
    }
}
