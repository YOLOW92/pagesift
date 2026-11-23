//! 1.7 Resolve data model: epistemic stratification, Belnap four-valued evidence status,
//! Carneades-style proof standards & premise roles, provenance semiring with common-cause-collapsed
//! minimal cut sets, explicit scope/negative dependencies, and versioned conclusion contracts.

pub mod contract;
pub mod model;
pub mod provenance;

pub use contract::{
    CounterHypothesis, CounterProbeKind, CounterProbePlan, CounterProbeResult, CounterSearchAudit,
    ReconcileImpactReport, ReconcileMode, ResolveCheckOutcome, ResolveCheckRequest, ResolveEvent,
    ResolveEventKind, ResolveGap, ResolveGapKind, ResolveLearnFeedback, ResolveLearnSummary,
    ResolveStoppingReason, RevalidateMode, RevalidateOutcome, VerifierMode,
};
pub use model::{
    AtomicClaim, ClaimQuantifier, ClaimVerdict, ConclusionRecord, ConclusionRevision,
    CorrectionRecord, EpistemicKind, EvidenceBelnap, EvidencePolarity, EvidenceRef, PremiseRole,
    ProofStandard, QuantitativeCheck, QuantitativeMismatchKind, ResolutionState, ScopeDependency,
    ScopeDomainKind,
};
pub use provenance::{
    CutSetAnalysis, MAX_CUT_SETS, MAX_PROVENANCE_DEPTH, MinimalCutSet, OriginEvent, ProvenanceExpr,
    compute_minimal_cut_sets, detect_dependency_cycle,
};

/// Current wire/storage schema version for 1.7 Resolve conclusions and events.
pub const RESOLVE_SCHEMA_VERSION: u32 = 1;
/// Deterministic baseline verifier algorithm version identifier (bumping invalidates stale learn state).
pub const DEFAULT_VERIFIER_VERSION: &str = "resolve-det-v1";
