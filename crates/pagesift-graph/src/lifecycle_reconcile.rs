//! Reconciliation engine for per-resource claim lifecycle transitions.

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::lifecycle_as_of::{push_history, record_timeline};

struct TransitionSide {
    value: Option<NormalizedValue>,
    evidence: Option<EvidenceSpan>,
}

impl TransitionSide {
    const fn empty() -> Self {
        Self {
            value: None,
            evidence: None,
        }
    }

    fn of(claim: &ClaimRecord) -> Self {
        Self {
            value: Some(claim.normalized_value.clone()),
            evidence: Some(claim.evidence.clone()),
        }
    }

    fn value_only(claim: &ClaimRecord) -> Self {
        Self {
            value: Some(claim.normalized_value.clone()),
            evidence: None,
        }
    }
}

fn make_transition(
    rec: &ClaimLifecycleRecord,
    kind: ClaimTransitionKind,
    observed_at_ms: u64,
    before: TransitionSide,
    after: TransitionSide,
    explanation: String,
) -> ClaimTransition {
    ClaimTransition {
        lifecycle_id: rec.lifecycle_id.clone(),
        kind,
        subject_key: rec.claim.subject_key.clone(),
        resource_key: rec.claim.resource_key.clone(),
        normalized_field: rec.claim.normalized_field.clone(),
        field_path: rec.claim.field_path.clone(),
        observed_at_ms,
        before_value: before.value,
        after_value: after.value,
        before_evidence: before.evidence,
        after_evidence: after.evidence,
        explanation,
    }
}

fn rec_contract(rec: &ClaimLifecycleRecord) -> &str {
    if rec.claim.contract_id.is_empty() {
        DEFAULT_CONTRACT_ID
    } else {
        rec.claim.contract_id.as_str()
    }
}

fn contract_matches(rec: &ClaimLifecycleRecord, scope: Option<&str>) -> bool {
    match scope {
        None => true,
        Some(cid) => {
            let target = if cid.trim().is_empty() {
                DEFAULT_CONTRACT_ID
            } else {
                cid
            };
            rec_contract(rec) == target
        }
    }
}

/// Reconcile previous claim lifecycles for a single logical resource against a new observation.
pub fn reconcile_resource_claims(
    previous: &[ClaimLifecycleRecord],
    outcome: &ObservationOutcome,
    new_claims: &[ClaimRecord],
    observed_at_ms: u64,
) -> (Vec<ClaimLifecycleRecord>, Vec<ClaimTransition>) {
    let inferred = new_claims.first().map(|c| {
        if c.contract_id.is_empty() {
            DEFAULT_CONTRACT_ID
        } else {
            c.contract_id.as_str()
        }
    });
    reconcile_resource_claims_for_contract(previous, outcome, new_claims, observed_at_ms, inferred)
}

