//! Epistemic stratification, Belnap four-valued evidence state, Carneades proof standards,
//! scope/negative dependencies, and persistent versioned conclusion objects.

use serde::{Deserialize, Serialize};

use crate::context::UnitStatus;
use crate::hash::short_id;
use crate::norm::NormalizedValue;
use crate::resolve::contract::{CounterSearchAudit, ResolveGap};
use crate::resolve::provenance::{CutSetAnalysis, ProvenanceExpr};

fn is_false(v: &bool) -> bool {
    !*v
}

/// Epistemic layer of a knowledge object. Model inferences and conclusions must NEVER be
/// silently written back into DataGraph or ContextStore as external observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EpistemicKind {
    Observation,
    SourceClaim,
    Evidence,
    Inference,
    Conclusion,
    Hypothesis,
    Unknown,
}

/// Belnap (1977) four-valued evidence state tracking supporting and refuting evidence on
/// two independent dimensions rather than collapsing into a single truth bit or score.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceBelnap {
    pub has_support: bool,
    pub has_refutation: bool,
}

impl EvidenceBelnap {
    pub const UNKNOWN: Self = Self {
        has_support: false,
        has_refutation: false,
    };
    pub const SUPPORTED: Self = Self {
        has_support: true,
        has_refutation: false,
    };
    pub const REFUTED: Self = Self {
        has_support: false,
        has_refutation: true,
    };
    pub const CONFLICTED: Self = Self {
        has_support: true,
        has_refutation: true,
    };

    pub fn combine(self, other: Self) -> Self {
        Self {
            has_support: self.has_support || other.has_support,
            has_refutation: self.has_refutation || other.has_refutation,
        }
    }

    pub fn label(self) -> &'static str {
        match (self.has_support, self.has_refutation) {
            (false, false) => "unknown",
            (true, false) => "supported",
            (false, true) => "refuted",
            (true, true) => "conflicted",
        }
    }
}

/// Auditable resolution state of a claim or conclusion (distinct from objective world truth).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResolutionState {
    /// Satisfies the requested proof standard with grounded independent evidence and no active refutation.
    Supported,
    /// Evidence supports the statement within the observed finite sample, but open-world or scope bounds apply.
    QualifiedInObservedScope,
    /// Refuted by grounded counter-evidence, unit/period/time mismatch, polarity flip, or comparative counterexample.
    Refuted,
    /// Both independent supporting and independent refuting evidence coexist.
    Conflicted,
    /// Available evidence is insufficient to confirm or refute (or citations are missing/unaligned).
    Insufficient,
    /// Universal, superlative, or negative-existential claim lacks closed-world scope completeness.
    ScopeIncomplete,
    /// Causal assertion lacks identification evidence (only correlation/co-occurrence observed).
    CausalUnidentifiable,
    /// Statement is subjective, rhetorical, or structurally unverifiable against documentary evidence.
    Unverifiable,
    /// Underlying evidence, source instance, or scope changed (or observation failed transiently); revalidation pending.
    StalePendingRevalidation,
    /// Requires explicit human or higher-privilege review before acceptance.
    NeedsReview,
}

impl ResolutionState {
    pub fn is_supported_like(self) -> bool {
        matches!(self, Self::Supported | Self::QualifiedInObservedScope)
    }

    pub fn requires_attention(self) -> bool {
        matches!(
            self,
            Self::Refuted
                | Self::Conflicted
                | Self::Insufficient
                | Self::ScopeIncomplete
                | Self::CausalUnidentifiable
                | Self::StalePendingRevalidation
                | Self::NeedsReview
        )
    }
}

/// Carneades-inspired proof standard governing evidentiary burden before marking `Supported`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProofStandard {
    /// At least one grounded supporting evidence root and no active refutation.
    Scintilla,
    /// Independent supporting roots strictly exceed independent refuting roots, with no hard violation.
    #[default]
    Preponderance,
    /// At least 2 independent origin roots support the claim, zero unresolved independent refutations,
    /// and all ordinary/scope premises are met.
    ClearAndConvincing,
    /// Required for universal/superlative/negative-existential assertions: demands closed scope completeness
    /// in addition to clear and convincing support; otherwise downgrades cleanly.
    StrictUniversal,
}

/// Quantifier / logical scope shape extracted from an atomic claim.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClaimQuantifier {
    #[default]
    Existential,
    Universal,
    Superlative,
    NegativeExistential,
}

impl ClaimQuantifier {
    pub fn is_scope_sensitive(self) -> bool {
        matches!(
            self,
            Self::Universal | Self::Superlative | Self::NegativeExistential
        )
    }
}

/// Carneades premise role in an argument or derivation graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PremiseRole {
    OrdinaryPremise,
    Assumption,
    Exception,
    ScopeCompleteness,
}

