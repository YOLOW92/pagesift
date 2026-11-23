#![forbid(unsafe_code)]

//! Lightweight cross-source provenance, citation, version lineage, evidence, and conflict graph.
//!
//! Designed as a deterministic streaming layer rather than a full knowledge-graph database:
//! heavy passes remain opt-in and release per-page text/DOM buffers immediately after ingestion.

pub mod block_index;
pub mod builder;
#[cfg(test)]
mod builder_tests;
pub mod citation;
pub mod conflict;
pub mod context;
pub mod corroborate;
pub mod delta;
pub mod evidence;
mod evidence_json;
mod hash;
pub mod independence;
pub mod lifecycle;
mod lifecycle_as_of;
#[cfg(test)]
mod lifecycle_tests;
pub mod model;
pub mod norm;
mod norm_parse;
pub mod provenance;
pub mod resolve;
pub mod temporal;
pub mod version;

pub use builder::StreamingGraphBuilder;
pub use citation::{
    AnchorSample, CitationEdge, LinkRole, LinkSection, RawLinkObservation, aggregate_citations,
    classify_link_role, extract_markdown_links,
};
pub use conflict::{
    ConflictKind, ConflictRecord, ConflictSeverity, ConflictVariant, PreferredHint,
    detect_conflicts, detect_conflicts_with_lineage,
};
pub use corroborate::{
    ClaimRecord, DEFAULT_CONTRACT_ID, FactCluster, SubjectKey, SubjectKeySource,
    build_fact_clusters, build_fact_clusters_from_lifecycles_checked,
    build_fact_clusters_with_lineage, build_fact_clusters_with_lineage_checked,
    resolve_entity_subject_keys, resolve_subject_key,
};
pub use delta::{DocumentInput, GraphSlice, GraphStats, GraphSummary, PageGraphDelta};
pub use evidence::{
    EvidenceAlignment, EvidenceSpan, ExtractionMethod, RawFieldTrace, align_traces,
    collect_json_traces,
};
pub use hash::{
    compact_excerpt, host_of, normalize_resource_key, normalize_url, same_origin, sha256_hex,
};
pub use independence::{
    OriginRelation, SourceLineage, compute_source_lineage, independent_roots_for_claim_group,
};
pub use lifecycle::{
    ClaimConfirmationPoint, ClaimLifecycleRecord, ClaimStatus, ClaimTimelineEntry, ClaimTransition,
    ClaimTransitionKind, HistoricalClaimEntry, KnowledgeDelta, ObservationOutcome,
    WithdrawalReason, outcome_for_scrape, reconcile_resource_claims,
    reconcile_resource_claims_for_contract,
};
pub use lifecycle_as_of::{claims_as_of, claims_as_of_checked, latest_claims_per_resource};
pub use model::{
    BlockKind, BlockRef, DocNode, GraphLimits, GraphOptions, IndexedBlock, SourceKind,
    build_block_refs, build_indexed_blocks, build_indexed_blocks_checked,
};
pub use norm::{
    DatePrecision, MoneyPeriod, NormalizedValue, ValueComparison, canonical_currency_code,
    compare_normalized, normalize_field_key, normalize_field_value,
    normalize_field_value_with_currency,
};
pub use provenance::{
    CanonicalCluster, CanonicalSource, ProvenanceEdge, RedirectKind, canonical_edge,
    cluster_canonicals,
};
pub use temporal::{TemporalSemantics, parse_timestamp_ms};
pub use version::{
    VersionChangeKind, VersionDiffStats, VersionEdge, VersionRecord, build_version_chain,
    classify_version_change, etag_weak_match, if_none_match_matches, link_version_pair,
    normalize_etag_opaque,
};