/// Reconcile previous claim lifecycles scoped to `contract_id` (isolating unrelated extraction schemas).
pub fn reconcile_resource_claims_for_contract(
    previous: &[ClaimLifecycleRecord],
    outcome: &ObservationOutcome,
    new_claims: &[ClaimRecord],
    observed_at_ms: u64,
    contract_id: Option<&str>,
) -> (Vec<ClaimLifecycleRecord>, Vec<ClaimTransition>) {
    let mut updated_records = Vec::new();
    let mut transitions = Vec::new();

    match outcome {
        ObservationOutcome::NotModified304 => {
            for rec in previous {
                let mut next = rec.clone();
                if contract_matches(&next, contract_id) && next.status.is_active_or_unverified() {
                    let was_unverified = matches!(next.status, ClaimStatus::Unverified { .. });
                    let was_degraded = matches!(next.status, ClaimStatus::EvidenceDegraded { .. });
                    next.last_confirmed_at_ms = observed_at_ms;
                    next.observation_count = next.observation_count.saturating_add(1);
                    if was_degraded || (was_unverified && !next.claim.evidence.verified) {
                        if was_unverified {
                            next.status = ClaimStatus::EvidenceDegraded {
                                reason: degradation_reason(&next.claim.evidence.alignment),
                            };
                        }
                        record_timeline(&mut next, observed_at_ms);
                        transitions.push(make_transition(
                            &next,
                            ClaimTransitionKind::Confirmed,
                            observed_at_ms,
                            TransitionSide::of(&next.claim),
                            TransitionSide::of(&next.claim),
                            "confirmed unchanged via conditional check (evidence remains degraded)"
                                .into(),
                        ));
                    } else {
                        next.status = ClaimStatus::Active;
                        record_timeline(&mut next, observed_at_ms);
                        let kind = if was_unverified {
                            ClaimTransitionKind::Restored
                        } else {
                            ClaimTransitionKind::Confirmed
                        };
                        transitions.push(make_transition(
                            &next,
                            kind,
                            observed_at_ms,
                            TransitionSide::of(&next.claim),
                            TransitionSide::of(&next.claim),
                            "confirmed unchanged via conditional check".into(),
                        ));
                    }
                }
                updated_records.push(next);
            }
            return (updated_records, transitions);
        }
        ObservationOutcome::TransientFetchError { error }
        | ObservationOutcome::SuspectEmptyOrBarrier { reason: error } => {
            let msg =
                format!("observation inconclusive ({error}); retaining prior claim as unverified");
            return mark_all_unverified(previous, observed_at_ms, error, &msg, contract_id);
        }
        ObservationOutcome::SuspectedGone { status_code } => {
            let reason = format!(
                "resource suspected gone (status={:?}); awaiting confirmation",
                status_code
            );
            return mark_all_unverified(previous, observed_at_ms, &reason, &reason, contract_id);
        }
        ObservationOutcome::ConfirmedGone { reason } => {
            for rec in previous {
                let mut next = rec.clone();
                if next.status.is_active_or_unverified() {
                    push_history(&mut next, observed_at_ms);
                    next.status = ClaimStatus::Withdrawn {
                        at_ms: observed_at_ms,
                        reason: WithdrawalReason::ResourceConfirmedGone,
                    };
                    record_timeline(&mut next, observed_at_ms);
                    transitions.push(make_transition(
                        &next,
                        ClaimTransitionKind::Withdrawn,
                        observed_at_ms,
                        TransitionSide::of(&next.claim),
                        TransitionSide::empty(),
                        format!("resource confirmed gone: {reason}"),
                    ));
                }
                updated_records.push(next);
            }
            return (updated_records, transitions);
        }
        ObservationOutcome::VerifiedContent => {}
    }

    let mut untouched_other_contracts = Vec::new();
    let mut prev_by_slot: BTreeMap<String, &ClaimLifecycleRecord> = BTreeMap::new();
    for rec in previous {
        if !contract_matches(rec, contract_id) {
            untouched_other_contracts.push(rec.clone());
            continue;
        }
        prev_by_slot.insert(rec.lifecycle_id.clone(), rec);
    }

    let mut occ_counts: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    let mut matched_slots: BTreeSet<String> = BTreeSet::new();

    for new_claim in new_claims {
        let cid =
            if new_claim.contract_id.is_empty() || new_claim.contract_id == DEFAULT_CONTRACT_ID {
                contract_id.unwrap_or(DEFAULT_CONTRACT_ID)
            } else {
                new_claim.contract_id.as_str()
            };
        let occ_key = (
            new_claim.subject_key.clone(),
            cid.to_owned(),
            new_claim.normalized_field.clone(),
        );
        let occ_idx = *occ_counts.get(&occ_key).unwrap_or(&0);
        occ_counts.insert(occ_key, occ_idx + 1);

        let mut claim_scoped = new_claim.clone();
        if claim_scoped.contract_id.is_empty() || claim_scoped.contract_id == DEFAULT_CONTRACT_ID {
            claim_scoped.contract_id = cid.to_owned();
        }
        let slot_id = ClaimLifecycleRecord::slot_key_for_claim(&claim_scoped, occ_idx);
        matched_slots.insert(slot_id.clone());

        let old_match = prev_by_slot.get(&slot_id).copied().or_else(|| {
            // Backward compatibility when a legacy record lacks contract_id or occ index
            (occ_idx == 0).then(|| {
                prev_by_slot
                    .values()
                    .find(|r| {
                        r.claim.subject_key == claim_scoped.subject_key
                            && r.claim.normalized_field == claim_scoped.normalized_field
                    })
                    .copied()
            })?
        });

        match old_match {
            None => {
                let rec = ClaimLifecycleRecord::new_observed(claim_scoped.clone(), occ_idx);
                transitions.push(make_transition(
                    &rec,
                    ClaimTransitionKind::Appeared,
                    observed_at_ms,
                    TransitionSide::empty(),
                    TransitionSide::of(&claim_scoped),
                    format!(
                        "claim first observed with value {}",
                        claim_scoped.normalized_value.raw()
                    ),
                ));
                updated_records.push(rec);
            }
            Some(old_rec) => {
                matched_slots.insert(old_rec.lifecycle_id.clone());
                let mut next = old_rec.clone();
                next.lifecycle_id = slot_id;
                let same_val = old_rec.claim.normalized_value.bucket_key()
                    == claim_scoped.normalized_value.bucket_key();
                let was_withdrawn = matches!(
                    old_rec.status,
                    ClaimStatus::Withdrawn { .. } | ClaimStatus::Superseded { .. }
                );
                let was_unverified = matches!(old_rec.status, ClaimStatus::Unverified { .. });
                let was_degraded = matches!(old_rec.status, ClaimStatus::EvidenceDegraded { .. });

                if same_val && !was_withdrawn {
                    next.claim = claim_scoped.clone();
                    next.last_confirmed_at_ms = observed_at_ms;
                    next.observation_count = next.observation_count.saturating_add(1);
                    if !claim_scoped.evidence.verified
                        && (old_rec.claim.evidence.verified || was_degraded)
                    {
                        let reason = degradation_reason(&claim_scoped.evidence.alignment);
                        next.status = ClaimStatus::EvidenceDegraded {
                            reason: reason.clone(),
                        };
                        record_timeline(&mut next, observed_at_ms);
                        let (kind, msg) = if was_degraded {
                            (
                                ClaimTransitionKind::Confirmed,
                                "claim confirmed with ungrounded or degraded evidence".to_owned(),
                            )
                        } else {
                            (ClaimTransitionKind::EvidenceChanged, reason)
                        };
                        transitions.push(make_transition(
                            &next,
                            kind,
                            observed_at_ms,
                            TransitionSide::of(&old_rec.claim),
                            TransitionSide::of(&claim_scoped),
                            msg,
                        ));
                    } else {
                        next.status = ClaimStatus::Active;
                        record_timeline(&mut next, observed_at_ms);
                        let (kind, msg) = if was_unverified || was_degraded {
                            (
                                ClaimTransitionKind::Restored,
                                "claim re-verified after prior inconclusive or degraded observation",
                            )
                        } else {
                            (
                                ClaimTransitionKind::Confirmed,
                                "claim value and evidence confirmed",
                            )
                        };
                        transitions.push(make_transition(
                            &next,
                            kind,
                            observed_at_ms,
                            TransitionSide::of(&old_rec.claim),
                            TransitionSide::of(&claim_scoped),
                            msg.into(),
                        ));
                    }
                } else {
                    push_history(&mut next, observed_at_ms);
                    next.claim = claim_scoped.clone();
                    next.last_confirmed_at_ms = observed_at_ms;
                    next.observation_count = next.observation_count.saturating_add(1);
                    next.status = if claim_scoped.evidence.verified {
                        ClaimStatus::Active
                    } else {
                        ClaimStatus::EvidenceDegraded {
                            reason: degradation_reason(&claim_scoped.evidence.alignment),
                        }
                    };
                    record_timeline(&mut next, observed_at_ms);
                    let (kind, msg) = if was_withdrawn {
                        (
                            ClaimTransitionKind::Restored,
                            format!(
                                "previously withdrawn claim re-appeared with value {}",
                                claim_scoped.normalized_value.raw()
                            ),
                        )
                    } else {
                        (
                            ClaimTransitionKind::Updated,
                            format!(
                                "value updated from {} to {}",
                                old_rec.claim.normalized_value.raw(),
                                claim_scoped.normalized_value.raw()
                            ),
                        )
                    };
                    transitions.push(make_transition(
                        &next,
                        kind,
                        observed_at_ms,
                        TransitionSide::of(&old_rec.claim),
                        TransitionSide::of(&claim_scoped),
                        msg,
                    ));
                }
                updated_records.push(next);
            }
        }
    }

    for (slot_id, old_rec) in prev_by_slot {
        if matched_slots.contains(&slot_id) {
            continue;
        }
        let mut next = old_rec.clone();
        if next.status.is_active_or_unverified() {
            push_history(&mut next, observed_at_ms);
            next.status = ClaimStatus::Withdrawn {
                at_ms: observed_at_ms,
                reason: WithdrawalReason::OmittedInVerifiedVersion,
            };
            record_timeline(&mut next, observed_at_ms);
            transitions.push(make_transition(
                &next,
                ClaimTransitionKind::Withdrawn,
                observed_at_ms,
                TransitionSide::of(&next.claim),
                TransitionSide::empty(),
                "claim omitted in latest verified document version".into(),
            ));
        }
        updated_records.push(next);
    }

    updated_records.extend(untouched_other_contracts);
    (updated_records, transitions)
}

fn mark_all_unverified(
    previous: &[ClaimLifecycleRecord],
    observed_at_ms: u64,
    reason: &str,
    explanation: &str,
    contract_id: Option<&str>,
) -> (Vec<ClaimLifecycleRecord>, Vec<ClaimTransition>) {
    let mut updated_records = Vec::new();
    let mut transitions = Vec::new();
    for rec in previous {
        let mut next = rec.clone();
        if contract_matches(&next, contract_id) && next.status.is_active_or_unverified() {
            next.status = ClaimStatus::Unverified {
                since_ms: observed_at_ms,
                reason: reason.to_owned(),
            };
            record_timeline(&mut next, observed_at_ms);
            transitions.push(make_transition(
                &next,
                ClaimTransitionKind::Unverified,
                observed_at_ms,
                TransitionSide::of(&next.claim),
                TransitionSide::value_only(&next.claim),
                explanation.to_owned(),
            ));
        }
        updated_records.push(next);
    }
    (updated_records, transitions)
}
