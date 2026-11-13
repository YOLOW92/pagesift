//! Field-level extraction provenance and three-tier deterministic alignment to content blocks.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::hash::compact_excerpt;
use crate::model::{BlockKind, IndexedBlock};

/// Extraction mechanism that produced a structured field value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ExtractionMethod {
    CssSelector {
        selector: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        relocated_from: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        step: Option<String>,
    },
    XPathSelector {
        selector: String,
    },
    RegexPattern {
        pattern: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    JsonLd {
        #[serde(skip_serializing_if = "Option::is_none")]
        entity_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        entity_id: Option<String>,
    },
    Microdata {
        #[serde(skip_serializing_if = "Option::is_none")]
        itemtype: Option<String>,
    },
    OpenGraph {
        property: String,
    },
    Agent {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        cited_evidence_ids: Vec<String>,
    },
}

impl ExtractionMethod {
    pub fn is_metadata_island(&self) -> bool {
        matches!(
            self,
            Self::JsonLd { .. } | Self::Microdata { .. } | Self::OpenGraph { .. }
        )
    }
}

/// Raw pre-alignment trace emitted by `pagesift-extract` extractors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawFieldTrace {
    pub field_path: String,
    pub field_name: String,
    pub value: Value,
    pub raw_quote: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attr_name: Option<String>,
    pub method: ExtractionMethod,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    pub visible_in_dom: bool,
}

/// Alignment quality tier linking a field trace back to a block coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EvidenceAlignment {
    ExactBlock,
    MetadataIsland,
    TokenFuzzy,
    Unaligned,
}

/// Grounded evidence pointer connecting a structured claim back to its source block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceSpan {
    pub doc_id: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_idx: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<usize>,
    pub excerpt: String,
    pub confidence: f32,
    pub alignment: EvidenceAlignment,
    pub verified: bool,
    pub field_path: String,
    pub method: ExtractionMethod,
}

/// Align raw extractor traces onto transient single-page `IndexedBlock` entries.
pub fn align_traces(
    doc_id: &str,
    url: &str,
    traces: &[RawFieldTrace],
    blocks: &[IndexedBlock],
) -> Vec<EvidenceSpan> {
    let prepared: Vec<PreparedBlock> = blocks.iter().map(PreparedBlock::new).collect();
    traces
        .iter()
        .map(|trace| align_one(doc_id, url, trace, &prepared))
        .collect()
}

struct PreparedBlock<'a> {
    block: &'a IndexedBlock,
    norm_text: String,
    padded_tokens: String,
    token_set: HashSet<String>,
    heading_lower: String,
}

impl<'a> PreparedBlock<'a> {
    fn new(block: &'a IndexedBlock) -> Self {
        let norm_text = normalize_for_match(&block.text);
        let tokens = tokenize_words(&norm_text);
        let padded_tokens = format!(" {} ", tokens.join(" "));
        let token_set: HashSet<String> = tokens.into_iter().collect();
        let heading_lower = block
            .meta
            .heading_path
            .iter()
            .map(|h| h.to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            block,
            norm_text,
            padded_tokens,
            token_set,
            heading_lower,
        }
    }
}

