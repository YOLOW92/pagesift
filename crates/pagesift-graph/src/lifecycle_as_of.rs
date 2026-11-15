//! Point-in-time (`claims_as_of`) lifecycle reconstruction and coalesced claim timeline storage.

use std::collections::BTreeMap;

use crate::corroborate::ClaimRecord;
use crate::lifecycle::{
    ClaimConfirmationPoint, ClaimLifecycleRecord, ClaimStatus, ClaimTimelineEntry,
    HistoricalClaimEntry, MAX_CLAIM_HISTORY_LEN, MAX_CLAIM_TIMELINE_LEN,
    MAX_TIMELINE_CONFIRMATIONS_PER_ENTRY,
};

/// Record or extend a coalesced interval in `rec.timeline` at `observed_at_ms`.
pub(crate) fn record_timeline(rec: &mut ClaimLifecycleRecord, observed_at_ms: u64) {
    let point = ClaimConfirmationPoint {
        at_ms: observed_at_ms,
        observed_at_ms: rec.claim.observed_at_ms,
        last_confirmed_at_ms: rec.last_confirmed_at_ms,
        observation_count: rec.observation_count,
    };

    if let Some(last) = rec.timeline.last_mut() {
        let same_state = last.status == rec.status
            && claims_equal_except_observed_at(&last.claim, &rec.claim)
            && !last.confirmations.is_empty()
            && last.confirmations.len() < MAX_TIMELINE_CONFIRMATIONS_PER_ENTRY
            && observed_at_ms >= last.until_ms;
        if same_state {
            last.until_ms = last.until_ms.max(observed_at_ms);
            last.last_confirmed_at_ms = rec.last_confirmed_at_ms;
            last.observation_count = rec.observation_count;
            if let Some(prev_pt) = last.confirmations.last_mut()
                && prev_pt.at_ms == observed_at_ms
            {
                *prev_pt = point;
            } else {
                last.confirmations.push(point);
            }
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
        confirmations: vec![point],
    });
}

fn claims_equal_except_observed_at(a: &ClaimRecord, b: &ClaimRecord) -> bool {
    let ClaimRecord {
        claim_id: a_claim_id,
        subject_key: a_subject_key,
        resource_key: a_resource_key,
        field_path: a_field_path,
        normalized_field: a_normalized_field,
        normalized_value: a_normalized_value,
        raw_value: a_raw_value,
        doc_id: a_doc_id,
        source_url: a_source_url,
        host: a_host,
        source_kind: a_source_kind,
        engine: a_engine,
        observed_at_ms: _,
        temporal: a_temporal,
        canonical_url: a_canonical_url,
        evidence: a_evidence,
        contract_id: a_contract_id,
    } = a;
    let ClaimRecord {
        claim_id: b_claim_id,
        subject_key: b_subject_key,
        resource_key: b_resource_key,
        field_path: b_field_path,
        normalized_field: b_normalized_field,
        normalized_value: b_normalized_value,
        raw_value: b_raw_value,
        doc_id: b_doc_id,
        source_url: b_source_url,
        host: b_host,
        source_kind: b_source_kind,
        engine: b_engine,
        observed_at_ms: _,
        temporal: b_temporal,
        canonical_url: b_canonical_url,
        evidence: b_evidence,
        contract_id: b_contract_id,
    } = b;
    a_claim_id == b_claim_id
        && a_subject_key == b_subject_key
        && a_resource_key == b_resource_key
        && a_field_path == b_field_path
        && a_normalized_field == b_normalized_field
        && a_normalized_value == b_normalized_value
        && a_raw_value == b_raw_value
        && a_doc_id == b_doc_id
        && a_source_url == b_source_url
        && a_host == b_host
        && a_source_kind == b_source_kind
        && a_engine == b_engine
        && a_temporal == b_temporal
        && a_canonical_url == b_canonical_url
        && a_evidence == b_evidence
        && a_contract_id == b_contract_id
}

pub(crate) fn push_history(rec: &mut ClaimLifecycleRecord, archived_at_ms: u64) {
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
        archived_at_ms: Some(archived_at_ms),
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
/// or lacks exact intra-interval confirmation resolution prior to `as_of_ms`.
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

        if let Some((idx, _)) = rec
            .timeline
            .iter()
            .enumerate()
            .rfind(|(_, e)| e.since_ms <= as_of_ms)
        {
            let (restored, unavail) = restore_from_timeline(rec, idx, as_of_ms);
            history_unavailable |= unavail;
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
            if let Some(restored) = restore_truncated_before_timeline(rec, as_of_ms) {
                out.push(restored);
            }
            continue;
        }

        let (maybe_rec, unavail) = restore_without_timeline(rec, as_of_ms);
        history_unavailable |= unavail;
        if let Some(restored) = maybe_rec {
            out.push(restored);
        }
    }

    (out, history_unavailable)
}

