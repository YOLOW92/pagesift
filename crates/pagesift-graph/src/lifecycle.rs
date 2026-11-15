//! Claim and evidence lifecycle management across temporal observations.
//!
//! Distinguishes five fundamentally different events:
//! 1. A claim newly appears (`Appeared`) or is re-confirmed (`Confirmed`);
//! 2. The source updates a claim's value (`Updated` / `Superseded`);
//! 3. The source was fetched cleanly and no longer displays the claim, or is confirmed gone (`Withdrawn`);
//! 4. PageSift failed to fetch the source or received a suspect empty/barrier shell (`Unverified`);
//! 5. Supporting block evidence degraded (`EvidenceChanged`).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::conflict::ConflictRecord;
use crate::corroborate::ClaimRecord;
use crate::evidence::{EvidenceAlignment, EvidenceSpan};
use crate::hash::short_id;
use crate::norm::NormalizedValue;

const MAX_CLAIM_HISTORY_LEN: usize = 32; // [策略] 单声明保留历史版本链上限

/// Semantic classification of a resource observation outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ObservationOutcome {
    /// Resource was fetched and parsed with valid content.
    VerifiedContent,
    /// Conditional HTTP 304 or identical content hash confirmed no change.
    NotModified304,
    /// HTTP 200 returned an empty body, unrendered SPA shell, or access barrier.
    SuspectEmptyOrBarrier { reason: String },
    /// Network timeout, 5xx, rate limit (429), or transient fetch failure.
    TransientFetchError { error: String },
    /// Single 404 or missed in an incomplete crawl; awaiting confirmation.
    SuspectedGone { status_code: Option<u16> },
    /// Confirmed removal (HTTP 410, repeated 404s, or full error-free crawl absence).
    ConfirmedGone { reason: String },
}

/// Reason why a previously active claim was marked withdrawn.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WithdrawalReason {
    /// Source was fetched cleanly and the field/claim is no longer present.
    OmittedInVerifiedVersion,
    /// Source resource was confirmed removed (`ConfirmedGone`).
    ResourceConfirmedGone,
}

/// Current lifecycle status of a structured claim on a resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ClaimStatus {
    /// Confirmed present in the latest verified observation.
    Active,
    /// Replaced by a newer value on the same logical resource.
    Superseded {
        by_claim_id: String,
        by_doc_id: String,
        at_ms: u64,
    },
    /// Withdrawn by the source or due to confirmed resource removal.
    Withdrawn {
        at_ms: u64,
        reason: WithdrawalReason,
    },
    /// Latest observation failed or was inconclusive; prior claim is retained pending verification.
    Unverified { since_ms: u64, reason: String },
    /// Claim value still exists (e.g. in metadata island) but grounded block evidence degraded.
    EvidenceDegraded { reason: String },
}

impl ClaimStatus {
    pub fn is_active_or_unverified(&self) -> bool {
        matches!(
            self,
            Self::Active | Self::Unverified { .. } | Self::EvidenceDegraded { .. }
        )
    }
}

/// Historical value snapshot for a claim before it was updated or withdrawn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoricalClaimEntry {
    pub claim_id: String,
    pub doc_id: String,
    pub value: NormalizedValue,
    pub raw_value: Value,
    pub observed_at_ms: u64,
    pub evidence: EvidenceSpan,
}

/// Persistent lifecycle record tracking one `(subject_key, resource_key, normalized_field)` slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimLifecycleRecord {
    pub lifecycle_id: String,
    pub claim: ClaimRecord,
    pub status: ClaimStatus,
    pub first_observed_at_ms: u64,
    pub last_confirmed_at_ms: u64,
    pub observation_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<HistoricalClaimEntry>,
}

impl ClaimLifecycleRecord {
    pub fn slot_key(subject_key: &str, resource_key: &str, normalized_field: &str) -> String {
        short_id("clc", &[subject_key, resource_key, normalized_field])
    }

    pub fn new_active(claim: ClaimRecord) -> Self {
        let ts = claim.observed_at_ms;
        let lifecycle_id = Self::slot_key(
            &claim.subject_key,
            &claim.resource_key,
            &claim.normalized_field,
        );
        Self {
            lifecycle_id,
            claim,
            status: ClaimStatus::Active,
            first_observed_at_ms: ts,
            last_confirmed_at_ms: ts,
            observation_count: 1,
            history: Vec::new(),
        }
    }
}

/// Type of transition a claim underwent during an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClaimTransitionKind {
    Appeared,
    Confirmed,
    Updated,
    Withdrawn,
    Unverified,
    EvidenceChanged,
    Restored,
}