fn align_one(
    doc_id: &str,
    url: &str,
    trace: &RawFieldTrace,
    blocks: &[PreparedBlock<'_>],
) -> EvidenceSpan {
    let needle_raw = value_needle(trace);
    let needle = normalize_for_match(&needle_raw);
    let needle_tokens = tokenize_words(&needle);

    if trace.visible_in_dom
        && !needle_tokens.is_empty()
        && let Some(hit) = best_exact_block(trace, &needle, &needle_tokens, blocks)
    {
        return span_from_block(
            doc_id,
            url,
            trace,
            hit.block,
            1.0,
            EvidenceAlignment::ExactBlock,
            true,
        );
    }

    if trace.method.is_metadata_island() || !trace.visible_in_dom {
        let body_hit = if !needle_tokens.is_empty() {
            best_exact_block(trace, &needle, &needle_tokens, blocks)
        } else {
            None
        };
        if let Some(hit) = body_hit {
            return span_from_block(
                doc_id,
                url,
                trace,
                hit.block,
                0.95, // [策略] JSON-LD 主体命中正文块的高置信度
                EvidenceAlignment::MetadataIsland,
                true,
            );
        }
        return EvidenceSpan {
            doc_id: doc_id.to_owned(),
            url: trace.source_url.clone().unwrap_or_else(|| url.to_owned()),
            block_id: None,
            chunk_idx: None,
            start: None,
            end: None,
            excerpt: compact_excerpt(&trace.raw_quote, 160), // [策略] 未对齐证据预览最大字符数 // [策略] 元数据岛证据预览最大字符数
            confidence: 0.85, // [策略] 结构化元数据岛未匹配正文时的初始置信度
            alignment: EvidenceAlignment::MetadataIsland,
            verified: false,
            field_path: trace.field_path.clone(),
            method: trace.method.clone(),
        };
    }

    if needle_tokens.len() >= 2
        && let Some((hit, score)) = best_fuzzy_block(&needle_tokens, blocks)
    {
        let confidence = (0.50 + 0.30 * score).clamp(0.50, 0.80); // [策略] 模糊块匹配置信度区间 0.50..=0.80
        return span_from_block(
            doc_id,
            url,
            trace,
            hit.block,
            confidence,
            EvidenceAlignment::TokenFuzzy,
            false,
        );
    }

    EvidenceSpan {
        doc_id: doc_id.to_owned(),
        url: trace.source_url.clone().unwrap_or_else(|| url.to_owned()),
        block_id: None,
        chunk_idx: None,
        start: None,
        end: None,
        excerpt: compact_excerpt(&trace.raw_quote, 160), // [策略] 未对齐证据预览最大字符数
        confidence: 0.0,
        alignment: EvidenceAlignment::Unaligned,
        verified: false,
        field_path: trace.field_path.clone(),
        method: trace.method.clone(),
    }
}

fn span_from_block(
    doc_id: &str,
    url: &str,
    trace: &RawFieldTrace,
    block: &IndexedBlock,
    confidence: f32,
    alignment: EvidenceAlignment,
    verified: bool,
) -> EvidenceSpan {
    EvidenceSpan {
        doc_id: doc_id.to_owned(),
        url: trace.source_url.clone().unwrap_or_else(|| url.to_owned()),
        block_id: Some(block.meta.block_id.clone()),
        chunk_idx: block.meta.chunk_idx,
        start: Some(block.meta.start),
        end: Some(block.meta.end),
        excerpt: block.meta.excerpt.clone(),
        confidence,
        alignment,
        verified,
        field_path: trace.field_path.clone(),
        method: trace.method.clone(),
    }
}

fn best_exact_block<'a>(
    trace: &RawFieldTrace,
    needle: &str,
    needle_tokens: &[String],
    blocks: &'a [PreparedBlock<'a>],
) -> Option<&'a PreparedBlock<'a>> {
    let token_phrase = format!(" {} ", needle_tokens.join(" "));
    let field_lower = trace.field_name.to_ascii_lowercase();
    let has_cjk = needle.chars().any(is_cjk_char);

    let mut best: Option<(usize, &'a PreparedBlock<'a>)> = None;
    for b in blocks {
        let matched = b.padded_tokens.contains(&token_phrase)
            || (has_cjk && !needle.trim().is_empty() && b.norm_text.contains(needle.trim()));
        if !matched {
            continue;
        }
        let mut rank = match b.block.meta.kind {
            BlockKind::TableRow | BlockKind::Paragraph | BlockKind::ListItem => 10, // [策略] 主体内容块优先权重
            BlockKind::Heading => 6, // [策略] 标题块次优权重
            _ => 4,                  // [策略] 其他块基础权重
        };
        if !field_lower.is_empty()
            && (b.norm_text.contains(&field_lower) || b.heading_lower.contains(&field_lower))
        {
            rank += 5; // [策略] 字段名同块或同标题命中加权
        }
        if best.is_none_or(|(best_rank, _)| rank > best_rank) {
            best = Some((rank, b));
        }
    }
    best.map(|(_, b)| b)
}

