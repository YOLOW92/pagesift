//! Unified serializable request/response contracts for 1.6 Context Search, Pack, Action, and Learn.

use serde::{Deserialize, Serialize};

use crate::conflict::ConflictRecord;
use crate::context::delta::{ContextDelta, IndexVisibility};
use crate::context::unit::{KnowledgeUnit, UnitKind, UnitStatus};
use crate::corroborate::FactCluster;

/// Cross-version unit identity matching algorithm mode (with deterministic fallback switches).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UnitMatchMode {
    /// Strict exact hash + structural/Patience anchor matching only.
    ExactAnchorOnly,
    /// Exact anchors + greedy token-Dice local pairing baseline.
    GreedyDice,
    /// Exact anchors + entity-guarded bounded entropic Unbalanced Optimal Transport on local subgraphs.
    #[default]
    BoundedUot,
}

/// Validity status of a unit relative to a requested target timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ValidityStatus {
    ValidAtTarget,
    ExpiredAtTarget,
    NotYetValidAtTarget,
    UnknownValidity,
}

/// Six decoupled retrieval evaluation dimensions (never collapsed into one opaque score).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchDimensionScores {
    pub relevance: f32,
    pub support: f32,
    pub coverage: f32,
    pub freshness_validity: f32,
    pub calibrated_confidence: f32,
    pub source_independence: f32,
}

/// Multi-strategy evidence- and time-aware search request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSearchRequest {
    pub query: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_at_ms: Option<u64>,
    #[serde(default = "default_top_k")]
    pub limit: usize,
    #[serde(default)]
    pub include_unverified: bool,
    #[serde(default = "default_true")]
    pub include_conflicts: bool,
    #[serde(default = "default_true")]
    pub enable_semantic_sketch: bool,
    #[serde(default = "default_true")]
    pub learn_enabled: bool,
}

impl Default for ContextSearchRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            source_ids: Vec::new(),
            subject_key: None,
            required_fields: Vec::new(),
            as_of_ms: None,
            effective_at_ms: None,
            limit: default_top_k(),
            include_unverified: false,
            include_conflicts: true,
            enable_semantic_sketch: true,
            learn_enabled: true,
        }
    }
}

/// Single unit hit returned by [`ContextSearchReport`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSearchHit {
    pub unit: KnowledgeUnit,
    pub rank_score: f32,
    pub dimensions: SearchDimensionScores,
    pub validity_status: ValidityStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_terms: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_strategies: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub conflict_counterpart: bool,
}

/// Structured multi-dimensional output of Context Search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSearchReport {
    pub query: String,
    pub hits: Vec<ContextSearchHit>,
    pub dimensions: SearchDimensionScores,
    pub independent_source_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub origin_roots: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<ConflictRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fact_clusters: Vec<FactCluster>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strategies_used: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visibilities: Vec<IndexVisibility>,
    pub learn_applied: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Optimization strategy for compiling task-oriented [`ContextPack`] under token and evidence constraints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PackSelector {
    /// Closure-grouped submodular coverage + independent-root reward under twin-greedy knapsack.
    #[default]
    SubmodularClosure,
    /// Bounded candidate-set L-ensemble Determinantal Point Process (Cholesky MAP).
    BoundedDpp,
    /// Maximal Marginal Relevance baseline.
    MmrGreedy,
    /// Pure relevance rank greedy baseline.
    TopKGreedy,
}

/// Ordering strategy of selected context units inside the compiled prompt payload.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PackOrdering {
    /// Places highest-utility & conflict-critical items at prompt boundaries (mitigating lost-in-the-middle).
    #[default]
    EdgeSalience,
    ScoreDescending,
    DocumentOrder,
}

/// Request to compile a budget- and evidence-constrained [`ContextPack`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPackRequest {
    pub search: ContextSearchRequest,
    #[serde(default = "default_budget_tokens")]
    pub budget_tokens: usize,
    #[serde(default)]
    pub selector: PackSelector,
    #[serde(default)]
    pub ordering: PackOrdering,
    /// Fractional reserve (`0.0..0.5`) guarding against model BPE vs heuristic token drift.
    #[serde(default = "default_safety_margin")]
    pub token_safety_margin: f32,
    #[serde(default = "default_true")]
    pub must_keep_conflicts: bool,
    #[serde(default = "default_true")]
    pub must_keep_table_headers: bool,
    #[serde(default = "default_true")]
    pub must_keep_qualifiers: bool,
}

impl Default for ContextPackRequest {
    fn default() -> Self {
        Self {
            search: ContextSearchRequest::default(),
            budget_tokens: default_budget_tokens(),
            selector: PackSelector::default(),
            ordering: PackOrdering::default(),
            token_safety_margin: default_safety_margin(),
            must_keep_conflicts: true,
            must_keep_table_headers: true,
            must_keep_qualifiers: true,
        }
    }
}

/// Machine-auditable citation entry in a [`ContextPack`]'s provenance manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceEntry {
    pub ref_id: String,
    pub unit_id: String,
    pub revision_id: String,
    pub doc_id: String,
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
    pub status: UnitStatus,
}

