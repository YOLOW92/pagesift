//! Evidence span alignment and block-remap helpers after HTML/markdown transforms.

use std::collections::HashMap;

use pagesift_api::types::DocChunk;
use pagesift_graph::{EvidenceAlignment, EvidenceSpan, IndexedBlock};
use serde_json::Value;

pub(super) fn align_span_chunk(span: &mut EvidenceSpan, chunks: &[DocChunk]) {
    if let (Some(start), Some(end)) = (span.start, span.end)
        && let Some((idx, _)) = chunks
            .iter()
            .enumerate()
            .filter(|(_, c)| start < c.end && end > c.start)
            .max_by_key(|(_, c)| end.min(c.end).saturating_sub(start.max(c.start)))
    {
        span.chunk_idx = Some(idx);
    }
}

fn norm_match_text(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn value_text(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) => {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::Bool(b)) => Some(b.to_string()),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BlockMapKind {
    Exact(usize),
    Partial(usize),
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn contains_whole_token_phrase(haystack: &str, needle: &str) -> bool {
    let h = norm_match_text(haystack);
    let n = norm_match_text(needle);
    if h.is_empty() || n.is_empty() || n.len() > h.len() {
        return false;
    }
    let hb = h.as_bytes();
    let nb = n.as_bytes();
    let first_is_word = nb.first().copied().is_some_and(is_word_byte);
    let last_is_word = nb.last().copied().is_some_and(is_word_byte);
    let mut pos = 0usize;
    while let Some(rel) = h[pos..].find(&n) {
        let start = pos + rel;
        let end = start + n.len();
        let left_ok = !first_is_word || start == 0 || !is_word_byte(hb[start - 1]);
        let right_ok = !last_is_word || end == hb.len() || !is_word_byte(hb[end]);
        if left_ok && right_ok {
            return true;
        }
        pos = start + 1;
        while pos < h.len() && !h.is_char_boundary(pos) {
            pos += 1;
        }
    }
    false
}

fn heading_path_compatible(old_hp: &[String], new_hp: &[String]) -> bool {
    if old_hp == new_hp || new_hp.is_empty() {
        return true;
    }
    !old_hp.is_empty() && old_hp.ends_with(new_hp)
}

pub(super) fn build_old_to_new_block_map(
    old_indexed: &[IndexedBlock],
    new_indexed: &[IndexedBlock],
) -> HashMap<String, BlockMapKind> {
    let mut map = HashMap::new();
    if old_indexed.is_empty() || new_indexed.is_empty() {
        return map;
    }
    let old_norms: Vec<String> = old_indexed
        .iter()
        .map(|b| norm_match_text(&b.text))
        .collect();
    let new_norms: Vec<String> = new_indexed
        .iter()
        .map(|b| norm_match_text(&b.text))
        .collect();

    for (o_idx, ob) in old_indexed.iter().enumerate() {
        let norm_ob = &old_norms[o_idx];
        if norm_ob.is_empty() {
            continue;
        }
        // 1. Exact (kind, normalized_text, heading_path) match with ordinal disambiguation.
        let same_old: Vec<usize> = old_indexed
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                b.meta.kind == ob.meta.kind
                    && b.meta.heading_path == ob.meta.heading_path
                    && norm_match_text(&b.text) == *norm_ob
            })
            .map(|(i, _)| i)
            .collect();
        let same_new: Vec<usize> = new_indexed
            .iter()
            .enumerate()
            .filter(|(i, b)| {
                b.meta.kind == ob.meta.kind
                    && b.meta.heading_path == ob.meta.heading_path
                    && &new_norms[*i] == norm_ob
            })
            .map(|(i, _)| i)
            .collect();

        if !same_new.is_empty() {
            if same_old.len() == same_new.len()
                && let Some(ord) = same_old.iter().position(|&i| i == o_idx)
            {
                map.insert(ob.meta.block_id.clone(), BlockMapKind::Exact(same_new[ord]));
            }
            continue;
        }

        // 2. Heading path was stripped/shortened (e.g. headings: false), require global uniqueness.
        let all_old_text_matches = old_norms.iter().filter(|t| *t == norm_ob).count();
        if all_old_text_matches != 1 {
            continue;
        }
        let compat_new: Vec<usize> = new_indexed
            .iter()
            .enumerate()
            .filter(|(i, b)| {
                b.meta.kind == ob.meta.kind
                    && &new_norms[*i] == norm_ob
                    && heading_path_compatible(&ob.meta.heading_path, &b.meta.heading_path)
            })
            .map(|(i, _)| i)
            .collect();
        if compat_new.len() == 1 {
            map.insert(ob.meta.block_id.clone(), BlockMapKind::Exact(compat_new[0]));
            continue;
        }

        // 3. Partial block match when a block was split/trimmed, requiring >= 16 chars & >= 2 tokens & uniqueness.
        const MIN_PARTIAL_CHARS: usize = 16; // [策略] 分块裁剪后部分块匹配的最短字符数，过滤短词巧合
        let partial_new: Vec<usize> = new_indexed
            .iter()
            .enumerate()
            .filter(|(i, b)| {
                if !heading_path_compatible(&ob.meta.heading_path, &b.meta.heading_path) {
                    return false;
                }
                let nb_norm = &new_norms[*i];
                let shorter = if nb_norm.len() < norm_ob.len() {
                    nb_norm.as_str()
                } else {
                    norm_ob.as_str()
                };
                shorter.chars().count() >= MIN_PARTIAL_CHARS
                    && shorter.split_whitespace().count() >= 2
                    && (contains_whole_token_phrase(norm_ob, nb_norm)
                        || contains_whole_token_phrase(nb_norm, norm_ob))
            })
            .map(|(i, _)| i)
            .collect();
        if partial_new.len() == 1 {
            map.insert(
                ob.meta.block_id.clone(),
                BlockMapKind::Partial(partial_new[0]),
            );
        }
    }
    map
}

