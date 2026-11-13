//! Input and output DTOs for streaming DataGraph deltas, per-page slices, and job summaries.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::citation::{CitationEdge, RawLinkObservation};
use crate::conflict::ConflictRecord;
use crate::corroborate::{ClaimRecord, FactCluster, SubjectKey};
use crate::evidence::{EvidenceSpan, RawFieldTrace};
use crate::model::{BlockRef, DocNode, SourceKind};
use crate::provenance::{CanonicalCluster, ProvenanceEdge, RedirectKind};
use crate::version::{VersionDiffStats, VersionEdge, VersionRecord};

/// One processed document's inputs handed to [`crate::builder::StreamingGraphBuilder::ingest`].
#[derive(Debug, Clone)]
pub struct DocumentInput {
    pub requested_url: String,
    pub final_url: String,
    pub canonical_url: Option<String>,
    pub canonical_from_header: bool,
    pub source_kind: SourceKind,
    pub observed_at_ms: u64,
    pub status_code: u16,
    pub engine: String,
    pub content_hash: Option<String>,
    pub raw_hash: Option<String>,
    pub text_hash: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub title: Option<String>,
    pub markdown: String,
    /// `(from_url, to_url, status, kind)`.
    pub redirect_chain: Vec<(String, String, u16, RedirectKind)>,
    pub links: Vec<RawLinkObservation>,
    /// `(alt_url, hreflang, media_type)`.
    pub alternates: Vec<(String, Option<String>, Option<String>)>,
    pub duplicate_of: Option<String>,
    /// `(attachment_url, media_id, member_path)`.
    pub attachments: Vec<(String, Option<String>, Option<String>)>,
    pub structured_json_ld: Vec<Value>,
    pub extracted_fields: Option<Value>,
    pub raw_traces: Vec<RawFieldTrace>,
    /// Optional DocChunk offsets `(chunk_idx, start_byte, end_byte)` aligned to `markdown`.
    pub chunk_spans: Vec<(usize, usize, usize)>,
    /// Optional predecessor version snapshot supplied from persistent history (Store).
    pub previous_version: Option<VersionRecord>,
    pub version_diff_stats: Option<VersionDiffStats>,
    pub changed_block_ids: Vec<String>,
}

/// Compact incremental graph output emitted for a single ingested document (`graph.jsonl`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageGraphDelta {
    pub node: DocNode,
    pub subject_key: SubjectKey,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<BlockRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<CitationEdge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<ProvenanceEdge>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<VersionEdge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceSpan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<ClaimRecord>,
}

/// Per-document DataGraph slice optionally attached to API `Document.graph`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphSlice {
    pub doc_id: String,
    pub resource_key: String,
    pub source_kind: SourceKind,
    pub observed_at_ms: u64,
    pub subject_key: SubjectKey,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<BlockRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<CitationEdge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<ProvenanceEdge>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<VersionEdge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceSpan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<ClaimRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_deltas: Vec<PageGraphDelta>,
}

impl From<PageGraphDelta> for GraphSlice {
    fn from(d: PageGraphDelta) -> Self {
        Self {
            doc_id: d.node.doc_id,
            resource_key: d.node.resource_key,
            source_kind: d.node.source_kind,
            observed_at_ms: d.node.observed_at_ms,
            subject_key: d.subject_key,
            blocks: d.blocks,
            citations: d.citations,
            provenance: d.provenance,
            version: d.version,
            evidence: d.evidence,
            claims: d.claims,
            member_deltas: Vec::new(),
        }
    }
}

/// Summary counters for an assembled crawl/job DataGraph.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphStats {
    pub nodes: usize,
    pub blocks: usize,
    pub citations: usize,
    pub provenance_edges: usize,
    pub version_edges: usize,
    pub evidence_spans: usize,
    pub claims: usize,
    pub fact_clusters: usize,
    pub conflicts: usize,
    pub canonical_clusters: usize,
}

/// Complete job-level DataGraph summary (`graph_summary.json` and `GET /v2/crawl/{id}/graph`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphSummary {
    pub stats: GraphStats,
    pub nodes: Vec<DocNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub canonical_clusters: Vec<CanonicalCluster>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<CitationEdge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<ProvenanceEdge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<VersionEdge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fact_clusters: Vec<FactCluster>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<ConflictRecord>,
}
