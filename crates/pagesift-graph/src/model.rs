//! Core graph primitives: options, limits, document nodes, and lightweight block references.

use serde::{Deserialize, Serialize};

pub use crate::block_index::{
    BlockKind, BlockRef, IndexedBlock, build_block_refs, build_indexed_blocks,
    build_indexed_blocks_checked,
};
use crate::hash::{normalize_resource_key, normalize_url, short_id};

/// Hard bounds keeping streaming DataGraph memory and per-document fan-out predictable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GraphLimits {
    pub max_edges_per_doc: usize,
    pub max_anchor_samples: usize,
    pub max_claims_per_bucket: usize,
    pub max_blocks_per_doc: usize,
    pub max_excerpt_chars: usize,
    pub max_nodes: usize,
    pub max_citations: usize,
    pub max_provenance_edges: usize,
    pub max_versions: usize,
    pub max_claim_buckets: usize,
    pub max_claims: usize,
}

impl GraphLimits {
    pub const HARD_MAX_EDGES_PER_DOC: usize = 4_096; // [策略] 单文档引用边硬上限
    pub const HARD_MAX_ANCHOR_SAMPLES: usize = 32; // [策略] 单条引用边保留的锚文本样本硬上限
    pub const HARD_MAX_CLAIMS_PER_BUCKET: usize = 512; // [策略] 单事实聚类桶内保留的声明硬上限
    pub const HARD_MAX_BLOCKS_PER_DOC: usize = 32_768; // [策略] 单文档结构化块硬上限
    pub const HARD_MAX_EXCERPT_CHARS: usize = 4_096; // [策略] 块摘录最大字符数硬上限
    pub const HARD_MAX_NODES: usize = 100_000; // [策略] 单次爬取图谱聚合的文档节点硬上限
    pub const HARD_MAX_CITATIONS: usize = 1_000_000; // [策略] 单次爬取图谱聚合的引用边硬上限
    pub const HARD_MAX_PROVENANCE_EDGES: usize = 500_000; // [策略] 单次爬取图谱聚合的来源归属边硬上限
    pub const HARD_MAX_VERSIONS: usize = 200_000; // [策略] 单次爬取图谱聚合的版本边硬上限
    pub const HARD_MAX_CLAIM_BUCKETS: usize = 200_000; // [策略] 单次爬取图谱聚合的事实桶硬上限
    pub const HARD_MAX_CLAIMS: usize = 1_000_000; // [策略] 单次爬取图谱聚合的总声明数硬上限
    pub const MIN_EXCERPT_CHARS: usize = 16; // [策略] 块摘录最小字符数下限

    /// Clamps all limit fields to safe `[1, HARD_MAX_*]` bounds.
    pub fn clamped(&self) -> Self {
        Self {
            max_edges_per_doc: self
                .max_edges_per_doc
                .clamp(1, Self::HARD_MAX_EDGES_PER_DOC),
            max_anchor_samples: self
                .max_anchor_samples
                .clamp(1, Self::HARD_MAX_ANCHOR_SAMPLES),
            max_claims_per_bucket: self
                .max_claims_per_bucket
                .clamp(1, Self::HARD_MAX_CLAIMS_PER_BUCKET),
            max_blocks_per_doc: self
                .max_blocks_per_doc
                .clamp(1, Self::HARD_MAX_BLOCKS_PER_DOC),
            max_excerpt_chars: self
                .max_excerpt_chars
                .clamp(Self::MIN_EXCERPT_CHARS, Self::HARD_MAX_EXCERPT_CHARS),
            max_nodes: self.max_nodes.clamp(1, Self::HARD_MAX_NODES),
            max_citations: self.max_citations.clamp(1, Self::HARD_MAX_CITATIONS),
            max_provenance_edges: self
                .max_provenance_edges
                .clamp(1, Self::HARD_MAX_PROVENANCE_EDGES),
            max_versions: self.max_versions.clamp(1, Self::HARD_MAX_VERSIONS),
            max_claim_buckets: self
                .max_claim_buckets
                .clamp(1, Self::HARD_MAX_CLAIM_BUCKETS),
            max_claims: self.max_claims.clamp(1, Self::HARD_MAX_CLAIMS),
        }
    }
}

impl Default for GraphLimits {
    fn default() -> Self {
        Self {
            max_edges_per_doc: 256,        // [策略] 单文档默认最大边数
            max_anchor_samples: 3,         // [策略] 单引用边默认保留锚文本样本数
            max_claims_per_bucket: 64,     // [策略] 单分桶默认声明上限
            max_blocks_per_doc: 4096,      // [策略] 单文档默认索引块上限
            max_excerpt_chars: 160,        // [策略] 默认块摘要截断字符数
            max_nodes: 50_000,             // [策略] 单任务默认节点数上限
            max_citations: 500_000,        // [策略] 单任务默认引用边上限
            max_provenance_edges: 250_000, // [策略] 单任务默认溯源边上限
            max_versions: 100_000,         // [策略] 单任务默认版本边上限
            max_claim_buckets: 100_000,    // [策略] 单任务默认声明分桶上限
            max_claims: 500_000,           // [策略] 单任务默认声明总数上限
        }
    }
}

fn default_true() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

