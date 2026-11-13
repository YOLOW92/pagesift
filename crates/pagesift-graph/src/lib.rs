#![forbid(unsafe_code)]

//! Lightweight cross-source provenance, citation, version lineage, evidence, and conflict graph.
//!
//! Designed as a deterministic streaming layer rather than a full knowledge-graph database:
//! heavy passes remain opt-in and release per-page text/DOM buffers immediately after ingestion.

pub mod builder;
pub mod citation;
pub mod conflict;
pub mod corroborate;
pub mod delta;
pub mod evidence;
mod evidence_json;
mod hash;
pub mod model;
pub mod norm;
mod norm_parse;
pub mod provenance;
pub mod version;

pub use builder::StreamingGraphBuilder;
pub use citation::{
    AnchorSample, CitationEdge, LinkRole, LinkSection, RawLinkObservation, aggregate_citations,
    classify_link_role, extract_markdown_links,
};
pub use conflict::{
    ConflictKind, ConflictRecord, ConflictSeverity, ConflictVariant, PreferredHint,
    detect_conflicts,
};
pub use corroborate::{
    ClaimRecord, FactCluster, SubjectKey, SubjectKeySource, build_fact_clusters,
    resolve_subject_key,
};
pub use delta::{DocumentInput, GraphSlice, GraphStats, GraphSummary, PageGraphDelta};
pub use evidence::{
    EvidenceAlignment, EvidenceSpan, ExtractionMethod, RawFieldTrace, align_traces,
    collect_json_traces,
};
pub use hash::{compact_excerpt, host_of, normalize_resource_key, normalize_url, sha256_hex};
pub use model::{
    BlockKind, BlockRef, DocNode, GraphLimits, GraphOptions, IndexedBlock, SourceKind,
    build_block_refs, build_indexed_blocks,
};
pub use norm::{
    DatePrecision, MoneyPeriod, NormalizedValue, ValueComparison, compare_normalized,
    normalize_field_key, normalize_field_value,
};
pub use provenance::{
    CanonicalCluster, CanonicalSource, ProvenanceEdge, RedirectKind, canonical_edge,
    cluster_canonicals,
};
pub use version::{
    VersionChangeKind, VersionDiffStats, VersionEdge, VersionRecord, build_version_chain,
    classify_version_change, link_version_pair,
};
