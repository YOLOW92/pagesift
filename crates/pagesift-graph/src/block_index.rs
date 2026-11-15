//! Markdown structural block scanner and compact block coordinate references.

use serde::{Deserialize, Serialize};

use crate::hash::{compact_excerpt, sha256_hex};

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