/// Opt-in configuration for building cross-source DataGraph outputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GraphOptions {
    pub enabled: bool,
    pub blocks: bool,
    pub citations: bool,
    pub provenance: bool,
    pub versions: bool,
    pub evidence: bool,
    pub facts: bool,
    pub conflicts: bool,
    #[serde(default)]
    pub subject_keys: Vec<String>,
    pub limits: GraphLimits,
    /// When `false`, build slices and version diffs in-memory without persisting into `Store`.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub persist: bool,
}

impl Default for GraphOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            blocks: true,
            citations: true,
            provenance: true,
            versions: true,
            evidence: true,
            facts: true,
            conflicts: true,
            subject_keys: Vec::new(),
            limits: GraphLimits::default(),
            persist: true,
        }
    }
}

impl GraphOptions {
    /// Preset with all DataGraph pipelines enabled and default bounded limits.
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }

    /// Return a copy that builds graph slices in-memory without mutating persistent `Store`.
    pub fn as_attempt(&self) -> Self {
        let mut copy = self.clone();
        copy.persist = false;
        copy
    }
}

/// Underlying document / artifact container kind producing a node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SourceKind {
    Html,
    Pdf,
    Office {
        format: String,
    },
    ArchiveMember {
        archive_url: String,
        member_path: String,
    },
    EmailPart {
        mail_url: String,
        part_name: String,
    },
    ImageAsset {
        parent_url: String,
        media_id: String,
    },
    Feed,
    Api,
    Other {
        label: String,
    },
}

impl SourceKind {
    /// Short lowercase format label used in distinct format sets and conflict summaries.
    pub fn format_label(&self) -> String {
        match self {
            Self::Html => "html".into(),
            Self::Pdf => "pdf".into(),
            Self::Office { format } => format.to_ascii_lowercase(),
            Self::ArchiveMember { member_path, .. } => member_path
                .rsplit('.')
                .next()
                .map(|ext| format!("archive:{ext}"))
                .unwrap_or_else(|| "archive".into()),
            Self::EmailPart { .. } => "email".into(),
            Self::ImageAsset { .. } => "image".into(),
            Self::Feed => "feed".into(),
            Self::Api => "api".into(),
            Self::Other { label } => label.to_ascii_lowercase(),
        }
    }
}

/// One observed document, parsed upload, or extracted archive/mail/media unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocNode {
    pub doc_id: String,
    pub resource_key: String,
    pub requested_url: String,
    pub final_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    pub source_kind: SourceKind,
    pub observed_at_ms: u64,
    pub status_code: u16,
    pub engine: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl DocNode {
    /// Validate whether a declared canonical URL matches the fetched origin and URL identity.
    ///
    /// Uncorroborated cross-path declarations on the same host (or cross-port / cross-scheme
    /// origins) are recorded in provenance only and never rewrite `resource_key` or `doc_id`.
    pub fn is_trusted_canonical(canonical_url: &str, final_url: &str) -> bool {
        let trimmed = canonical_url.trim();
        if trimmed.is_empty() {
            return false;
        }
        crate::hash::same_origin(trimmed, final_url)
            && normalize_resource_key(trimmed) == normalize_resource_key(final_url)
    }

    /// Return normalized canonical URL when non-empty and same-host trusted for identity.
    pub fn trusted_canonical_url(canonical_url: Option<&str>, final_url: &str) -> Option<String> {
        let can = canonical_url.map(str::trim).filter(|c| !c.is_empty())?;
        Self::is_trusted_canonical(can, final_url).then(|| normalize_url(can))
    }

    /// Unified logical URL: trusted same-host canonical URL if present, else normalized `final_url`.
    pub fn logical_url_for(canonical_url: Option<&str>, final_url: &str) -> String {
        Self::trusted_canonical_url(canonical_url, final_url)
            .unwrap_or_else(|| normalize_url(final_url))
    }

    /// Deterministic observation id from canonical/final URL, timestamp, and content hash.
    pub fn compute_doc_id(
        canonical_or_final_url: &str,
        observed_at_ms: u64,
        content_hash: Option<&str>,
    ) -> String {
        let norm = normalize_url(canonical_or_final_url);
        let ts = observed_at_ms.to_string();
        short_id("doc", &[&norm, &ts, content_hash.unwrap_or("")])
    }

    /// Deterministic observation id unified with [`crate::builder::StreamingGraphBuilder::ingest`].
    pub fn compute_observation_doc_id(
        canonical_url: Option<&str>,
        final_url: &str,
        observed_at_ms: u64,
        content_hash: Option<&str>,
        raw_hash: Option<&str>,
    ) -> String {
        let logical = Self::logical_url_for(canonical_url, final_url);
        Self::compute_doc_id(&logical, observed_at_ms, content_hash.or(raw_hash))
    }

    /// Best logical key: same-host trusted canonical URL when present, otherwise final URL.
    pub fn logical_url(&self) -> &str {
        self.canonical_url
            .as_deref()
            .filter(|c| Self::is_trusted_canonical(c, &self.final_url))
            .unwrap_or(&self.final_url)
    }

    /// Compute normalized `resource_key` from same-host trusted canonical or final URL.
    pub fn derive_resource_key(canonical_url: Option<&str>, final_url: &str) -> String {
        normalize_resource_key(&Self::logical_url_for(canonical_url, final_url))
    }
}