/// Auditable transition event describing how a claim changed between observations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimTransition {
    pub lifecycle_id: String,
    pub kind: ClaimTransitionKind,
    pub subject_key: String,
    pub resource_key: String,
    pub normalized_field: String,
    pub field_path: String,
    pub observed_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_value: Option<NormalizedValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_value: Option<NormalizedValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_evidence: Option<EvidenceSpan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_evidence: Option<EvidenceSpan>,
    pub explanation: String,
}

/// Incremental knowledge changes produced by a sync tick or multi-version update.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeDelta {
    pub transitions: Vec<ClaimTransition>,
    pub active_claims: usize,
    pub appeared_count: usize,
    pub updated_count: usize,
    pub confirmed_count: usize,
    pub withdrawn_count: usize,
    pub unverified_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub new_conflicts: Vec<ConflictRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved_conflict_ids: Vec<String>,
}

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

/// Reconcile previous claim lifecycles for a single logical resource against a new observation.
pub fn reconcile_resource_claims(
    previous: &[ClaimLifecycleRecord],
    outcome: &ObservationOutcome,
    new_claims: &[ClaimRecord],
    observed_at_ms: u64,
) -> (Vec<ClaimLifecycleRecord>, Vec<ClaimTransition>) {
    let mut updated_records = Vec::new();
    let mut transitions = Vec::new();

    match outcome {
        ObservationOutcome::NotModified304 => {
            for rec in previous {
                let mut next = rec.clone();
                if next.status.is_active_or_unverified() {
                    let was_unverified = matches!(next.status, ClaimStatus::Unverified { .. });
                    next.status = ClaimStatus::Active;
                    next.last_confirmed_at_ms = observed_at_ms;
                    next.observation_count = next.observation_count.saturating_add(1);
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
                updated_records.push(next);
            }
            return (updated_records, transitions);
        }
        ObservationOutcome::TransientFetchError { error }
        | ObservationOutcome::SuspectEmptyOrBarrier { reason: error } => {
            let msg =
                format!("observation inconclusive ({error}); retaining prior claim as unverified");
            return mark_all_unverified(previous, observed_at_ms, error, &msg);
        }
        ObservationOutcome::SuspectedGone { status_code } => {
            let reason = format!(
                "resource suspected gone (status={:?}); awaiting confirmation",
                status_code
            );
            return mark_all_unverified(previous, observed_at_ms, &reason, &reason);
        }
        ObservationOutcome::ConfirmedGone { reason } => {
            for rec in previous {
                let mut next = rec.clone();
                if next.status.is_active_or_unverified() {
                    push_history(&mut next);
                    next.status = ClaimStatus::Withdrawn {
                        at_ms: observed_at_ms,
                        reason: WithdrawalReason::ResourceConfirmedGone,
                    };
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

    let mut prev_by_field: BTreeMap<(String, String), &ClaimLifecycleRecord> = BTreeMap::new();
    for rec in previous {
        prev_by_field.insert(
            (
                rec.claim.subject_key.clone(),
                rec.claim.normalized_field.clone(),
            ),
            rec,
        );
    }

    let mut matched_keys: BTreeSet<(String, String)> = BTreeSet::new();
    for new_claim in new_claims {
        let key = (
            new_claim.subject_key.clone(),
            new_claim.normalized_field.clone(),
        );
        if !matched_keys.insert(key.clone()) {
            continue;
        }
        match prev_by_field.get(&key) {
            None => {
                let rec = ClaimLifecycleRecord::new_active(new_claim.clone());
                transitions.push(make_transition(
                    &rec,
                    ClaimTransitionKind::Appeared,
                    observed_at_ms,
                    TransitionSide::empty(),
                    TransitionSide::of(new_claim),
                    format!(
                        "claim first observed with value {}",
                        new_claim.normalized_value.raw()
                    ),
                ));
                updated_records.push(rec);
            }
            Some(old_rec) => {
                let mut next = (*old_rec).clone();
                let same_val = old_rec.claim.normalized_value.bucket_key()
                    == new_claim.normalized_value.bucket_key();
                let was_withdrawn = matches!(old_rec.status, ClaimStatus::Withdrawn { .. });
                let was_unverified = matches!(old_rec.status, ClaimStatus::Unverified { .. });

                if same_val && !was_withdrawn {
                    let ev_degraded =
                        old_rec.claim.evidence.verified && !new_claim.evidence.verified;
                    next.claim = new_claim.clone();
                    next.last_confirmed_at_ms = observed_at_ms;
                    next.observation_count = next.observation_count.saturating_add(1);
                    if ev_degraded {
                        let reason = match new_claim.evidence.alignment {
                            EvidenceAlignment::MetadataIsland => {
                                "visible block evidence degraded to metadata island".to_owned()
                            }
                            EvidenceAlignment::Unaligned => {
                                "claim value ungrounded in visible blocks".to_owned()
                            }
                            _ => "evidence verification degraded".to_owned(),
                        };
                        next.status = ClaimStatus::EvidenceDegraded {
                            reason: reason.clone(),
                        };
                        transitions.push(make_transition(
                            &next,
                            ClaimTransitionKind::EvidenceChanged,
                            observed_at_ms,
                            TransitionSide::of(&old_rec.claim),
                            TransitionSide::of(new_claim),
                            reason,
                        ));
                    } else {
                        next.status = ClaimStatus::Active;
                        let (kind, msg) = if was_unverified {
                            (
                                ClaimTransitionKind::Restored,
                                "claim re-verified after prior inconclusive observation",
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
                            TransitionSide::of(new_claim),
                            msg.into(),
                        ));
                    }
                } else {
                    push_history(&mut next);
                    next.claim = new_claim.clone();
                    next.status = ClaimStatus::Active;
                    next.last_confirmed_at_ms = observed_at_ms;
                    next.observation_count = next.observation_count.saturating_add(1);
                    let (kind, msg) = if was_withdrawn {
                        (
                            ClaimTransitionKind::Restored,
                            format!(
                                "previously withdrawn claim re-appeared with value {}",
                                new_claim.normalized_value.raw()
                            ),
                        )
                    } else {
                        (
                            ClaimTransitionKind::Updated,
                            format!(
                                "value updated from {} to {}",
                                old_rec.claim.normalized_value.raw(),
                                new_claim.normalized_value.raw()
                            ),
                        )
                    };
                    transitions.push(make_transition(
                        &next,
                        kind,
                        observed_at_ms,
                        TransitionSide::of(&old_rec.claim),
                        TransitionSide::of(new_claim),
                        msg,
                    ));
                }
                updated_records.push(next);
            }
        }
    }

    for (key, old_rec) in prev_by_field {
        if matched_keys.contains(&key) {
            continue;
        }
        let mut next = old_rec.clone();
        if next.status.is_active_or_unverified() {
            push_history(&mut next);
            next.status = ClaimStatus::Withdrawn {
                at_ms: observed_at_ms,
                reason: WithdrawalReason::OmittedInVerifiedVersion,
            };
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

    (updated_records, transitions)
}

fn mark_all_unverified(
    previous: &[ClaimLifecycleRecord],
    observed_at_ms: u64,
    reason: &str,
    explanation: &str,
) -> (Vec<ClaimLifecycleRecord>, Vec<ClaimTransition>) {
    let mut updated_records = Vec::new();
    let mut transitions = Vec::new();
    for rec in previous {
        let mut next = rec.clone();
        if next.status.is_active_or_unverified() {
            next.status = ClaimStatus::Unverified {
                since_ms: observed_at_ms,
                reason: reason.to_owned(),
            };
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

fn push_history(rec: &mut ClaimLifecycleRecord) {
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

/// Filter a multi-version claim slice so only the latest observation per `(resource_key, engine)`
/// participates in cross-source conflict detection.
pub fn latest_claims_per_resource(claims: &[ClaimRecord]) -> Vec<&ClaimRecord> {
    let mut latest_ts: BTreeMap<(&str, &str), u64> = BTreeMap::new();
    for c in claims {
        let entry = latest_ts
            .entry((c.resource_key.as_str(), c.engine.as_str()))
            .or_insert(0);
        if c.observed_at_ms > *entry {
            *entry = c.observed_at_ms;
        }
    }
    claims
        .iter()
        .filter(|c| {
            latest_ts
                .get(&(c.resource_key.as_str(), c.engine.as_str()))
                .is_none_or(|&max_ts| c.observed_at_ms == max_ts)
        })
        .collect()
}

/// Reconstruct the effective state of claim lifecycles as of `as_of_ms`.
pub fn claims_as_of(records: &[ClaimLifecycleRecord], as_of_ms: u64) -> Vec<ClaimLifecycleRecord> {
    let mut out = Vec::new();
    for rec in records {
        if rec.first_observed_at_ms > as_of_ms {
            continue;
        }
        if rec.claim.observed_at_ms <= as_of_ms {
            let mut copy = rec.clone();
            copy.history.retain(|h| h.observed_at_ms <= as_of_ms);
            if let ClaimStatus::Withdrawn { at_ms, .. } | ClaimStatus::Superseded { at_ms, .. } =
                copy.status
                && at_ms > as_of_ms
            {
                copy.status = ClaimStatus::Active;
            }
            out.push(copy);
            continue;
        }
        if let Some((idx, hist)) = rec
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
            restored.status = ClaimStatus::Active;
            restored.history = rec.history[..idx].to_vec();
            out.push(restored);
        }
    }
    out
}
