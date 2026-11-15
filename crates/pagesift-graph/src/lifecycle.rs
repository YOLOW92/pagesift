//! Claim and evidence lifecycle management across temporal observations.
//!
//! Distinguishes five fundamentally different events:
//! 1. A claim newly appears (`Appeared`) or is re-confirmed (`Confirmed`);
//! 2. The source updates a claim's value (`Updated` / `Superseded`);
//! 3. The source was fetched cleanly and no longer displays the claim, or is confirmed gone (`Withdrawn`);
//! 4. PageSift failed to fetch the source or received a suspect empty/barrier shell (`Unverified`);
//! 5. Supporting block evidence degraded (`EvidenceChanged`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::conflict::ConflictRecord;
use crate::corroborate::{ClaimRecord, DEFAULT_CONTRACT_ID};
use crate::evidence::{EvidenceAlignment, EvidenceSpan};
use crate::hash::short_id;
use crate::norm::NormalizedValue;

pub(crate) const MAX_CLAIM_HISTORY_LEN: usize = 32; // [策略] 旧版摘要历史链上限
pub(crate) const MAX_CLAIM_TIMELINE_LEN: usize = 256; // [策略] 单声明时序区间历史上限

fn is_false(v: &bool) -> bool {
    !*v
}

/// Unified semantic classification of one resource observation across HTTP, render, and Sync.
pub fn outcome_for_scrape(
    status_code: u16,
    not_modified: bool,
    capture_sufficient: bool,
    is_barrier: bool,
    fetch_failed: bool,
    detail: Option<&str>,
) -> ObservationOutcome {
    if fetch_failed || status_code == 0 {
        return ObservationOutcome::TransientFetchError {
            error: detail.unwrap_or("fetch failed").to_string(),
        };
    }
    match status_code {
        404 => ObservationOutcome::SuspectedGone {
            status_code: Some(404),
        },
        410 => ObservationOutcome::ConfirmedGone {
            reason: "HTTP 410 Gone".into(),
        },
        400..=599 => ObservationOutcome::TransientFetchError {
            error: detail
                .map(|d| format!("HTTP {status_code}: {d}"))
                .unwrap_or_else(|| format!("HTTP {status_code}")),
        },
        _ if not_modified => ObservationOutcome::NotModified304,
        _ if is_barrier || !capture_sufficient => ObservationOutcome::SuspectEmptyOrBarrier {
            reason: detail
                .unwrap_or(if is_barrier {
                    "access barrier or challenge detected"
                } else {
                    "insufficient capture coverage"
                })
                .to_string(),
        },
        _ => ObservationOutcome::VerifiedContent,
    }
}

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

/// State-interval history snapshot used for point-in-time `claims_as_of` reconstruction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimTimelineEntry {
    pub since_ms: u64,
    pub until_ms: u64,
    pub status: ClaimStatus,
    pub claim: ClaimRecord,
    pub last_confirmed_at_ms: u64,
    pub observation_count: u32,
}

/// Persistent lifecycle record tracking one `(subject_key, resource_key, contract_id, field, occurrence)` slot.
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timeline: Vec<ClaimTimelineEntry>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub history_truncated: bool,
}

impl ClaimLifecycleRecord {
    pub fn slot_key(subject_key: &str, resource_key: &str, normalized_field: &str) -> String {
        short_id("clc", &[subject_key, resource_key, normalized_field])
    }

    pub fn slot_key_for_claim(claim: &ClaimRecord, occurrence_idx: usize) -> String {
        let is_default = claim.contract_id.is_empty() || claim.contract_id == DEFAULT_CONTRACT_ID;
        if is_default && occurrence_idx == 0 {
            Self::slot_key(
                &claim.subject_key,
                &claim.resource_key,
                &claim.normalized_field,
            )
        } else {
            let occ = occurrence_idx.to_string();
            let cid = if claim.contract_id.is_empty() {
                DEFAULT_CONTRACT_ID
            } else {
                &claim.contract_id
            };
            short_id(
                "clc",
                &[
                    &claim.subject_key,
                    &claim.resource_key,
                    cid,
                    &claim.normalized_field,
                    &occ,
                ],
            )
        }
    }

    pub fn new_active(claim: ClaimRecord) -> Self {
        Self::new_observed(claim, 0)
    }

    pub fn new_observed(claim: ClaimRecord, occurrence_idx: usize) -> Self {
        let ts = claim.observed_at_ms;
        let lifecycle_id = Self::slot_key_for_claim(&claim, occurrence_idx);
        let status = if claim.evidence.alignment == EvidenceAlignment::Unaligned {
            ClaimStatus::EvidenceDegraded {
                reason: degradation_reason(&claim.evidence.alignment),
            }
        } else {
            ClaimStatus::Active
        };
        let mut rec = Self {
            lifecycle_id,
            claim,
            status,
            first_observed_at_ms: ts,
            last_confirmed_at_ms: ts,
            observation_count: 1,
            history: Vec::new(),
            timeline: Vec::new(),
            history_truncated: false,
        };
        record_timeline(&mut rec, ts);
        rec
    }
}

fn degradation_reason(alignment: &EvidenceAlignment) -> String {
    match alignment {
        EvidenceAlignment::MetadataIsland => {
            "visible block evidence degraded to metadata island".to_owned()
        }
        EvidenceAlignment::Unaligned => "claim value ungrounded in visible blocks".to_owned(),
        _ => "evidence verification degraded".to_owned(),
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

#[path = "lifecycle_reconcile.rs"]
mod lifecycle_reconcile;
use crate::lifecycle_as_of::record_timeline;
pub use crate::lifecycle_as_of::{claims_as_of, claims_as_of_checked, latest_claims_per_resource};
pub use lifecycle_reconcile::{reconcile_resource_claims, reconcile_resource_claims_for_contract};