fn best_fuzzy_block<'a>(
    needle_tokens: &[String],
    blocks: &'a [PreparedBlock<'a>],
) -> Option<(&'a PreparedBlock<'a>, f32)> {
    let mut best: Option<(f32, &'a PreparedBlock<'a>)> = None;
    for b in blocks {
        let hits = needle_tokens
            .iter()
            .filter(|t| b.token_set.contains(t.as_str()))
            .count();
        if hits == 0 {
            continue;
        }
        const MIN_FUZZY_CONTAINMENT: f32 = 0.45; // [策略] 模糊匹配最小词覆盖率阈值
        let containment = hits as f32 / needle_tokens.len() as f32;
        if containment >= MIN_FUZZY_CONTAINMENT && best.is_none_or(|(s, _)| containment > s) {
            best = Some((containment, b));
        }
    }
    best.map(|(s, b)| (b, s))
}

pub use crate::evidence_json::collect_json_traces;
use crate::evidence_json::scalar_quote;

fn value_needle(trace: &RawFieldTrace) -> String {
    if !trace.raw_quote.trim().is_empty() {
        return trace.raw_quote.clone();
    }
    scalar_quote(&trace.value)
}

/// Remove thousand-separator commas between digits and fold ASCII case.
pub(crate) fn normalize_for_match(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    for (i, &c) in chars.iter().enumerate() {
        let is_thousands_comma = c == ','
            && i > 0
            && chars[i - 1].is_ascii_digit()
            && chars.get(i + 1).is_some_and(char::is_ascii_digit);
        if is_thousands_comma {
            continue;
        }
        for lc in c.to_lowercase() {
            out.push(lc);
        }
    }
    out
}

fn tokenize_words(norm: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut buf = String::new();
    for c in norm.chars() {
        if c.is_alphanumeric() {
            buf.push(c);
        } else if !buf.is_empty() {
            tokens.push(std::mem::take(&mut buf));
        }
    }
    if !buf.is_empty() {
        tokens.push(buf);
    }
    tokens
}

fn is_cjk_char(c: char) -> bool {
    matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x3040..=0x30FF | 0xAC00..=0xD7AF) // [协议] Unicode CJK/假名/谚文码位区间
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::build_indexed_blocks;
    use serde_json::json;

    #[test]
    fn aligns_exact_metadata_island_and_whole_word_boundaries() {
        let md = "# Plans\n\nPro tier costs $1,234/yr and serves Room 312.\n";
        let blocks = build_indexed_blocks("doc_1", md, &[], 64, 160);
        let traces = vec![
            RawFieldTrace {
                field_path: "/price".into(),
                field_name: "price".into(),
                value: json!("$1234/yr"),
                raw_quote: "$1234/yr".into(),
                attr_name: None,
                method: ExtractionMethod::CssSelector {
                    selector: ".price".into(),
                    relocated_from: None,
                    step: None,
                },
                source_url: None,
                visible_in_dom: true,
            },
            RawFieldTrace {
                field_path: "/room_false".into(),
                field_name: "room".into(),
                value: json!(12),
                raw_quote: "12".into(),
                attr_name: None,
                method: ExtractionMethod::CssSelector {
                    selector: ".room".into(),
                    relocated_from: None,
                    step: None,
                },
                source_url: None,
                visible_in_dom: true,
            },
            RawFieldTrace {
                field_path: "/sku".into(),
                field_name: "sku".into(),
                value: json!("SKU-PRO-99"),
                raw_quote: "SKU-PRO-99".into(),
                attr_name: Some("content".into()),
                method: ExtractionMethod::JsonLd {
                    entity_type: Some("Product".into()),
                    entity_id: None,
                },
                source_url: None,
                visible_in_dom: false,
            },
        ];
        let spans = align_traces("doc_1", "https://example.com", &traces, &blocks);
        assert_eq!(spans[0].alignment, EvidenceAlignment::ExactBlock);
        assert!(spans[0].verified);
        assert_eq!(spans[0].block_id.as_deref(), Some("doc_1#b1"));

        assert_eq!(spans[1].alignment, EvidenceAlignment::Unaligned);
        assert!(!spans[1].verified);

        assert_eq!(spans[2].alignment, EvidenceAlignment::MetadataIsland);
        assert_eq!(spans[2].confidence, 0.85);
    }
}
