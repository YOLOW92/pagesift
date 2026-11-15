//! Point-in-time (`claims_as_of`) lifecycle reconstruction and coalesced claim timeline storage.

use std::collections::BTreeMap;

use crate::corroborate::ClaimRecord;
use crate::lifecycle::{
    ClaimLifecycleRecord, ClaimStatus, ClaimTimelineEntry, HistoricalClaimEntry,
    MAX_CLAIM_HISTORY_LEN, MAX_CLAIM_TIMELINE_LEN,
};

/// Record or extend a coalesced interval in `rec.timeline` at `observed_at_ms`.
pub(crate) fn record_timeline(rec: &mut ClaimLifecycleRecord, observed_at_ms: u64) {
    if let Some(last) = rec.timeline.last_mut() {
        let same_state = same_status_bucket(&last.status, &rec.status)
            && last.claim.normalized_value.bucket_key() == rec.claim.normalized_value.bucket_key()
            && last.claim.evidence.verified == rec.claim.evidence.verified
            && last.claim.contract_id == rec.claim.contract_id;
        if same_state {
            last.until_ms = last.until_ms.max(observed_at_ms);
            last.last_confirmed_at_ms = rec.last_confirmed_at_ms;
            last.observation_count = rec.observation_count;
            return;
        }
    }
    if rec.timeline.len() >= MAX_CLAIM_TIMELINE_LEN {
        rec.timeline.remove(0);
        rec.history_truncated = true;
    }
    rec.timeline.push(ClaimTimelineEntry {
        since_ms: observed_at_ms,
        until_ms: observed_at_ms,
        status: rec.status.clone(),
        claim: rec.claim.clone(),
        last_confirmed_at_ms: rec.last_confirmed_at_ms,
        observation_count: rec.observation_count,
    });
}

fn same_status_bucket(a: &ClaimStatus, b: &ClaimStatus) -> bool {
    matches!(
        (a, b),
        (ClaimStatus::Active, ClaimStatus::Active)
            | (
                ClaimStatus::Unverified { .. },
                ClaimStatus::Unverified { .. }
            )
            | (
                ClaimStatus::EvidenceDegraded { .. },
                ClaimStatus::EvidenceDegraded { .. }
            )
            | (ClaimStatus::Withdrawn { .. }, ClaimStatus::Withdrawn { .. })
            | (
                ClaimStatus::Superseded { .. },
                ClaimStatus::Superseded { .. }
            )
    )
}

pub(crate) fn push_history(rec: &mut ClaimLifecycleRecord) {
    if rec.history.len() >= MAX_CLAIM_HISTORY_LEN {
        rec.history.remove(0);
    }
    rec.history.push(HistoricalClaimEntry {
        claim_id: rec.claim.claim_id.clone(),
        doc_id: rec.claim.doc_id.clone(),
        value: rec.claim.normalized_value.clone(),
        raw_value: rec.claim.raw_value.clone(),
        observed_at_ms: rec.claim.observed_at_ms,
        evidence: rec.claim.evidence.clone(),
    });
}

/// Filter a multi-version claim slice so only the latest observation per `(resource_key, engine, contract)`
/// participates in cross-source conflict detection.
pub fn latest_claims_per_resource(claims: &[ClaimRecord]) -> Vec<&ClaimRecord> {
    let mut latest_ts: BTreeMap<(&str, &str, &str), u64> = BTreeMap::new();
    for c in claims {
        let entry = latest_ts
            .entry((
                c.resource_key.as_str(),
                c.engine.as_str(),
                c.contract_id.as_str(),
            ))
            .or_insert(0);
        if c.observed_at_ms > *entry {
            *entry = c.observed_at_ms;
        }
    }
    claims
        .iter()
        .filter(|c| {
            latest_ts
                .get(&(
                    c.resource_key.as_str(),
                    c.engine.as_str(),
                    c.contract_id.as_str(),
                ))
                .is_none_or(|&max_ts| c.observed_at_ms == max_ts)
        })
        .collect()
}

/// Reconstruct the effective state of claim lifecycles as of `as_of_ms`.
pub fn claims_as_of(records: &[ClaimLifecycleRecord], as_of_ms: u64) -> Vec<ClaimLifecycleRecord> {
    claims_as_of_checked(records, as_of_ms).0
}