fn restore_from_timeline(
    rec: &ClaimLifecycleRecord,
    idx: usize,
    as_of_ms: u64,
) -> (ClaimLifecycleRecord, bool) {
    let entry = &rec.timeline[idx];
    let mut restored = rec.clone();
    restored.claim = entry.claim.clone();
    restored.status = entry.status.clone();
    restored.timeline = rec.timeline[..=idx].to_vec();

    let status_leaks = status_has_future_timestamp(&entry.status, as_of_ms);
    let mut unavail = false;

    if !entry.confirmations.is_empty() {
        if let Some(pt) = entry
            .confirmations
            .iter()
            .filter(|p| p.at_ms <= as_of_ms)
            .rev()
            .max_by_key(|p| p.at_ms)
            && !status_leaks
            && pt.last_confirmed_at_ms <= as_of_ms
            && pt.observed_at_ms <= as_of_ms
        {
            restored.claim.observed_at_ms = pt.observed_at_ms;
            restored.last_confirmed_at_ms = pt.last_confirmed_at_ms;
            restored.observation_count = pt.observation_count;
            if let Some(last) = restored.timeline.last_mut() {
                last.confirmations.retain(|p| p.at_ms <= as_of_ms);
                last.until_ms = pt.at_ms;
                last.claim.observed_at_ms = pt.observed_at_ms;
                last.last_confirmed_at_ms = pt.last_confirmed_at_ms;
                last.observation_count = pt.observation_count;
            }
        } else {
            unavail = true;
            redact_legacy_timeline_tail(&mut restored, rec, idx, entry, as_of_ms);
        }
    } else if !status_leaks
        && entry.until_ms <= as_of_ms
        && entry.last_confirmed_at_ms <= as_of_ms
        && entry.claim.observed_at_ms <= as_of_ms
    {
        restored.last_confirmed_at_ms = entry.last_confirmed_at_ms;
        restored.observation_count = entry.observation_count;
    } else {
        unavail = true;
        redact_legacy_timeline_tail(&mut restored, rec, idx, entry, as_of_ms);
    }

    filter_history_as_of(
        &mut restored.history,
        as_of_ms,
        &restored.claim,
        &restored.status,
    );
    (restored, unavail)
}

fn redact_legacy_timeline_tail(
    restored: &mut ClaimLifecycleRecord,
    rec: &ClaimLifecycleRecord,
    idx: usize,
    entry: &ClaimTimelineEntry,
    as_of_ms: u64,
) {
    restored.history_truncated = true;
    if status_has_future_timestamp(&restored.status, as_of_ms) {
        restored.status = fallback_status_from_evidence(restored.claim.evidence.verified);
    }
    let safe_observed = if entry.claim.observed_at_ms <= as_of_ms {
        entry.claim.observed_at_ms
    } else if entry.since_ms <= as_of_ms {
        entry.since_ms
    } else {
        rec.first_observed_at_ms
    };
    restored.claim.observed_at_ms = safe_observed;

    if entry.last_confirmed_at_ms <= as_of_ms {
        restored.last_confirmed_at_ms = entry.last_confirmed_at_ms;
        restored.observation_count = entry.observation_count;
    } else {
        restored.last_confirmed_at_ms = safe_observed;
        let prior_count = idx
            .checked_sub(1)
            .and_then(|i| rec.timeline.get(i))
            .map(|p| p.observation_count)
            .unwrap_or(0);
        restored.observation_count = prior_count.saturating_add(1).min(entry.observation_count);
    }

    if let Some(last) = restored.timeline.last_mut() {
        last.confirmations.retain(|p| p.at_ms <= as_of_ms);
        last.until_ms = last.since_ms.min(as_of_ms);
        last.claim.observed_at_ms = restored.claim.observed_at_ms;
        last.status = restored.status.clone();
        last.last_confirmed_at_ms = restored.last_confirmed_at_ms;
        last.observation_count = restored.observation_count;
    }
}