/// Direction of an evidence item relative to a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EvidencePolarity {
    Supports,
    Refutes,
    Qualifies,
    ContextualOnly,
}

/// Classification of deterministic quantitative / unit / temporal comparison outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QuantitativeMismatchKind {
    Matched,
    ValueMismatch,
    UnitOrCurrencyMismatch,
    BillingPeriodMismatch,
    UnitOrPeriodUnspecified,
    ComparativeCounterexample,
}

/// Deterministic quantitative check result (small-scale verification only; does not preempt 1.9 Compute).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuantitativeCheck {
    pub kind: QuantitativeMismatchKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_value: Option<NormalizedValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_value: Option<NormalizedValue>,
    pub detail: String,
}

/// Grounded reference to a specific `KnowledgeUnit` revision and byte span supporting or refuting a claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceRef {
    pub evidence_id: String,
    pub epistemic_kind: EpistemicKind,
    pub polarity: EvidencePolarity,
    pub unit_id: String,
    pub revision_id: String,
    pub content_hash: String,
    pub doc_id: String,
    pub resource_key: String,
    pub source_url: String,
    pub origin_root: String,
    pub origin_independent: bool,
    pub byte_start: usize,
    pub byte_end: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heading_path: Vec<String>,
    pub observed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
    pub unit_status: UnitStatus,
    pub excerpt: String,
    /// Whether this unit was explicitly cited by the candidate answer (vs discovered during verification).
    #[serde(default, skip_serializing_if = "is_false")]
    pub explicitly_cited: bool,
    pub match_reason: String,
}

impl EvidenceRef {
    pub fn compute_id(unit_id: &str, revision_id: &str, polarity: EvidencePolarity) -> String {
        let p = match polarity {
            EvidencePolarity::Supports => "sup",
            EvidencePolarity::Refutes => "ref",
            EvidencePolarity::Qualifies => "qual",
            EvidencePolarity::ContextualOnly => "ctx",
        };
        short_id("evr", &[unit_id, revision_id, p])
    }
}

/// Context-preserving atomic claim extracted from a natural-language answer or structured statement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AtomicClaim {
    pub claim_id: String,
    pub index: usize,
    pub text: String,
    /// Preserved qualifier/condition clauses (`unless`, `except`, `only if`, `除非`, `不含`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub qualifiers: Vec<String>,
    /// Extracted entity/subject anchors (SKU, model ID, product/org name, code symbol).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entity_anchors: Vec<String>,
    /// Extracted calendar years explicitly asserted in the claim.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub asserted_years: Vec<u64>,
    /// Explicit inline citations (`[ref-1]`, `[1]`, URL, or `unt_...`) attached to this claim.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cited_refs: Vec<String>,
    pub negated: bool,
    pub quantifier: ClaimQuantifier,
    pub asserts_causality: bool,
    pub verifiable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unverifiable_reason: Option<String>,
}

impl AtomicClaim {
    pub fn compute_id(index: usize, text: &str) -> String {
        let idx = index.to_string();
        short_id("rclm", &[&idx, text.trim()])
    }
}

/// Multi-dimensional verification verdict for a single [`AtomicClaim`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimVerdict {
    pub claim: AtomicClaim,
    pub belnap: EvidenceBelnap,
    pub state: ResolutionState,
    pub proof_standard: ProofStandard,
    /// Decoupled verification dimensions (never collapsed into one opaque boolean):
    pub citation_present: bool,
    pub citation_accessible: bool,
    pub semantic_supported: bool,
    pub temporal_valid: Option<bool>,
    pub quantitative_valid: Option<bool>,
    pub polarity_consistent: Option<bool>,
    pub qualifier_preserved: bool,
    pub causal_identifiable: Option<bool>,
    pub scope_sufficient: bool,
    /// Calibrated probability ONLY when backed by independent hard ground-truth calibration (`None` otherwise!).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibrated_probability: Option<f32>,
    /// Raw deterministic heuristic alignment score in `[0.0, 1.0]` for internal ranking & calibration input only.
    pub raw_alignment_score: f32,
    pub independent_support_roots: Vec<String>,
    pub independent_refute_roots: Vec<String>,
    pub total_support_hits: usize,
    pub total_refute_hits: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supporting_evidence: Vec<EvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refuting_evidence: Vec<EvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub qualifying_evidence: Vec<EvidenceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantitative_check: Option<QuantitativeCheck>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<ResolveGap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ProvenanceExpr>,
}

/// Whether the studied comparison universe is closed or open-world.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScopeDomainKind {
    #[default]
    OpenWorld,
    ClosedObservedSet,
}

