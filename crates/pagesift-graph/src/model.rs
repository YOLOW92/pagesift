//! Core graph primitives: options, limits, document nodes, and lightweight block references.

use serde::{Deserialize, Serialize};

use crate::hash::{
    compact_excerpt, host_of, normalize_resource_key, normalize_url, sha256_hex, short_id,
};

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
    /// Validate whether a declared canonical URL shares the fetched host and can anchor identity.
    pub fn is_trusted_canonical(canonical_url: &str, final_url: &str) -> bool {
        let trimmed = canonical_url.trim();
        if trimmed.is_empty() {
            return false;
        }
        let final_host = host_of(final_url);
        let can_host = host_of(trimmed);
        (!final_host.is_empty() && can_host == final_host)
            || normalize_url(trimmed) == normalize_url(final_url)
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

/// Structural role of a Markdown block within a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlockKind {
    Heading,
    Paragraph,
    ListItem,
    TableRow,
    Code,
    Quote,
    Caption,
    Other,
}

/// Compact coordinate into a parsed document's Markdown stream (omits full text to bound RAM).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockRef {
    pub block_id: String,
    pub doc_id: String,
    pub block_idx: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_idx: Option<usize>,
    pub kind: BlockKind,
    /// UTF-8 byte start offset in the document's Markdown string.
    pub start: usize,
    /// UTF-8 byte end offset in the document's Markdown string.
    pub end: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heading_path: Vec<String>,
    pub block_hash: String,
    pub excerpt: String,
}

/// Temporary per-page block metadata paired with raw block text for local alignment only.
#[derive(Debug, Clone)]
pub struct IndexedBlock {
    pub meta: BlockRef,
    pub text: String,
}

/// Parse Markdown into structural blocks without retaining body text across documents.
pub fn build_block_refs(
    doc_id: &str,
    markdown: &str,
    chunk_spans: &[(usize, usize, usize)],
    max_blocks: usize,
    max_excerpt_chars: usize,
) -> Vec<BlockRef> {
    build_indexed_blocks(doc_id, markdown, chunk_spans, max_blocks, max_excerpt_chars)
        .into_iter()
        .map(|b| b.meta)
        .collect()
}