/// Reconstruct point-in-time claim lifecycles and report whether retained history was truncated
/// prior to `as_of_ms`.
pub fn claims_as_of_checked(
    records: &[ClaimLifecycleRecord],
    as_of_ms: u64,
) -> (Vec<ClaimLifecycleRecord>, bool) {
    let mut out = Vec::new();
    let mut history_unavailable = false;

    for rec in records {
        if rec.first_observed_at_ms > as_of_ms {
            continue;
        }

        if let Some((idx, entry)) = rec
            .timeline
            .iter()
            .enumerate()
            .rfind(|(_, e)| e.since_ms <= as_of_ms)
        {
            let mut restored = rec.clone();
            restored.claim = entry.claim.clone();
            restored.status = entry.status.clone();
            restored.last_confirmed_at_ms = entry.last_confirmed_at_ms.min(as_of_ms);
            restored.observation_count = entry.observation_count;
            restored.timeline = rec.timeline[..=idx].to_vec();
            restored.history.retain(|h| h.observed_at_ms <= as_of_ms);
            out.push(restored);
            continue;
        }

        if rec.history_truncated
            && rec
                .timeline
                .first()
                .is_some_and(|first| first.since_ms > as_of_ms)
        {
            history_unavailable = true;
            if let Some((idx, hist)) = rec
                .history
                .iter()
                .enumerate()
                .rfind(|(_, h)| h.observed_at_ms <= as_of_ms)
            {
                let mut restored = rec.clone();
                restored.claim.claim_id = hist.claim_id.clone();
                restored.claim.doc_id = hist.doc_id.clone();
                restored.claim.normalized_value = hist.value.clone();
                restored.claim.raw_value = hist.raw_value.clone();
                restored.claim.observed_at_ms = hist.observed_at_ms;
                restored.claim.evidence = hist.evidence.clone();
                restored.last_confirmed_at_ms = hist.observed_at_ms;
                restored.status = if hist.evidence.verified {
                    ClaimStatus::Active
                } else {
                    ClaimStatus::EvidenceDegraded {
                        reason: "retained historical snapshot lacked verified block evidence"
                            .to_owned(),
                    }
                };
                restored.history = rec.history[..idx].to_vec();
                restored.timeline.clear();
                out.push(restored);
            }
            continue;
        }

        // Legacy fallback for 1.5.0 persisted records without `timeline`.
        if rec.claim.observed_at_ms <= as_of_ms {
            let mut copy = rec.clone();
            copy.history.retain(|h| h.observed_at_ms <= as_of_ms);
            match copy.status {
                ClaimStatus::Withdrawn { at_ms, .. } | ClaimStatus::Superseded { at_ms, .. }
                    if at_ms > as_of_ms =>
                {
                    copy.status = if copy.claim.evidence.verified {
                        ClaimStatus::Active
                    } else {
                        ClaimStatus::EvidenceDegraded {
                            reason: "evidence verification degraded".to_owned(),
                        }
                    };
                }
                ClaimStatus::Unverified { since_ms, .. } if since_ms > as_of_ms => {
                    copy.status = if copy.claim.evidence.verified {
                        ClaimStatus::Active
                    } else {
                        ClaimStatus::EvidenceDegraded {
                            reason: "evidence verification degraded".to_owned(),
                        }
                    };
                }
                _ => {}
            }
            out.push(copy);
        } else if let Some((idx, hist)) = rec
            .history
            .iter()
            .enumerate()
            .rev()
            .find(|(_, h)| h.observed_at_ms <= as_of_ms)
        {
            let mut restored = rec.clone();
            restored.claim.claim_id = hist.claim_id.clone();
            restored.claim.doc_id = hist.doc_id.clone();
            restored.claim.normalized_value = hist.value.clone();
            restored.claim.raw_value = hist.raw_value.clone();
            restored.claim.observed_at_ms = hist.observed_at_ms;
            restored.claim.evidence = hist.evidence.clone();
            restored.last_confirmed_at_ms = hist.observed_at_ms;
            restored.status = if hist.evidence.verified {
                ClaimStatus::Active
            } else {
                ClaimStatus::EvidenceDegraded {
                    reason: "evidence verification degraded".to_owned(),
                }
            };
            restored.history = rec.history[..idx].to_vec();
            out.push(restored);
        } else {
            history_unavailable = true;
        }
    }

    (out, history_unavailable)
}