/// Explicit scope, completeness, and negative-dependency declaration for a conclusion.
/// Enables incremental invalidation when NEW objects enter the domain even if old citations do not change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeDependency {
    pub domain_kind: ScopeDomainKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subject_keys: Vec<String>,
    /// Entities observed inside the comparison domain `S` when this conclusion was checked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observed_entities: Vec<String>,
    /// Indexed scope predicates (`source:<id>`, `term:<kw>`, `category:<c>`, `neg:<term>`)
    /// matched against newly appearing units during Live change propagation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_predicates: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_negative_or_universal_dependency: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_at_ms: Option<u64>,
}

/// Auditable correction entry recorded whenever a conclusion transitions state or version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CorrectionRecord {
    pub from_version: u64,
    pub to_version: u64,
    pub from_state: ResolutionState,
    pub to_state: ResolutionState,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggering_unit_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggering_delta_seqs: Vec<u64>,
    pub at_ms: u64,
}

/// Immutable point-in-time snapshot of a conclusion version for historical audit and replay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConclusionRevision {
    pub conclusion_id: String,
    pub version: u64,
    pub content_digest: String,
    pub answer_text: String,
    pub belnap: EvidenceBelnap,
    pub state: ResolutionState,
    pub proof_standard: ProofStandard,
    pub claims: Vec<ClaimVerdict>,
    pub scope: ScopeDependency,
    pub origin_roots: Vec<String>,
    pub independent_support_roots: Vec<String>,
    pub independent_refute_roots: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_gaps: Vec<ResolveGap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ProvenanceExpr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut_set_analysis: Option<CutSetAnalysis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counter_audit: Option<CounterSearchAudit>,
    pub checked_at_ms: u64,
    pub verifier_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
}

/// Persistent, versioned, evidence-grounded conclusion object in 1.7 Resolve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConclusionRecord {
    pub schema_version: u32,
    pub conclusion_id: String,
    pub version: u64,
    pub content_digest: String,
    pub epistemic_kind: EpistemicKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    pub answer_text: String,
    pub belnap: EvidenceBelnap,
    pub state: ResolutionState,
    pub proof_standard: ProofStandard,
    pub claims: Vec<ClaimVerdict>,
    pub scope: ScopeDependency,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exceptions: Vec<String>,
    /// Upstream conclusion IDs this conclusion builds upon (cycle-checked; never counted as external observations).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_conclusion_ids: Vec<String>,
    /// All distinct unit IDs depended upon by this conclusion (for O(1) reverse invalidation).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependent_unit_ids: Vec<String>,
    /// All distinct resource keys depended upon by this conclusion.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependent_resource_keys: Vec<String>,
    /// Distinct canonical origin roots across all evidence referenced by this conclusion.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub origin_roots: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub independent_support_roots: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub independent_refute_roots: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ProvenanceExpr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut_set_analysis: Option<CutSetAnalysis>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_gaps: Vec<ResolveGap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counter_audit: Option<CounterSearchAudit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corrections: Vec<CorrectionRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
    pub verifier_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub checked_at_ms: u64,
}

impl ConclusionRecord {
    pub fn mint_id(query: Option<&str>, answer_text: &str, scope_sources: &[String]) -> String {
        let q = query.unwrap_or_default();
        let srcs = scope_sources.join("\u{1f}");
        short_id("rcon", &[q, answer_text.trim(), &srcs])
    }

    pub fn compute_digest(
        answer_text: &str,
        state: ResolutionState,
        belnap: EvidenceBelnap,
        dependent_unit_ids: &[String],
        independent_support_roots: &[String],
        independent_refute_roots: &[String],
    ) -> String {
        let st = format!("{state:?}");
        let bl = belnap.label();
        let u = dependent_unit_ids.join("\u{1f}");
        let s = independent_support_roots.join("\u{1f}");
        let r = independent_refute_roots.join("\u{1f}");
        short_id("rcdg", &[answer_text.trim(), &st, bl, &u, &s, &r])
    }

    pub fn to_revision(&self) -> ConclusionRevision {
        ConclusionRevision {
            conclusion_id: self.conclusion_id.clone(),
            version: self.version,
            content_digest: self.content_digest.clone(),
            answer_text: self.answer_text.clone(),
            belnap: self.belnap,
            state: self.state,
            proof_standard: self.proof_standard,
            claims: self.claims.clone(),
            scope: self.scope.clone(),
            origin_roots: self.origin_roots.clone(),
            independent_support_roots: self.independent_support_roots.clone(),
            independent_refute_roots: self.independent_refute_roots.clone(),
            unresolved_gaps: self.unresolved_gaps.clone(),
            provenance: self.provenance.clone(),
            cut_set_analysis: self.cut_set_analysis.clone(),
            counter_audit: self.counter_audit.clone(),
            checked_at_ms: self.checked_at_ms,
            verifier_version: self.verifier_version.clone(),
            model_id: self.model_id.clone(),
            stale_reason: self.stale_reason.clone(),
        }
    }
}