/// Scan Markdown into structural blocks with transient full text for page-local evidence matching.
pub fn build_indexed_blocks(
    doc_id: &str,
    markdown: &str,
    chunk_spans: &[(usize, usize, usize)],
    max_blocks: usize,
    max_excerpt_chars: usize,
) -> Vec<IndexedBlock> {
    if markdown.is_empty() || max_blocks == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut heading_stack: Vec<(u8, String)> = Vec::new();
    let bytes = markdown.as_bytes();
    let mut pos = 0usize;

    while pos < bytes.len() && out.len() < max_blocks {
        let line_end = find_line_end(bytes, pos);
        let raw_line = &markdown[pos..line_end];
        let trimmed = raw_line.trim();
        let next_pos = skip_newline(bytes, line_end);

        if trimmed.is_empty() {
            pos = next_pos;
            continue;
        }

        if let Some((level, title)) = parse_atx_heading(trimmed) {
            while heading_stack.last().is_some_and(|(l, _)| *l >= level) {
                heading_stack.pop();
            }
            heading_stack.push((level, title.clone()));
            push_block(
                &mut out,
                doc_id,
                BlockKind::Heading,
                pos,
                line_end,
                &heading_stack,
                raw_line,
                chunk_spans,
                max_excerpt_chars,
            );
            pos = next_pos;
            continue;
        }

        if let Some(fence) = fence_prefix(trimmed) {
            let start = pos;
            let mut cur = next_pos;
            let mut end = line_end;
            while cur < bytes.len() {
                let le = find_line_end(bytes, cur);
                end = le;
                let line = markdown[cur..le].trim();
                cur = skip_newline(bytes, le);
                if line.starts_with(fence)
                    && line.trim_matches(fence.chars().next().unwrap()).is_empty()
                {
                    break;
                }
            }
            push_block(
                &mut out,
                doc_id,
                BlockKind::Code,
                start,
                end,
                &heading_stack,
                &markdown[start..end],
                chunk_spans,
                max_excerpt_chars,
            );
            pos = cur;
            continue;
        }

        if is_table_line(trimmed) {
            push_block(
                &mut out,
                doc_id,
                BlockKind::TableRow,
                pos,
                line_end,
                &heading_stack,
                raw_line,
                chunk_spans,
                max_excerpt_chars,
            );
            pos = next_pos;
            continue;
        }

        if trimmed.starts_with('>') {
            let (end, cur) = consume_while(markdown, pos, |t| t.starts_with('>'));
            push_block(
                &mut out,
                doc_id,
                BlockKind::Quote,
                pos,
                end,
                &heading_stack,
                &markdown[pos..end],
                chunk_spans,
                max_excerpt_chars,
            );
            pos = cur;
            continue;
        }

        if is_list_line(trimmed) {
            push_block(
                &mut out,
                doc_id,
                BlockKind::ListItem,
                pos,
                line_end,
                &heading_stack,
                raw_line,
                chunk_spans,
                max_excerpt_chars,
            );
            pos = next_pos;
            continue;
        }

        let (end, cur) = consume_paragraph(markdown, pos);
        push_block(
            &mut out,
            doc_id,
            BlockKind::Paragraph,
            pos,
            end,
            &heading_stack,
            &markdown[pos..end],
            chunk_spans,
            max_excerpt_chars,
        );
        pos = cur;
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn push_block(
    out: &mut Vec<IndexedBlock>,
    doc_id: &str,
    kind: BlockKind,
    start: usize,
    end: usize,
    headings: &[(u8, String)],
    raw_text: &str,
    chunk_spans: &[(usize, usize, usize)],
    max_excerpt_chars: usize,
) {
    let idx = out.len();
    let chunk_idx = chunk_spans
        .iter()
        .find(|(_, cs, ce)| start < *ce && *cs < end)
        .map(|(ci, _, _)| *ci);
    let block_hash = sha256_hex(raw_text.as_bytes());
    out.push(IndexedBlock {
        meta: BlockRef {
            block_id: format!("{doc_id}#b{idx}"),
            doc_id: doc_id.to_owned(),
            block_idx: idx,
            chunk_idx,
            kind,
            start,
            end,
            heading_path: headings.iter().map(|(_, h)| h.clone()).collect(),
            block_hash,
            excerpt: compact_excerpt(raw_text, max_excerpt_chars),
        },
        text: raw_text.to_owned(),
    });
}

fn find_line_end(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
        i += 1;
    }
    i
}

fn skip_newline(bytes: &[u8], line_end: usize) -> usize {
    if line_end >= bytes.len() {
        return line_end;
    }
    if bytes[line_end] == b'\r' && bytes.get(line_end + 1) == Some(&b'\n') {
        line_end + 2
    } else {
        line_end + 1
    }
}

fn parse_atx_heading(trimmed: &str) -> Option<(u8, String)> {
    const MAX_HEADING_LEVEL: usize = 6; // [协议] CommonMark/HTML 标题层级 h1..h6
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if !(1..=MAX_HEADING_LEVEL).contains(&hashes) {
        return None;
    }
    let rest = trimmed[hashes..].strip_prefix(char::is_whitespace)?;
    let title = rest.trim().trim_end_matches('#').trim();
    (!title.is_empty()).then(|| (hashes as u8, title.to_owned()))
}

fn fence_prefix(trimmed: &str) -> Option<&'static str> {
    if trimmed.starts_with("```") {
        Some("```")
    } else if trimmed.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

fn is_table_line(trimmed: &str) -> bool {
    trimmed.starts_with('|') && trimmed.matches('|').count() >= 2
}

fn is_list_line(trimmed: &str) -> bool {
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        return true;
    }
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && (trimmed[digits..].starts_with(". ") || trimmed[digits..].starts_with(") "))
}

fn consume_while(markdown: &str, start: usize, pred: impl Fn(&str) -> bool) -> (usize, usize) {
    let bytes = markdown.as_bytes();
    let mut cur = start;
    let mut last_end = start;
    while cur < bytes.len() {
        let le = find_line_end(bytes, cur);
        let t = markdown[cur..le].trim();
        if t.is_empty() || !pred(t) {
            break;
        }
        last_end = le;
        cur = skip_newline(bytes, le);
    }
    (last_end, cur)
}

fn consume_paragraph(markdown: &str, start: usize) -> (usize, usize) {
    let bytes = markdown.as_bytes();
    let mut cur = start;
    let mut last_end = start;
    while cur < bytes.len() {
        let le = find_line_end(bytes, cur);
        let t = markdown[cur..le].trim();
        if t.is_empty()
            || (cur > start
                && (parse_atx_heading(t).is_some()
                    || fence_prefix(t).is_some()
                    || is_table_line(t)
                    || is_list_line(t)
                    || t.starts_with('>')))
        {
            break;
        }
        last_end = le;
        cur = skip_newline(bytes, le);
    }
    (last_end, cur)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_headings_tables_and_chunk_alignment() {
        let md = "# Pricing\n\nStandard plan costs $49/mo.\n\n| Tier | Price |\n| --- | --- |\n| Pro | $99/mo |\n";
        let chunks = [(0, 0, md.len())];
        let blocks = build_indexed_blocks("doc_1", md, &chunks, 64, 120);
        assert_eq!(blocks.len(), 5);
        assert_eq!(blocks[0].meta.kind, BlockKind::Heading);
        assert_eq!(blocks[1].meta.kind, BlockKind::Paragraph);
        assert_eq!(blocks[1].meta.heading_path, ["Pricing"]);
        assert_eq!(blocks[1].meta.chunk_idx, Some(0));
        assert_eq!(blocks[4].meta.kind, BlockKind::TableRow);
        assert!(blocks[4].text.contains("$99/mo"));
    }
}
