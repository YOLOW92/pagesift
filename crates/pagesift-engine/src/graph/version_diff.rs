//! Version diffing, 304 conditional snapshot validation, and doc_id remapping on CAS retry.

use std::collections::HashSet;

use pagesift_diff::{BlockChange, ChangeType, DiffOptions, NoiseCategory};
use pagesift_graph::{
    DocNode, GraphOptions, PageGraphDelta, VersionDiffStats, VersionEdge, VersionRecord,
    build_indexed_blocks, link_version_pair,
};
use pagesift_store::LoadedVersionSnapshot;

use super::PageGraphSeed;

const MIN_FUZZY_BLOCK_CHARS: usize = 16; // [策略] 变更块文本回退匹配最短字符阈值

/// Keep dates/relative-times/counts as noise while treating prices & numbers as substantive changes.
pub fn version_diff_options() -> DiffOptions {
    DiffOptions {
        noise_categories: vec![
            NoiseCategory::Date,
            NoiseCategory::RelativeTime,
            NoiseCategory::Count,
        ],
        ..Default::default()
    }
}

fn normalize_diff_block_text(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

pub(super) fn match_changed_blocks(
    changes: &[BlockChange],
    doc_id: &str,
    markdown: &str,
    chunk_spans: &[(usize, usize, usize)],
    opts: &GraphOptions,
) -> Vec<String> {
    if !opts.blocks || markdown.is_empty() {
        return Vec::new();
    }
    let blocks = build_indexed_blocks(
        doc_id,
        markdown,
        chunk_spans,
        opts.limits.max_blocks_per_doc,
        opts.limits.max_excerpt_chars,
    );
    if blocks.is_empty() {
        return Vec::new();
    }
    let norm_blocks: Vec<String> = blocks
        .iter()
        .map(|b| normalize_diff_block_text(&b.text))
        .collect();
    let mut ids = Vec::new();
    let mut matched = HashSet::new();

    for change in changes {
        if !matches!(
            change.change,
            ChangeType::Added | ChangeType::Modified | ChangeType::Moved
        ) {
            continue;
        }
        let needle = change.new_text.as_deref().unwrap_or("").trim();
        let norm_needle = normalize_diff_block_text(needle);
        if norm_needle.is_empty() {
            continue;
        }

        // 1. 优先用 diff 报告的新块序号精确对齐；避免相同页脚/导航模板误命中首个副本。
        if let Some(new_idx) = change.new_index
            && new_idx < blocks.len()
            && norm_blocks[new_idx] == norm_needle
            && matched.insert(blocks[new_idx].meta.block_id.clone())
        {
            ids.push(blocks[new_idx].meta.block_id.clone());
            continue;
        }

        // 2. 精确规范化全文匹配（按与 `new_index` 最近距离消歧同文块）。
        let exact_idx = norm_blocks
            .iter()
            .enumerate()
            .filter(|(idx, bt)| {
                *bt == &norm_needle && !matched.contains(&blocks[*idx].meta.block_id)
            })
            .min_by_key(|(idx, _)| change.new_index.map_or(*idx, |ni| idx.abs_diff(ni)))
            .map(|(idx, _)| idx);
        if let Some(idx) = exact_idx
            && matched.insert(blocks[idx].meta.block_id.clone())
        {
            ids.push(blocks[idx].meta.block_id.clone());
            continue;
        }

        // 3. 段落分块边界略有差异时，仅对足够长的非琐碎文本允许唯一/最近子串回退。
        if norm_needle.chars().count() >= MIN_FUZZY_BLOCK_CHARS {
            let fuzzy_idx = norm_blocks
                .iter()
                .enumerate()
                .filter(|(idx, bt)| {
                    !matched.contains(&blocks[*idx].meta.block_id)
                        && (bt.contains(&norm_needle)
                            || (bt.chars().count() >= MIN_FUZZY_BLOCK_CHARS
                                && norm_needle.contains(bt.as_str())))
                })
                .min_by_key(|(idx, _)| change.new_index.map_or(*idx, |ni| idx.abs_diff(ni)))
                .map(|(idx, _)| idx);
            if let Some(idx) = fuzzy_idx
                && matched.insert(blocks[idx].meta.block_id.clone())
            {
                ids.push(blocks[idx].meta.block_id.clone());
            }
        }
    }
    ids
}

pub(super) fn compute_version_diff(
    prev_md: Option<&str>,
    markdown: &str,
    not_modified: bool,
    doc_id: &str,
    chunk_spans: &[(usize, usize, usize)],
    opts: &GraphOptions,
) -> (Option<VersionDiffStats>, Vec<String>) {
    if not_modified && markdown.is_empty() {
        return (Some(VersionDiffStats::default()), Vec::new());
    }
    let Some(prev_md) = prev_md else {
        return (None, Vec::new());
    };
    let report = pagesift_diff::diff_with(prev_md, markdown, &version_diff_options());
    let stats = VersionDiffStats {
        old_blocks: report.stats.old_blocks,
        new_blocks: report.stats.new_blocks,
        unchanged: report.stats.unchanged,
        added: report.stats.added,
        removed: report.stats.removed,
        modified: report.stats.modified,
        moved: report.stats.moved,
        noise: report.stats.noise,
        substantive: report.stats.substantive,
    };
    let changed = match_changed_blocks(&report.blocks, doc_id, markdown, chunk_spans, opts);
    (Some(stats), changed)
}

pub(super) fn load_prev_snapshot_for_version(
    gs: &pagesift_store::GraphStore<'_>,
    prev: &VersionRecord,
) -> (Option<String>, Option<String>, Option<String>) {
    match gs.load_version_snapshot(prev) {
        Ok(LoadedVersionSnapshot::Markdown(md)) => (Some(md), None, None),
        Ok(LoadedVersionSnapshot::Unavailable { reason }) => {
            let warn = format!("graph_version_diff_unavailable: {reason}");
            (None, Some(reason), Some(warn))
        }
        Err(e) => {
            let reason = format!("snapshot_read_error: {e}");
            let warn = format!("graph_version_diff_unavailable: {reason}");
            (None, Some(reason), Some(warn))
        }
    }
}

pub(super) fn remap_delta_doc_id(
    delta: &mut PageGraphDelta,
    new_doc_id: String,
    observed_at_ms: u64,
    prev_version: Option<&VersionRecord>,
    diff_stats: Option<VersionDiffStats>,
    changed_block_ids: Vec<String>,
    diff_unavailable_reason: Option<String>,
) {
    let old_doc_id = std::mem::replace(&mut delta.node.doc_id, new_doc_id.clone());
    let old_prefix = format!("{old_doc_id}#");
    delta.node.observed_at_ms = observed_at_ms;
    if let Some(ref mut t) = delta.temporal {
        t.observed_at_ms = observed_at_ms;
    }

    for b in &mut delta.blocks {
        b.doc_id = new_doc_id.clone();
        b.block_id = format!("{new_doc_id}#b{}", b.block_idx);
    }
    for c in &mut delta.citations {
        if c.from_doc_id == old_doc_id {
            c.from_doc_id = new_doc_id.clone();
        }
    }
    for ev in &mut delta.evidence {
        ev.doc_id = new_doc_id.clone();
        if let Some(bid) = ev.block_id.as_mut()
            && let Some(suffix) = bid.strip_prefix(&old_prefix)
        {
            *bid = format!("{new_doc_id}#{suffix}");
        }
    }
    for claim in &mut delta.claims {
        claim.doc_id = new_doc_id.clone();
        claim.observed_at_ms = observed_at_ms;
        if let Some(ref mut t) = claim.temporal {
            t.observed_at_ms = observed_at_ms;
        }
        claim.evidence.doc_id = new_doc_id.clone();
        if let Some(bid) = claim.evidence.block_id.as_mut()
            && let Some(suffix) = bid.strip_prefix(&old_prefix)
        {
            *bid = format!("{new_doc_id}#{suffix}");
        }
    }
    for edge in &mut delta.provenance {
        match edge {
            pagesift_graph::ProvenanceEdge::AttachmentOf { parent_doc_id, .. } => {
                if parent_doc_id.as_deref() == Some(old_doc_id.as_str()) {
                    *parent_doc_id = Some(new_doc_id.clone());
                }
            }
            pagesift_graph::ProvenanceEdge::DerivedFrom {
                doc_id,
                parent_doc_id,
                ..
            } => {
                if doc_id == &old_doc_id {
                    *doc_id = new_doc_id.clone();
                }
                if parent_doc_id.as_deref() == Some(old_doc_id.as_str()) {
                    *parent_doc_id = Some(new_doc_id.clone());
                }
            }
            _ => {}
        }
    }

    let curr_record = VersionRecord {
        resource_key: delta.node.resource_key.clone(),
        doc_id: delta.node.doc_id.clone(),
        url: delta.node.final_url.clone(),
        canonical_url: delta.node.canonical_url.clone(),
        observed_at_ms: delta.node.observed_at_ms,
        content_hash: delta.node.content_hash.clone(),
        text_hash: delta.node.text_hash.clone(),
        status_code: delta.node.status_code,
        etag: delta.node.etag.clone(),
        last_modified: delta.node.last_modified.clone(),
    };
    let mut next_edge: Option<VersionEdge> = prev_version
        .map(|prev| link_version_pair(prev, &curr_record, diff_stats, changed_block_ids));
    if let (Some(edge), Some(reason)) = (next_edge.as_mut(), diff_unavailable_reason) {
        edge.diff_unavailable_reason = Some(reason);
    }
    delta.version = next_edge;
}

pub(super) fn resolve_304_snapshot(
    gs: &pagesift_store::GraphStore<'_>,
    lookup_refs: &[&str],
    resource_key: &str,
    final_url: &str,
    seed: &PageGraphSeed,
) -> (Option<VersionRecord>, Option<String>, Option<String>) {
    if seed.unconditional_not_modified {
        return (None, None, Some("unconditional_not_modified".to_string()));
    }
    let resp_etag = seed
        .etag
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let req_inm = seed
        .if_none_match
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let resp_lm = seed
        .last_modified
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let req_ims = seed
        .if_modified_since
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if let (Some(inm), Some(etag)) = (req_inm, resp_etag)
        && inm != "*"
        && !pagesift_graph::if_none_match_matches(inm, etag)
    {
        return (None, None, Some("validator_mismatch".to_string()));
    }

    let has_specific_etag = resp_etag.is_some() || req_inm.is_some_and(|v| v != "*");
    let target_lm = resp_lm.or(req_ims);
    if !has_specific_etag && target_lm.is_none() {
        return (None, None, Some("validator_mismatch".to_string()));
    }

    let norm_final = pagesift_graph::normalize_url(final_url);
    let matched_rec = gs
        .find_version_for_urls(lookup_refs, |rec| {
            let rec_key = DocNode::derive_resource_key(rec.canonical_url.as_deref(), &rec.url);
            let same_repr =
                rec_key == resource_key || pagesift_graph::normalize_url(&rec.url) == norm_final;
            if !same_repr {
                return false;
            }
            if let Some(etag) = resp_etag {
                return rec
                    .etag
                    .as_deref()
                    .is_some_and(|re| pagesift_graph::etag_weak_match(re, etag));
            }
            if let Some(inm) = req_inm.filter(|v| *v != "*") {
                return rec
                    .etag
                    .as_deref()
                    .is_some_and(|re| pagesift_graph::if_none_match_matches(inm, re));
            }
            if let Some(lm) = target_lm {
                return rec
                    .last_modified
                    .as_deref()
                    .is_some_and(|rlm| rlm.trim() == lm);
            }
            false
        })
        .ok()
        .flatten();

    let Some(matched) = matched_rec else {
        return (None, None, Some("validator_mismatch".to_string()));
    };
    let (md, unavail, _) = load_prev_snapshot_for_version(gs, &matched);
    (Some(matched), md, unavail)
}