fn restore_truncated_before_timeline(
    rec: &ClaimLifecycleRecord,
    as_of_ms: u64,
) -> Option<ClaimLifecycleRecord> {
    let (idx, hist) = rec
        .history
        .iter()
        .enumerate()
        .rfind(|(_, h)| h.observed_at_ms <= as_of_ms)?;
    let mut restored = rec.clone();
    apply_historical_entry(&mut restored, hist);
    restored.observation_count = u32::try_from(idx.saturating_add(1))
        .unwrap_or(u32::MAX)
        .min(rec.observation_count.max(1));
    restored.history = rec.history[..idx].to_vec();
    filter_history_as_of(
        &mut restored.history,
        as_of_ms,
        &restored.claim,
        &restored.status,
    );
    restored.timeline.clear();
    Some(restored)
}

fn restore_without_timeline(
    rec: &ClaimLifecycleRecord,
    as_of_ms: u64,
) -> (Option<ClaimLifecycleRecord>, bool) {
    if rec.claim.observed_at_ms <= as_of_ms {
        let mut copy = rec.clone();
        let future_status = status_has_future_timestamp(&copy.status, as_of_ms);
        let future_confirm = copy.last_confirmed_at_ms > as_of_ms;
        let unavail = future_status || future_confirm;
        if unavail {
            copy.history_truncated = true;
            if future_confirm {
                copy.last_confirmed_at_ms = copy.claim.observed_at_ms;
                let known_lower = u32::try_from(
                    copy.history
                        .iter()
                        .filter(|h| h.observed_at_ms <= as_of_ms)
                        .count(),
                )
                .unwrap_or(u32::MAX)
                .saturating_add(1)
                .min(rec.observation_count.max(1));
                copy.observation_count = known_lower;
            }
            if future_status {
                copy.status = fallback_status_from_evidence(copy.claim.evidence.verified);
            }
        }
        filter_history_as_of(&mut copy.history, as_of_ms, &copy.claim, &copy.status);
        return (Some(copy), unavail);
    }

    if let Some((idx, hist)) = rec
        .history
        .iter()
        .enumerate()
        .rfind(|(_, h)| h.observed_at_ms <= as_of_ms)
    {
        let mut restored = rec.clone();
        apply_historical_entry(&mut restored, hist);
        restored.history_truncated = true;
        restored.observation_count = u32::try_from(idx.saturating_add(1))
            .unwrap_or(u32::MAX)
            .min(rec.observation_count.max(1));
        restored.history = rec.history[..idx].to_vec();
        filter_history_as_of(
            &mut restored.history,
            as_of_ms,
            &restored.claim,
            &restored.status,
        );
        return (Some(restored), true);
    }

    (None, true)
}

fn apply_historical_entry(restored: &mut ClaimLifecycleRecord, hist: &HistoricalClaimEntry) {
    restored.claim.claim_id = hist.claim_id.clone();
    restored.claim.doc_id = hist.doc_id.clone();
    restored.claim.normalized_value = hist.value.clone();
    restored.claim.raw_value = hist.raw_value.clone();
    restored.claim.observed_at_ms = hist.observed_at_ms;
    restored.claim.evidence = hist.evidence.clone();
    restored.last_confirmed_at_ms = hist.observed_at_ms;
    restored.status = fallback_status_from_evidence(hist.evidence.verified);
}

fn fallback_status_from_evidence(verified: bool) -> ClaimStatus {
    if verified {
        ClaimStatus::Active
    } else {
        ClaimStatus::EvidenceDegraded {
            reason: "evidence verification degraded".to_owned(),
        }
    }
}

fn status_has_future_timestamp(status: &ClaimStatus, as_of_ms: u64) -> bool {
    match status {
        ClaimStatus::Withdrawn { at_ms, .. } | ClaimStatus::Superseded { at_ms, .. } => {
            *at_ms > as_of_ms
        }
        ClaimStatus::Unverified { since_ms, .. } => *since_ms > as_of_ms,
        ClaimStatus::Active | ClaimStatus::EvidenceDegraded { .. } => false,
    }
}

fn filter_history_as_of(
    history: &mut Vec<HistoricalClaimEntry>,
    as_of_ms: u64,
    restored_claim: &ClaimRecord,
    restored_status: &ClaimStatus,
) {
    let is_terminal = matches!(
        restored_status,
        ClaimStatus::Withdrawn { .. } | ClaimStatus::Superseded { .. }
    );
    history.retain(|h| match h.archived_at_ms {
        Some(archived_at_ms) => archived_at_ms <= as_of_ms,
        None => {
            h.observed_at_ms <= as_of_ms
                && (is_terminal
                    || !(h.claim_id == restored_claim.claim_id
                        && h.observed_at_ms == restored_claim.observed_at_ms))
        }
    });
}
