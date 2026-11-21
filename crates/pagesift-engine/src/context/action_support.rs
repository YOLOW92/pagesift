//! Support routines for Context Action: checked `Changes` delta-window streaming,
//! host allowlist matching, and corruption-aware `ContextLearnState` persistence.

use pagesift_api::{ApiError, ErrorCode};
use pagesift_extract::context::ContextLearnState;
use pagesift_graph::context::{
    ActionMode, ContextActionOutcome, ContextActionRequest, CoverageDiagnosis, InformationGap,
    StoppingReason,
};
use pagesift_net::{Url, host_allowed};
use pagesift_store::DeltaWindowBatch;

use crate::engine::Scraper;

// [策略] 单次 changes 最多汇总每个源 64 条增量记录
const CHANGES_BATCH_LIMIT: usize = 64;

pub(crate) fn url_permitted(url: &str, allowed_hosts: &[String]) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none_or(str::is_empty) {
        return false;
    }
    host_allowed(&parsed, allowed_hosts)
}

pub(crate) fn empty_coverage(min_indep: usize, reason: &str) -> CoverageDiagnosis {
    CoverageDiagnosis {
        term_coverage: 0.0,
        required_field_coverage: 0.0,
        independent_sources_observed: 0,
        min_independent_sources_required: min_indep,
        estimable: false,
        good_turing_unseen: None,
        good_turing_ci_high: None,
        capture_recapture_total: None,
        not_estimable_reason: Some(reason.to_owned()),
    }
}

/// Execute `ActionMode::Changes` with explicit retention-window gap, ABA instance replacement,
/// sequence-ahead, and truncation detection (never claims `Sufficient` over a broken delta stream).
pub(crate) async fn execute_changes_mode(
    scraper: &Scraper,
    tenant: Option<&str>,
    req: &ContextActionRequest,
) -> Result<ContextActionOutcome, ApiError> {
    let store = scraper.store_ref().cloned().ok_or_else(|| {
        ApiError::invalid("changes mode requires persistent store configured on Scraper")
    })?;
    let sids = req.pack.search.source_ids.clone();
    let since = req.since_delta_seq;
    let t_owned = tenant.map(str::to_owned);
    let (deltas, gaps, resync_required, truncated_any) = store
        .blocking(
            move |st| -> Result<(Vec<_>, Vec<InformationGap>, bool, bool), ApiError> {
                let ctx_st = st.context_for(t_owned.as_deref());
                let targets = if sids.is_empty() {
                    ctx_st
                        .list_indexed_sources()
                        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
                        .into_iter()
                        .map(|m| m.source_id)
                        .collect()
                } else {
                    sids
                };
                let mut all = Vec::new();
                let mut gaps = Vec::new();
                let mut resync_required = false;
                let mut truncated_any = false;

                for sid in targets {
                    match ctx_st
                        .list_deltas_checked(&sid, since, CHANGES_BATCH_LIMIT)
                        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
                    {
                        DeltaWindowBatch::Missing => {}
                        DeltaWindowBatch::InstanceSuperseded { instance_id } => {
                            resync_required = true;
                            gaps.push(InformationGap {
                                kind: "source_instance_superseded".to_owned(),
                                target: sid,
                                detail: format!(
                                    "source instance '{instance_id}' was deleted or replaced; full resync required"
                                ),
                            });
                        }
                        DeltaWindowBatch::RetentionGap {
                            instance_id,
                            requested_after_seq,
                            earliest_retained_seq,
                            latest_seq,
                        } => {
                            resync_required = true;
                            gaps.push(InformationGap {
                                kind: "delta_retention_gap".to_owned(),
                                target: sid,
                                detail: format!(
                                    "sinceDeltaSeq={requested_after_seq} is behind earliest retained delta {earliest_retained_seq} (latest={latest_seq}, instance={instance_id}); full resync required"
                                ),
                            });
                        }
                        DeltaWindowBatch::SequenceAheadOrReset {
                            instance_id,
                            requested_after_seq,
                            latest_seq,
                        } => {
                            resync_required = true;
                            gaps.push(InformationGap {
                                kind: "delta_sequence_ahead_or_reset".to_owned(),
                                target: sid,
                                detail: format!(
                                    "sinceDeltaSeq={requested_after_seq} exceeds current latest_delta_seq={latest_seq} on instance '{instance_id}'; possible ABA recreate, full resync required"
                                ),
                            });
                        }
                        DeltaWindowBatch::Continuous {
                            latest_seq,
                            truncated,
                            deltas: batch,
                            ..
                        } => {
                            if truncated {
                                truncated_any = true;
                                let last_seq = batch.last().map_or(0, |d| d.delta_seq);
                                gaps.push(InformationGap {
                                    kind: "delta_stream_truncated".to_owned(),
                                    target: sid,
                                    detail: format!(
                                        "returned deltas through seq={last_seq} of latest={latest_seq}; paginate with sinceDeltaSeq={last_seq}"
                                    ),
                                });
                            }
                            all.extend(batch);
                        }
                    }
                }
                Ok((all, gaps, resync_required, truncated_any))
            },
        )
        .await?;

    let (stopping_reason, reason_tag) = if resync_required {
        (StoppingReason::ResyncRequired, "changes_resync_required")
    } else if truncated_any {
        (StoppingReason::BudgetExhausted, "changes_delta_truncated")
    } else {
        (StoppingReason::Sufficient, "changes_delta_stream")
    };

    Ok(ContextActionOutcome {
        mode: ActionMode::Changes,
        stopping_reason,
        reused_existing_context: true,
        coverage: empty_coverage(req.min_independent_sources, reason_tag),
        gaps,
        acquisition_steps: Vec::new(),
        pack: None,
        deltas,
    })
}