fn field_leaf_token(field_path: &str) -> Option<String> {
    let leaf = field_path
        .rsplit(['/', '.', '['])
        .find(|s| !s.is_empty() && !s.ends_with(']'))?;
    let mut spaced = String::with_capacity(leaf.len() + 2);
    for (i, ch) in leaf.chars().enumerate() {
        if ch == '_' || ch == '-' {
            spaced.push(' ');
        } else if ch.is_ascii_uppercase() && i > 0 {
            spaced.push(' ');
            spaced.push(ch.to_ascii_lowercase());
        } else {
            spaced.push(ch.to_ascii_lowercase());
        }
    }
    let norm = norm_match_text(&spaced);
    (!norm.is_empty()).then_some(norm)
}

fn find_metadata_island_block<'a>(
    field_path: &str,
    val_str: &str,
    indexed: &'a [IndexedBlock],
) -> Option<&'a IndexedBlock> {
    let norm_val = norm_match_text(val_str);
    if norm_val.is_empty() {
        return None;
    }
    const MIN_STANDALONE_META_CHARS: usize = 16; // [策略] 无字段名同现时元数据岛值独立匹配正文块的最短字符数
    let needs_field_context = norm_val.chars().count() < MIN_STANDALONE_META_CHARS
        || !norm_val.chars().any(|c| c.is_alphabetic());
    let leaf = field_leaf_token(field_path);
    let matches: Vec<&IndexedBlock> = indexed
        .iter()
        .filter(|b| {
            if !contains_whole_token_phrase(&b.text, &norm_val) {
                return false;
            }
            if !needs_field_context {
                return true;
            }
            let Some(ref leaf_tok) = leaf else {
                return false;
            };
            contains_whole_token_phrase(&b.text, leaf_tok)
                || b.meta
                    .heading_path
                    .iter()
                    .any(|h| contains_whole_token_phrase(h, leaf_tok))
        })
        .collect();
    if matches.len() == 1 {
        Some(matches[0])
    } else {
        None
    }
}

fn mark_span_ungrounded(span: &mut EvidenceSpan) {
    span.block_id = None;
    span.chunk_idx = None;
    span.start = None;
    span.end = None;
    span.verified = false;
    if span.method.is_metadata_island() {
        span.alignment = EvidenceAlignment::MetadataIsland;
        span.confidence = 0.85; // [策略] 元数据岛引用内容被查询过滤移除后的保留置信度
    } else {
        span.alignment = EvidenceAlignment::Unaligned;
        span.confidence = 0.0;
    }
}

pub(super) fn realign_span_to_filtered_blocks(
    span: &mut EvidenceSpan,
    claim_val: Option<&Value>,
    old_md: &str,
    old_indexed: &[IndexedBlock],
    old_to_new: &HashMap<String, BlockMapKind>,
    indexed: &[IndexedBlock],
) {
    let had_body_coords = span.block_id.is_some() || span.start.is_some();
    if !had_body_coords && span.method.is_metadata_island() {
        if let Some(val_str) = value_text(claim_val)
            && let Some(hit) = find_metadata_island_block(&span.field_path, &val_str, indexed)
        {
            span.block_id = Some(hit.meta.block_id.clone());
            span.chunk_idx = hit.meta.chunk_idx;
            span.start = Some(hit.meta.start);
            span.end = Some(hit.meta.end);
            span.excerpt = hit.meta.excerpt.clone();
            span.confidence = 0.95; // [策略] 元数据岛在过滤后正文重命中的高置信度
            span.verified = true;
        }
        return;
    }

    let resolved_old_bid = span.block_id.clone().or_else(|| {
        let (s, e) = (span.start?, span.end?);
        old_indexed
            .iter()
            .find(|b| b.meta.start <= s && e <= b.meta.end)
            .map(|b| b.meta.block_id.clone())
    });

    let Some(old_bid) = resolved_old_bid else {
        mark_span_ungrounded(span);
        return;
    };
    let Some(&mapped) = old_to_new.get(&old_bid) else {
        mark_span_ungrounded(span);
        return;
    };

    match mapped {
        BlockMapKind::Exact(new_idx) => {
            let hit = &indexed[new_idx];
            span.block_id = Some(hit.meta.block_id.clone());
            span.chunk_idx = hit.meta.chunk_idx;
            span.start = Some(hit.meta.start);
            span.end = Some(hit.meta.end);
            span.excerpt = hit.meta.excerpt.clone();
        }
        BlockMapKind::Partial(new_idx) => {
            let hit = &indexed[new_idx];
            let mut probe_phrases = Vec::new();
            if let Some(v) = value_text(claim_val) {
                probe_phrases.push(v);
            }
            if let (Some(s), Some(e)) = (span.start, span.end)
                && s < e
                && e <= old_md.len()
                && old_md.is_char_boundary(s)
                && old_md.is_char_boundary(e)
            {
                probe_phrases.push(old_md[s..e].to_string());
            }
            if !span.excerpt.trim().is_empty() {
                probe_phrases.push(span.excerpt.clone());
            }
            let still_present = probe_phrases
                .iter()
                .any(|p| contains_whole_token_phrase(&hit.text, p));
            if still_present {
                span.block_id = Some(hit.meta.block_id.clone());
                span.chunk_idx = hit.meta.chunk_idx;
                span.start = Some(hit.meta.start);
                span.end = Some(hit.meta.end);
                span.excerpt = hit.meta.excerpt.clone();
            } else {
                mark_span_ungrounded(span);
            }
        }
    }
}
