//! Stable knowledge unit model separating logical identity, content revision,
//! physical source location, and cross-version lineage/uncertainty.

use serde::{Deserialize, Serialize};

use crate::corroborate::SubjectKey;
use crate::evidence::EvidenceSpan;
use crate::hash::short_id;
use crate::model::SourceKind;
use crate::temporal::TemporalSemantics;

/// Structural category of a [`KnowledgeUnit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UnitKind {
    SectionHeading,
    Paragraph,
    Table,
    CodeBlock,
    ListGroup,
    Quote,
    FigureCaption,
    ClaimFact,
}

impl UnitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SectionHeading => "sectionHeading",
            Self::Paragraph => "paragraph",
            Self::Table => "table",
            Self::CodeBlock => "codeBlock",
            Self::ListGroup => "listGroup",
            Self::Quote => "quote",
            Self::FigureCaption => "figureCaption",
            Self::ClaimFact => "claimFact",
        }
    }
}

/// Preserved table header, caption, and shape metadata so table slices never lose column semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableStructureMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<String>,
    pub row_count: usize,
    pub col_count: usize,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_slice: bool,
}

/// Observation-specific physical coordinates of a [`KnowledgeUnit`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitLocation {
    pub doc_id: String,
    pub resource_key: String,
    pub source_url: String,
    pub source_kind: SourceKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub block_ids: Vec<String>,
    pub byte_start: usize,
    pub byte_end: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heading_path: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table_meta: Option<TableStructureMeta>,
}

impl UnitLocation {
    /// Deterministic digest of physical placement and structural path (decoupled from body text).
    pub fn location_hash(&self) -> String {
        let range = format!("{}:{}", self.byte_start, self.byte_end);
        let page = match self.page_number {
            Some(p) => format!("p:{p}"),
            None => String::new(),
        };
        let headings = self.heading_path.join("\u{1f}");
        let blocks = self.block_ids.join("\u{1f}");
        let lang = self.code_language.as_deref().unwrap_or_default();
        let tbl_sig = match &self.table_meta {
            Some(tm) => format!(
                "{}:{}:{}:{}\u{1f}{}",
                tm.row_count,
                tm.col_count,
                u8::from(tm.is_slice),
                tm.caption.as_deref().unwrap_or_default(),
                tm.headers.join("\u{1f}")
            ),
            None => String::new(),
        };
        short_id(
            "uloc",
            &[
                &self.doc_id,
                &self.resource_key,
                &range,
                &page,
                &headings,
                &blocks,
                lang,
                &tbl_sig,
            ],
        )
    }
}

/// Cross-version evolution relation for a [`KnowledgeUnit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UnitLineageKind {
    Created,
    Unchanged,
    Relocated,
    Rewritten,
    SplitFrom,
    MergedFrom,
    Ambiguous,
}

/// Competing candidate recorded when cross-version identity cannot be uniquely resolved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AmbiguousCandidate {
    pub unit_id: String,
    pub score: f32,
    pub reason: String,
}

/// Explicit lineage and uncertainty record across document versions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitLineage {
    pub relation: UnitLineageKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_unit_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_revision_id: Option<String>,
    pub confidence: f32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub ambiguous: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<AmbiguousCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl UnitLineage {
    pub fn created() -> Self {
        Self {
            relation: UnitLineageKind::Created,
            parent_unit_ids: Vec::new(),
            prev_revision_id: None,
            confidence: 1.0,
            ambiguous: false,
            candidates: Vec::new(),
            notes: None,
        }
    }
}

/// Lifecycle state of a [`KnowledgeUnit`], aligned with 1.5 DataGraph/Loop semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum UnitStatus {
    Active,
    Unverified {
        #[serde(rename = "sinceMs")]
        since_ms: u64,
        reason: String,
    },
    Withdrawn {
        #[serde(rename = "atMs")]
        at_ms: u64,
        reason: String,
    },
    Expired {
        #[serde(rename = "validUntil")]
        valid_until: String,
    },
    Superseded {
        #[serde(rename = "byRevisionId")]
        by_revision_id: String,
        #[serde(rename = "atMs")]
        at_ms: u64,
    },
}

impl UnitStatus {
    pub fn is_searchable_default(&self) -> bool {
        matches!(self, Self::Active | Self::Unverified { .. })
    }

    pub fn is_verified_active(&self) -> bool {
        matches!(self, Self::Active)
    }

    pub fn tag(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Unverified { .. } => "unverified",
            Self::Withdrawn { .. } => "withdrawn",
            Self::Expired { .. } => "expired",
            Self::Superseded { .. } => "superseded",
        }
    }
}

/// Evidence-grounded knowledge unit with stable logical identity across document revisions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeUnit {
    /// Stable logical identity (`unt_<16hex>`) preserved across edits and relocations when grounded.
    pub unit_id: String,
    /// Revision-specific state digest (`urev_<16hex>`) reflecting current content, claims, and validity.
    pub revision_id: String,
    /// Normalized content digest (`uch_<16hex>` or BLAKE3/SHA-256 hex).
    pub content_hash: String,
    pub kind: UnitKind,
    pub text: String,
    pub tokens: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_key: Option<SubjectKey>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entity_anchors: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_negation_or_qualifier: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_units_or_currency: bool,
    pub location: UnitLocation,
    pub lineage: UnitLineage,
    pub status: UnitStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporal: Option<TemporalSemantics>,
    pub origin_root: String,
    #[serde(default = "default_true")]
    pub origin_independent: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claim_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_spans: Vec<EvidenceSpan>,
    pub observed_at_ms: u64,
}

impl KnowledgeUnit {
    /// Mint a deterministic initial logical unit ID when a knowledge unit first appears.
    pub fn mint_unit_id(
        resource_key: &str,
        kind: UnitKind,
        structural_anchor: &str,
        content_hash: &str,
        occurrence: usize,
    ) -> String {
        let occ = occurrence.to_string();
        short_id(
            "unt",
            &[
                resource_key,
                kind.as_str(),
                structural_anchor,
                content_hash,
                &occ,
            ],
        )
    }

    /// Compute deterministic revision identity from logical ID, content hash, claims, and validity.
    pub fn compute_revision_id(
        unit_id: &str,
        content_hash: &str,
        claim_ids: &[String],
        temporal: Option<&TemporalSemantics>,
    ) -> String {
        let claims_joined = claim_ids.join("\u{1f}");
        let valid_from = temporal
            .and_then(|t| t.valid_from.as_deref())
            .unwrap_or("\u{0}");
        let valid_until = temporal
            .and_then(|t| t.valid_until.as_deref())
            .unwrap_or("\u{0}");
        short_id(
            "urev",
            &[
                unit_id,
                content_hash,
                &claims_joined,
                valid_from,
                valid_until,
            ],
        )
    }
}

fn is_false(v: &bool) -> bool {
    !*v
}

fn default_true() -> bool {
    true
}
