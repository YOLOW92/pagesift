//! Serializable request/response contracts for 1.7 Resolve: Check, Gap, Counter, Live, and Learn.
//! Designed for clean extensibility toward 1.8 Mesh events, 1.9 Compute alignment, and 2.0 Runtime routing.

use serde::{Deserialize, Serialize};

use crate::context::AcquisitionStepRecord;
use crate::resolve::model::{
    ClaimVerdict, ConclusionRecord, EvidenceRef, ProofStandard, ResolutionState, ScopeDomainKind,
};

fn default_true() -> bool {
    true
}

fn default_min_indep() -> usize {
    1
}

fn default_max_acq() -> usize {
    // [策略] 单次 Resolve 补证/反证采集默认最多 4 次请求（与 1.6 Context Action 保持一致防无界外联）
    4
}

/// Execution mode for semantic claim verification (deterministic core first; optional LLM judge).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VerifierMode {
    /// Pure deterministic verification (zero LLM calls; reproducible and low latency).
    #[default]
    DeterministicOnly,
    /// Deterministic hard-veto checks first; invokes configured LLM only on ambiguous entailment
    /// with deterministic quote-grounding post-verification and safe fallback on failure.
    HybridAssisted,
}

/// Structured information gap derived backwards from failed or inconclusive claim checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResolveGapKind {
    MissingEvidence,
    InvalidOrInaccessibleCitation,
    InsufficientIndependentSources,
    UnitOrPeriodAmbiguous,
    TemporalCoverageGap,
    ScopeCompletenessGap,
    QualifierOrExceptionUnchecked,
    UnresolvedConflict,
    CausalIdentificationMissing,
    AccessOrBudgetRestricted,
}

/// Actionable gap prioritized by Minimal Cut Set structural criticality and Howard VOI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveGap {
    pub gap_id: String,
    pub kind: ResolveGapKind,
    pub claim_id: String,
    pub target_query: String,
    /// Higher criticality indicates a Single Point of Failure in the conclusion's provenance.
    pub criticality: f32,
    pub detail: String,
    pub resolved: bool,
}

/// Category of proactive counter-evidence probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CounterProbeKind {
    DirectContradiction,
    OfficialErrataOrUpdate,
    ExceptionOrQualifier,
    ComparativeDomainScan,
    CompetingHypothesis,
}

/// Planned adversarial probe targeting a vulnerable premise or scope assumption.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CounterProbePlan {
    pub probe_id: String,
    pub kind: CounterProbeKind,
    pub claim_id: String,
    pub query: String,
    pub target_premise: String,
    pub priority: f32,
}

/// Outcome of executing one counter-evidence probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CounterProbeResult {
    pub probe_id: String,
    pub kind: CounterProbeKind,
    pub claim_id: String,
    pub query: String,
    pub executed: bool,
    pub searched_local_units: usize,
    pub searched_network_requests: usize,
    pub counter_evidence_found: usize,
    pub note: String,
}

/// Explicit competing explanation preserved alongside the primary candidate conclusion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CounterHypothesis {
    pub hypothesis_id: String,
    pub claim_id: String,
    pub description: String,
    pub supporting_roots: Vec<String>,
    pub evidence: Vec<EvidenceRef>,
}

/// Systematic-review-style audit of counter-evidence search coverage and limitations.
/// Absence of found counter-evidence NEVER proves objective truth.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CounterSearchAudit {
    pub enabled: bool,
    pub probes_planned: usize,
    pub probes_executed: usize,
    pub counter_evidence_found: usize,
    pub independent_roots_checked: Vec<String>,
    pub domain_kind: ScopeDomainKind,
    pub search_failures: Vec<String>,
    pub blocked_targets: Vec<String>,
    pub probe_results: Vec<CounterProbeResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub competing_hypotheses: Vec<CounterHypothesis>,
    /// Explicit reminder of epistemic coverage limits (e.g. `"no_counterevidence_found_within_searched_scope"`).
    pub coverage_statement: String,
}

/// Explicit stopping reason for Resolve check + Gap/Counter loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResolveStoppingReason {
    Verified,
    RefutedOrConflicted,
    GapsRemainPermissionRestricted,
    BudgetExhausted,
    VoiBelowCost,
    NoNewEvidence,
    UnverifiableOrCausalBoundary,
}

/// Request to verify an answer or structured claims against Context / inline documents and optionally persist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveCheckRequest {
    /// Candidate natural-language answer to decompose and verify (required unless `claims` is supplied).
    #[serde(default)]
    pub answer: String,
    /// Optional explicit atomic claims supplied directly by caller.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conclusion_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_conclusion_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub domain_kind: ScopeDomainKind,
    #[serde(default)]
    pub proof_standard: ProofStandard,
    #[serde(default)]
    pub verifier_mode: VerifierMode,
    #[serde(default = "default_min_indep")]
    pub min_independent_sources: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_at_ms: Option<u64>,
    #[serde(default)]
    pub include_unverified: bool,
    #[serde(default = "default_true")]
    pub enable_counter_search: bool,
    #[serde(default = "default_true")]
    pub enable_gap_resolution: bool,
    #[serde(default)]
    pub allow_network_acquisition: bool,
    #[serde(default)]
    pub allow_external_search: bool,
    #[serde(default)]
    pub strict_constrain_to_urls: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidate_urls: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_hosts: Vec<String>,
    #[serde(default = "default_max_acq")]
    pub max_acquisition_requests: usize,
    #[serde(default)]
    pub persist: bool,
    #[serde(default = "default_true")]
    pub learn_enabled: bool,
}