/// Compiled section placed into the final [`ContextPack`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackedSection {
    pub ref_id: String,
    pub unit_id: String,
    pub kind: UnitKind,
    pub rendered_text: String,
    pub tokens: usize,
    pub utility_gain: f32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub table_header_injected: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub paired_for_conflict: bool,
}

/// Transparent end-to-end CPU/latency accounting for Search + Pack compilation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackCostBreakdown {
    pub search_micros: u64,
    pub rerank_micros: u64,
    pub compile_micros: u64,
    pub total_micros: u64,
    pub candidate_units_considered: usize,
}

/// Task-compiled, evidence-preserving context package.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPack {
    pub query: String,
    pub compiled_context: String,
    pub sections: Vec<PackedSection>,
    pub provenance_manifest: Vec<ProvenanceEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<ConflictRecord>,
    pub total_tokens: usize,
    pub effective_budget_tokens: usize,
    pub requested_budget_tokens: usize,
    pub budget_insufficient: bool,
    pub must_keep_preserved: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted_must_keep_reasons: Vec<String>,
    pub selector_used: PackSelector,
    pub ordering_used: PackOrdering,
    pub dimensions: SearchDimensionScores,
    pub independent_source_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visibilities: Vec<IndexVisibility>,
    pub cost: PackCostBreakdown,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// High-level goal-oriented Context Action capabilities.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionMode {
    #[default]
    Context,
    Research,
    Verify,
    Watch,
    Changes,
}

/// Statistical and deterministic knowledge coverage diagnosis (refusing pseudo-precision when invalid).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageDiagnosis {
    pub term_coverage: f32,
    pub required_field_coverage: f32,
    pub independent_sources_observed: usize,
    pub min_independent_sources_required: usize,
    pub estimable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub good_turing_unseen: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub good_turing_ci_high: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_recapture_total: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_estimable_reason: Option<String>,
}

/// Verifiable stopping reason for sequential knowledge acquisition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StoppingReason {
    Sufficient,
    VoiBelowCost,
    BudgetExhausted,
    PermissionRestricted,
    NoNewEvidence,
    SourceUnavailable,
    ResyncRequired,
}

/// Explicit missing evidence or coverage gap reported when Context is incomplete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InformationGap {
    pub kind: String,
    pub target: String,
    pub detail: String,
}

/// Audit record of one controlled acquisition action decided via VOI and shadow prices.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcquisitionStepRecord {
    pub kind: String,
    pub target: String,
    pub expected_voi: f64,
    pub shadow_cost: f64,
    pub executed: bool,
    pub units_added_or_updated: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Goal-oriented Agent information operation request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextActionRequest {
    #[serde(default)]
    pub mode: ActionMode,
    pub pack: ContextPackRequest,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidate_urls: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_staleness_ms: Option<u64>,
    #[serde(default = "default_min_indep")]
    pub min_independent_sources: usize,
    #[serde(default)]
    pub allow_network_acquisition: bool,
    #[serde(default)]
    pub allow_external_search: bool,
    #[serde(default)]
    pub strict_constrain_to_urls: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_hosts: Vec<String>,
    #[serde(default = "default_max_acq_reqs")]
    pub max_acquisition_requests: usize,
    #[serde(default)]
    pub since_delta_seq: Option<u64>,
}

impl Default for ContextActionRequest {
    fn default() -> Self {
        Self {
            mode: ActionMode::Context,
            pack: ContextPackRequest::default(),
            candidate_urls: Vec::new(),
            max_staleness_ms: None,
            min_independent_sources: default_min_indep(),
            allow_network_acquisition: false,
            allow_external_search: false,
            strict_constrain_to_urls: false,
            allowed_hosts: Vec::new(),
            max_acquisition_requests: default_max_acq_reqs(),
            since_delta_seq: None,
        }
    }
}

/// Complete auditable outcome of a goal-oriented Context Action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextActionOutcome {
    pub mode: ActionMode,
    pub stopping_reason: StoppingReason,
    pub reused_existing_context: bool,
    pub coverage: CoverageDiagnosis,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<InformationGap>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acquisition_steps: Vec<AcquisitionStepRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<ContextPack>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deltas: Vec<ContextDelta>,
}

fn default_top_k() -> usize {
    // [策略] 默认检索返回前 16 个候选知识单元供下游查看或进一步编译
    16
}

fn default_budget_tokens() -> usize {
    // [策略] 默认上下文编译预算 2048 tokens，兼顾多文档覆盖与常见 Agent 单步窗口开销
    2048
}

fn default_safety_margin() -> f32 {
    // [策略] 默认预留 5% token 裕度抵消启发式分词与不同模型 BPE 的计数波动
    0.05
}

fn default_min_indep() -> usize {
    1
}

fn default_max_acq_reqs() -> usize {
    // [策略] 单次 Action 默认最多允许 4 次补充采集请求，防止无界外联
    4
}

fn default_true() -> bool {
    true
}

fn is_false(v: &bool) -> bool {
    !*v
}