/// Read persisted [`ContextLearnState`] without swallowing storage or JSON corruption.
/// Returns `Ok(None)` when no learn state has been saved yet (or no store is configured).
pub async fn try_load_learn_state(
    scraper: &Scraper,
    tenant: Option<&str>,
) -> Result<Option<ContextLearnState>, ApiError> {
    let Some(store) = scraper.store_ref().cloned() else {
        return Ok(None);
    };
    let t = tenant.map(str::to_owned);
    store
        .blocking(move |st| -> Result<Option<ContextLearnState>, ApiError> {
            let raw_opt = st
                .context_for(t.as_deref())
                .load_learn_bytes("default")
                .map_err(|e| {
                    ApiError::new(
                        ErrorCode::Internal,
                        format!("load context learn state failed: {e}"),
                    )
                })?;
            match raw_opt {
                None => Ok(None),
                Some(bytes) => {
                    let parsed =
                        serde_json::from_slice::<ContextLearnState>(&bytes).map_err(|e| {
                            ApiError::new(
                                ErrorCode::Internal,
                                format!("corrupt context learn state JSON: {e}"),
                            )
                        })?;
                    Ok(Some(parsed))
                }
            }
        })
        .await
}

/// Load [`ContextLearnState`] for read-only Search/Pack/Action paths: clean cold-start returns
/// the default state without warning; storage/JSON corruption degrades to neutral (uncalibrated)
/// scoring AND surfaces an explicit warning rather than pretending everything is healthy.
pub async fn load_learn_state_with_warning(
    scraper: &Scraper,
    tenant: Option<&str>,
) -> (ContextLearnState, Option<String>) {
    match try_load_learn_state(scraper, tenant).await {
        Ok(Some(state)) => (state, None),
        Ok(None) => (ContextLearnState::default(), None),
        Err(err) => {
            tracing::warn!(
                error = %err,
                "corrupt persisted context learn state; degrading to neutral uncalibrated scoring"
            );
            let mut neutral = ContextLearnState::default();
            neutral.enabled = false;
            (
                neutral,
                Some(format!("learn_state_corrupt: {}", err.message)),
            )
        }
    }
}

pub async fn load_learn_state(scraper: &Scraper, tenant: Option<&str>) -> ContextLearnState {
    load_learn_state_with_warning(scraper, tenant).await.0
}

pub async fn save_learn_state(
    scraper: &Scraper,
    tenant: Option<&str>,
    state: &ContextLearnState,
) -> Result<(), ApiError> {
    let Some(store) = scraper.store_ref().cloned() else {
        return Err(ApiError::invalid(
            "persisting context learn state requires store configured on Scraper",
        ));
    };
    let bytes =
        serde_json::to_vec(state).map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
    let t = tenant.map(str::to_owned);
    store
        .blocking(move |st| {
            st.context_for(t.as_deref())
                .put_learn_bytes("default", &bytes)
        })
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("save learn state: {e}")))
}