impl Default for ResolveCheckRequest {
    fn default() -> Self {
        Self {
            answer: String::new(),
            claims: Vec::new(),
            query: None,
            domain: None,
            conclusion_id: None,
            source_ids: Vec::new(),
            subject_key: None,
            parent_conclusion_ids: Vec::new(),
            assumptions: Vec::new(),
            domain_kind: ScopeDomainKind::OpenWorld,
            proof_standard: ProofStandard::Preponderance,
            verifier_mode: VerifierMode::DeterministicOnly,
            min_independent_sources: default_min_indep(),
            as_of_ms: None,
            effective_at_ms: None,
            include_unverified: false,
            enable_counter_search: true,
            enable_gap_resolution: true,
            allow_network_acquisition: false,
            allow_external_search: false,
            strict_constrain_to_urls: false,
            candidate_urls: Vec::new(),
            allowed_hosts: Vec::new(),
            max_acquisition_requests: default_max_acq(),
            persist: false,
            learn_enabled: true,
        }
    }
}

/// Complete auditable output of a Resolve check + optional Gap/Counter/Persist execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveCheckOutcome {
    pub conclusion: ConclusionRecord,
    pub persisted: bool,
    pub stopping_reason: ResolveStoppingReason,
    pub verifier_mode_used: VerifierMode,
    pub llm_calls: usize,
    pub network_requests: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acquisition_steps: Vec<AcquisitionStepRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub elapsed_micros: u64,
}

/// Strategy for Live change reconciliation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReconcileMode {
    /// Incremental reverse-dependency + scope-predicate propagation with Salsa Early Cutoff;
    /// automatically falls back to conservative full-source invalidation on delta gap or ABA replacement.
    #[default]
    Incremental,
    /// Mark all conclusions tied to the target source(s) as impacted for full verification comparison.
    FullSource,
}

/// Strategy for executing Live revalidation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RevalidateMode {
    #[default]
    Incremental,
    Full,
}

/// Detailed impact report produced by consuming `ContextDelta` updates into Resolve.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileImpactReport {
    pub source_ids: Vec<String>,
    pub deltas_consumed: usize,
    pub impacted_conclusion_ids: Vec<String>,
    pub scope_impacted_conclusion_ids: Vec<String>,
    pub observation_lost_conclusion_ids: Vec<String>,
    /// Conclusions whose physical citation coordinates shifted (`RelocatedOnly` / `Reconfirmed`) without invalidation.
    pub early_cutoff_conclusion_ids: Vec<String>,
    pub full_fallback_sources: Vec<String>,
    pub aba_superseded_sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Result of revalidating one or more impacted/requested conclusions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevalidateOutcome {
    pub mode: RevalidateMode,
    pub checked_count: usize,
    pub state_changed_count: usize,
    pub unchanged_early_cutoff_count: usize,
    pub conclusions: Vec<ConclusionRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub elapsed_micros: u64,
}

/// Replayable conclusion lifecycle event (persisted in bounded ring; forward-compatible with 1.8 Mesh.Events).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResolveEventKind {
    ConclusionCreated,
    ConclusionRevalidated,
    ConclusionMarkedStale,
    ConclusionCorrected,
    ConclusionDeleted,
    SourceInstanceSuperseded,
    ScopeExpanded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveEvent {
    pub event_seq: u64,
    pub kind: ResolveEventKind,
    pub conclusion_id: String,
    pub version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_state: Option<ResolutionState>,
    pub to_state: ResolutionState,
    pub reason: String,
    pub at_ms: u64,
}

/// Independent ground-truth feedback sample for conservative Resolve strategy learning.
/// Self-grades or user silence MUST set `is_hard_ground_truth: false` (ignored by calibrator).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveLearnFeedback {
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub claim_kind: String,
    pub raw_score: f32,
    pub grounded_supported: bool,
    pub is_hard_ground_truth: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counter_probe_kind: Option<CounterProbeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counter_found: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier_version: Option<String>,
}

/// Inspectable summary of persisted Resolve strategy learning & drift fallback state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveLearnSummary {
    pub version: u32,
    pub verifier_version: String,
    pub enabled: bool,
    pub frozen: bool,
    pub bucket_count: usize,
    pub total_hard_samples: usize,
    pub drift_fallbacks: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fallback_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub probe_rates: Vec<(String, f64, u64)>,
}

/// Helper to summarize a list of claim verdicts into aggregate Belnap state and root sets.
pub fn aggregate_claim_verdicts(
    verdicts: &[ClaimVerdict],
) -> (
    crate::resolve::model::EvidenceBelnap,
    Vec<String>,
    Vec<String>,
) {
    use std::collections::BTreeSet;
    let mut belnap = crate::resolve::model::EvidenceBelnap::UNKNOWN;
    let mut sup = BTreeSet::new();
    let mut refu = BTreeSet::new();
    for v in verdicts {
        belnap = belnap.combine(v.belnap);
        sup.extend(v.independent_support_roots.iter().cloned());
        refu.extend(v.independent_refute_roots.iter().cloned());
    }
    (
        belnap,
        sup.into_iter().collect(),
        refu.into_iter().collect(),
    )
}
