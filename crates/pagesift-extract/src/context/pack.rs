//! Task-oriented, evidence- and budget-constrained [`ContextPack`] compiler.
//!
//! Implements:
//! - Hard MustKeep constraints: GFM table header/caption preservation & row slicing (with or without
//!   leading `|` pipes), atomic qualifier/negation/unit protection, and opposing-conflict bundle pairing
//!   with deduplicated union token accounting;
//! - Exact pre-rendered section header token accounting so compiled output never breaches `budget_tokens`;
//! - Orderings: `EdgeSalience` (boundary placement against lost-in-the-middle, Liu et al. 2024),
//!   `ScoreDescending`, and `DocumentOrder`.

use std::collections::BTreeSet;
use std::time::Instant;

use pagesift_graph::ConflictRecord;
use pagesift_graph::context::{
    ContextPack, ContextPackRequest, ContextSearchHit, ContextSearchReport, PackCostBreakdown,
    PackOrdering, PackSelector, PackedSection, ProvenanceEntry, UnitKind,
};

use crate::context::pack_select::{
    CandidateItem, ClosureBundle, assign_independent_root_ids, marginal_bundle_cost,
    select_dpp_bundles, select_mmr_bundles, select_submodular_closure, select_topk_bundles,
};
use crate::context::temporal_eval::canonical_origin_root;
use crate::context::tokenize_unified::{LexicalFeatures, analyze_query, analyze_text};
use crate::context::unit_scan::{fast_token_count, is_table_delimiter_row};

// [策略] 非有限 token_safety_margin 输入回退默认 5% 裕度
const DEFAULT_SAFETY_MARGIN: f32 = 0.05;
// [策略] 安全裕度上限 45% 防止可用预算归零
const MAX_SAFETY_MARGIN: f32 = 0.45;

/// Compile a [`ContextPack`] from a [`ContextSearchReport`] under token and evidence constraints.
pub fn compile_context_pack(
    req: &ContextPackRequest,
    report: &ContextSearchReport,
    search_micros: u64,
) -> ContextPack {
    let t_compile_start = Instant::now();
    let margin = if req.token_safety_margin.is_finite() {
        req.token_safety_margin.clamp(0.0, MAX_SAFETY_MARGIN)
    } else {
        DEFAULT_SAFETY_MARGIN
    };
    let effective_budget = ((req.budget_tokens as f64) * f64::from(1.0 - margin)).floor() as usize;

    let q_analysis = analyze_query(&req.search.query);
    let q_feats = &q_analysis.features;
    let mut warnings = report.warnings.clone();
    let mut omitted_reasons = Vec::new();

    if report.hits.is_empty() || effective_budget == 0 {
        let compile_us = t_compile_start.elapsed().as_micros() as u64;
        return ContextPack {
            query: req.search.query.clone(),
            compiled_context: String::new(),
            sections: Vec::new(),
            provenance_manifest: Vec::new(),
            conflicts: report.conflicts.clone(),
            total_tokens: 0,
            effective_budget_tokens: effective_budget,
            requested_budget_tokens: req.budget_tokens,
            budget_insufficient: !report.hits.is_empty() && effective_budget == 0,
            must_keep_preserved: report.hits.is_empty(),
            omitted_must_keep_reasons: if !report.hits.is_empty() && effective_budget == 0 {
                vec!["effective_budget_zero".to_owned()]
            } else {
                Vec::new()
            },
            selector_used: req.selector,
            ordering_used: req.ordering,
            dimensions: report.dimensions,
            independent_source_count: 0,
            visibilities: report.visibilities.clone(),
            cost: PackCostBreakdown {
                search_micros,
                rerank_micros: 0,
                compile_micros: compile_us,
                total_micros: search_micros + compile_us,
                candidate_units_considered: report.hits.len(),
            },
            warnings,
        };
    }

    // Reserve exact tokens for explicit conflict summary header if active conflicts exist
    let conflict_banner = render_conflict_banner(&report.conflicts);
    let conflict_banner_tokens = if conflict_banner.is_empty() {
        0
    } else {
        fast_token_count(&conflict_banner)
    };
    let banner_fits = !conflict_banner.is_empty() && conflict_banner_tokens <= effective_budget;
    let unit_budget = if banner_fits {
        effective_budget - conflict_banner_tokens
    } else {
        effective_budget
    };

    // 1. Deduplicate candidate hits by `unit_id` and compute EXACT header + body token costs!
    let (indep_root_ids, num_indep_roots) = assign_independent_root_ids(&report.hits);
    let mut seen_unit_ids: BTreeSet<&str> = BTreeSet::new();
    let mut items: Vec<CandidateItem> = Vec::with_capacity(report.hits.len());

    for (idx, hit) in report.hits.iter().enumerate() {
        if !seen_unit_ids.insert(hit.unit.unit_id.as_str()) {
            continue;
        }
        let header_suffix = format_header_suffix(&hit.unit);
        // Exact token count of `[C1 <header_suffix>]` (any `C<n>` is 1 ASCII token in `token_spans`!)
        let header_tokens = fast_token_count(&format!("[C1 {header_suffix}"));
        let (body, injected) = prepare_unit_body(
            hit,
            q_feats,
            unit_budget,
            header_tokens,
            req.must_keep_table_headers,
            req.must_keep_qualifiers,
        );
        let body_tokens = if !injected && body.len() == hit.unit.text.len() {
            hit.unit.tokens.max(1)
        } else {
            fast_token_count(&body).max(1)
        };
        let exact_cost = (header_tokens + body_tokens).max(1);
        let feats = analyze_text(&body);

        items.push(CandidateItem {
            hit_idx: idx,
            header_suffix,
            rendered_body: body,
            cost_tokens: exact_cost,
            // [策略] 最小非零基础质量分 0.05
            base_quality: f64::from(hit.rank_score).max(0.05),
            // [策略] 最小正证据支持度权重 0.20
            support_weight: f64::from(hit.dimensions.support).max(0.20),
            indep_root_id: indep_root_ids[idx],
            table_header_injected: injected,
            feats,
        });
    }

    // 2. Build complementary closure bundles (sharing common anchor/counterpart items without double-charging!)
    let bundles = build_closure_bundles(&items, &report.hits, &report.conflicts, req);

    // Check individual & union feasibility of all mandatory `must_keep` bundles against `unit_budget`
    let mut budget_insufficient = false;
    let mut must_keep_preserved = true;
    let mut union_must_mask = vec![false; items.len()];
    let mut union_must_cost = 0usize;
    for b in &bundles {
        if b.must_keep {
            let single_cost: usize = b.members.iter().map(|&mi| items[mi].cost_tokens).sum();
            if single_cost > unit_budget {
                budget_insufficient = true;
                must_keep_preserved = false;
                omitted_reasons.push(format!(
                    "must_keep_bundle_cost_{single_cost}_exceeds_budget_{unit_budget}"
                ));
            }
            let incr = marginal_bundle_cost(b, &items, &union_must_mask);
            union_must_cost = union_must_cost.saturating_add(incr);
            for &mi in &b.members {
                union_must_mask[mi] = true;
            }
        }
    }
    if union_must_cost > unit_budget && must_keep_preserved {
        budget_insufficient = true;
        must_keep_preserved = false;
        omitted_reasons.push(format!(
            "must_keep_union_cost_{union_must_cost}_exceeds_budget_{unit_budget}"
        ));
    }
    if !report.conflicts.is_empty() && conflict_banner_tokens > effective_budget {
        budget_insufficient = true;
        must_keep_preserved = false;
        omitted_reasons.push("conflict_disclosure_exceeds_budget".to_owned());
    }

    let t_rerank_start = Instant::now();
    let chosen_bundles = match req.selector {
        PackSelector::TopKGreedy => select_topk_bundles(&bundles, &items, unit_budget),
        PackSelector::MmrGreedy => select_mmr_bundles(&bundles, &items, &report.hits, unit_budget),
        PackSelector::BoundedDpp => select_dpp_bundles(&bundles, &items, &report.hits, unit_budget),
        PackSelector::SubmodularClosure => select_submodular_closure(
            &bundles,
            &items,
            &report.hits,
            &q_analysis,
            num_indep_roots,
            unit_budget,
        ),
    };
    let rerank_micros = t_rerank_start.elapsed().as_micros() as u64;

    if chosen_bundles.is_empty() && !items.is_empty() {
        budget_insufficient = true;
        let min_cost = items.iter().map(|it| it.cost_tokens).min().unwrap_or(0);
        if req.must_keep_qualifiers
            && items
                .iter()
                .any(|it| report.hits[it.hit_idx].unit.has_negation_or_qualifier)
        {
            must_keep_preserved = false;
            omitted_reasons.push(format!(
                "atomic_qualified_unit_min_cost_{min_cost}_exceeds_budget_{unit_budget}"
            ));
        } else {
            warnings.push(format!(
                "budget_too_small:min_unit_cost={min_cost},budget={unit_budget}"
            ));
        }
    }

    // Flatten chosen bundles into UNIQUE deduplicated item entries (never emitting any unit twice!)
    let mut item_slot: Vec<Option<usize>> = vec![None; items.len()];
    let mut flat_selected: Vec<(usize, f32, bool, bool)> = Vec::new();
    for &(b_idx, gain) in &chosen_bundles {
        let b = &bundles[b_idx];
        let per_member_gain = gain / (b.members.len().max(1) as f32);
        for &m_idx in &b.members {
            if let Some(pos) = item_slot[m_idx] {
                let entry = &mut flat_selected[pos];
                if per_member_gain > entry.1 {
                    entry.1 = per_member_gain;
                }
                entry.2 |= b.paired_conflict;
                entry.3 |= b.must_keep;
            } else {
                item_slot[m_idx] = Some(flat_selected.len());
                flat_selected.push((m_idx, per_member_gain, b.paired_conflict, b.must_keep));
            }
        }
    }

    // Guard check: if top-ranked hit carries negation/qualifier (`must_keep_qualifiers`) and could not fit
    if req.must_keep_qualifiers
        && let Some(top_it) = items.first()
        && report.hits[top_it.hit_idx].unit.has_negation_or_qualifier
        && item_slot[0].is_none()
        && top_it.cost_tokens > unit_budget
    {
        budget_insufficient = true;
        must_keep_preserved = false;
        omitted_reasons.push(format!(
            "top_qualified_unit_cost_{}_exceeds_budget_{unit_budget}",
            top_it.cost_tokens
        ));
    }

    // 3. Apply prompt ordering (mitigating lost-in-the-middle when EdgeSalience is chosen)
    order_selected_items(&mut flat_selected, &items, &report.hits, req.ordering);

    // 4. Render structured sections & enforce hard token ceiling (`total_tokens <= effective_budget`)
    let mut sections = Vec::with_capacity(flat_selected.len());
    let mut manifest = Vec::with_capacity(flat_selected.len());
    let mut rendered_blocks = Vec::with_capacity(flat_selected.len() + 1);
    let mut indep_roots: BTreeSet<String> = BTreeSet::new();
    let mut running_tokens = 0usize;

    if banner_fits {
        running_tokens += conflict_banner_tokens;
        rendered_blocks.push(conflict_banner);
    }

    for &(item_idx, gain, paired, is_must_keep) in &flat_selected {
        let item = &items[item_idx];
        let seq_num = sections.len() + 1;
        let ref_id = format!("C{seq_num}");
        let full_section = format!("[{ref_id} {}\n{}", item.header_suffix, item.rendered_body);
        let sec_tokens = fast_token_count(&full_section);

        if running_tokens + sec_tokens > effective_budget {
            budget_insufficient = true;
            if is_must_keep {
                must_keep_preserved = false;
                omitted_reasons.push(format!(
                    "section_overflow_guard:cost={sec_tokens},remaining={}",
                    effective_budget.saturating_sub(running_tokens)
                ));
            }
            continue;
        }
        running_tokens += sec_tokens;

        let hit = &report.hits[item.hit_idx];
        let u = &hit.unit;
        if u.origin_independent && !u.origin_root.trim().is_empty() {
            indep_roots.insert(canonical_origin_root(&u.origin_root));
        }

        rendered_blocks.push(full_section.clone());
        sections.push(PackedSection {
            ref_id: ref_id.clone(),
            unit_id: u.unit_id.clone(),
            kind: u.kind,
            rendered_text: full_section,
            tokens: sec_tokens,
            utility_gain: gain,
            table_header_injected: item.table_header_injected,
            paired_for_conflict: paired,
        });
        manifest.push(ProvenanceEntry {
            ref_id,
            unit_id: u.unit_id.clone(),
            revision_id: u.revision_id.clone(),
            doc_id: u.location.doc_id.clone(),
            source_url: u.location.source_url.clone(),
            origin_root: u.origin_root.clone(),
            origin_independent: u.origin_independent,
            byte_start: u.location.byte_start,
            byte_end: u.location.byte_end,
            page_number: u.location.page_number,
            heading_path: u.location.heading_path.clone(),
            observed_at_ms: u.observed_at_ms,
            valid_from: u.temporal.as_ref().and_then(|t| t.valid_from.clone()),
            valid_until: u.temporal.as_ref().and_then(|t| t.valid_until.clone()),
            status: u.status.clone(),
        });
    }

    let compiled_context = rendered_blocks.join("\n\n");
    let total_tokens = if compiled_context.is_empty() {
        0
    } else {
        fast_token_count(&compiled_context)
    };

    let compile_micros = t_compile_start.elapsed().as_micros() as u64;
    ContextPack {
        query: req.search.query.clone(),
        compiled_context,
        sections,
        provenance_manifest: manifest,
        conflicts: report.conflicts.clone(),
        total_tokens,
        effective_budget_tokens: effective_budget,
        requested_budget_tokens: req.budget_tokens,
        budget_insufficient,
        must_keep_preserved,
        omitted_must_keep_reasons: omitted_reasons,
        selector_used: req.selector,
        ordering_used: req.ordering,
        dimensions: report.dimensions,
        independent_source_count: indep_roots.len(),
        visibilities: report.visibilities.clone(),
        cost: PackCostBreakdown {
            search_micros,
            rerank_micros,
            compile_micros,
            total_micros: search_micros + compile_micros,
            candidate_units_considered: report.hits.len(),
        },
        warnings,
    }
}

fn format_header_suffix(u: &pagesift_graph::context::KnowledgeUnit) -> String {
    let validity_note = match (
        u.temporal.as_ref().and_then(|t| t.valid_from.as_deref()),
        u.temporal.as_ref().and_then(|t| t.valid_until.as_deref()),
    ) {
        (Some(f), Some(e)) => format!(" | valid:{f}..{e}"),
        (Some(f), None) => format!(" | validFrom:{f}"),
        (None, Some(e)) => format!(" | validUntil:{e}"),
        (None, None) => String::new(),
    };
    let heading_str = if u.location.heading_path.is_empty() {
        String::new()
    } else {
        format!(" | path:{}", u.location.heading_path.join(" > "))
    };
    format!(
        "| unit:{} | doc:{} | status:{}{validity_note}{heading_str}]",
        u.unit_id,
        u.location.doc_id,
        u.status.tag()
    )
}

fn prepare_unit_body(
    hit: &ContextSearchHit,
    q_feats: &LexicalFeatures,
    unit_budget: usize,
    header_tokens: usize,
    must_keep_table_headers: bool,
    must_keep_qualifiers: bool,
) -> (String, bool) {
    let u = &hit.unit;
    let body_budget = unit_budget.saturating_sub(header_tokens);
    if u.tokens <= body_budget || body_budget == 0 {
        let already_slice = u.location.table_meta.as_ref().is_some_and(|m| m.is_slice);
        return (u.text.clone(), already_slice);
    }

    // Oversized Table: extract matching data rows while forcibly injecting caption + GFM header + delimiter!
    if u.kind == UnitKind::Table
        && must_keep_table_headers
        && let Some(sliced) = slice_table_preserving_headers(
            &u.text,
            u.location.table_meta.as_ref(),
            q_feats,
            body_budget,
            must_keep_qualifiers,
        )
    {
        return (sliced, true);
    }

    (u.text.clone(), false)
}

fn slice_table_preserving_headers(
    text: &str,
    meta: Option<&pagesift_graph::context::TableStructureMeta>,
    q_feats: &LexicalFeatures,
    body_budget: usize,
    must_keep_qualifiers: bool,
) -> Option<String> {
    let all_lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    // Locate the GFM delimiter row (`| --- | --- |` or `--- | ---` without leading pipes!)
    let delim_idx = all_lines
        .iter()
        .enumerate()
        .find(|&(i, l)| i >= 1 && is_table_delimiter_row(l) && all_lines[i - 1].contains('|'))
        .map(|(i, _)| i)?;

    let header_line = all_lines[delim_idx - 1];
    let delim_line = all_lines[delim_idx];
    let body_lines: Vec<&str> = all_lines[delim_idx + 1..]
        .iter()
        .copied()
        .filter(|l| l.contains('|'))
        .collect();
    if body_lines.is_empty() {
        return None;
    }

    let prefix = match meta.and_then(|m| m.caption.as_deref()) {
        Some(cap) => format!("{cap}\n\n{header_line}\n{delim_line}"),
        None => format!("{header_line}\n{delim_line}"),
    };
    let prefix_toks = fast_token_count(&prefix);
    if prefix_toks >= body_budget {
        return None;
    }
    let rem_budget = body_budget - prefix_toks;
    let mut scored_rows: Vec<(usize, u32, &str)> = body_lines
        .iter()
        .enumerate()
        .map(|(idx, &row)| {
            let r_lex = analyze_text(row);
            let mut hits = 0u32;
            for qt in q_feats.term_freqs.keys() {
                if r_lex.term_freqs.contains_key(qt) {
                    hits += 2;
                }
            }
            for sym in &q_feats.exact_symbols {
                if r_lex.exact_symbols.contains(sym) {
                    // [策略] 表格数据行精确符号命中赋予 +4 优先保留分
                    hits += 4;
                }
            }
            if must_keep_qualifiers && r_lex.has_negation_or_qualifier {
                // [策略] 启用限定词保护时，含否定/例外说明的表格行（如脚注/除外条款行）加权 +2 防误裁
                hits += 2;
            }
            if r_lex.has_units_or_currency && hits > 0 {
                hits += 1;
            }
            (idx, hits, row)
        })
        .collect();
    scored_rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut chosen: Vec<(usize, &str)> = Vec::new();
    let mut used = 0usize;
    for (idx, _, row) in scored_rows {
        let rt = fast_token_count(row).max(1);
        if used + rt <= rem_budget {
            chosen.push((idx, row));
            used += rt;
        }
    }
    if chosen.is_empty() {
        return None;
    }
    chosen.sort_by_key(|&(idx, _)| idx);
    let body: Vec<&str> = chosen.into_iter().map(|(_, r)| r).collect();
    Some(format!("{prefix}\n{}", body.join("\n")))
}

fn build_closure_bundles(
    items: &[CandidateItem],
    hits: &[ContextSearchHit],
    conflicts: &[ConflictRecord],
    req: &ContextPackRequest,
) -> Vec<ClosureBundle> {
    let n = items.len();
    let mut covered_in_must_bundle = vec![false; n];
    let mut bundles = Vec::new();

    // 1. Pair opposing conflict variants when `must_keep_conflicts` is active.
    //    Allows multiple conflict bundles to share a common opposing benchmark unit without
    //    starving the second conflict pair; exact union cost prevents double token charging!
    if req.must_keep_conflicts && !conflicts.is_empty() {
        for c in conflicts {
            if c.variants.len() < 2 {
                continue;
            }
            let mut side_reps: Vec<usize> = Vec::new();
            for var in &c.variants {
                if let Some(idx) = find_best_variant_item_index(var, items, hits)
                    && !side_reps.contains(&idx)
                {
                    side_reps.push(idx);
                }
            }
            if side_reps.len() >= 2 {
                for &idx in &side_reps {
                    covered_in_must_bundle[idx] = true;
                }
                let primary = side_reps[0];
                bundles.push(ClosureBundle {
                    members: side_reps,
                    primary_item: primary,
                    must_keep: true,
                    paired_conflict: true,
                });
            }
        }
    }

    // Track header-only units (`row_count == 0`) that are already attached as a shared header to at
    // least one data-row unit so we do not emit a redundant header-only singleton competing against its rows!
    let mut attached_headers = vec![false; n];
    let mut shared_hdr_for_item: Vec<Option<usize>> = vec![None; n];
    if req.must_keep_table_headers {
        for i in 0..n {
            if covered_in_must_bundle[i]
                || hits[items[i].hit_idx].unit.kind == UnitKind::SectionHeading
            {
                continue;
            }
            if let Some(hdr_idx) = find_shared_header_unit_index(i, items, hits) {
                attached_headers[hdr_idx] = true;
                shared_hdr_for_item[i] = Some(hdr_idx);
            }
        }
    }

    // 2. Build candidate bundles for remaining items (setting `primary_item = i` to the substantive unit!)
    for i in 0..n {
        if covered_in_must_bundle[i] || attached_headers[i] {
            continue;
        }
        let mut members = Vec::with_capacity(2);
        if let Some(hdr_idx) = shared_hdr_for_item[i] {
            members.push(hdr_idx);
        }
        members.push(i);
        bundles.push(ClosureBundle {
            members,
            primary_item: i,
            must_keep: false,
            paired_conflict: false,
        });
    }
    bundles
}

fn find_best_variant_item_index(
    var: &pagesift_graph::ConflictVariant,
    items: &[CandidateItem],
    hits: &[ContextSearchHit],
) -> Option<usize> {
    let mut best: Option<(u8, usize)> = None;
    let val_hint = var
        .raw_values
        .first()
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();

    for (i, it) in items.iter().enumerate() {
        let u = &hits[it.hit_idx].unit;
        let matches_doc_or_src = var.evidence.iter().any(|ev| ev.doc_id == u.location.doc_id)
            || var.sources.iter().any(|s| s == &u.location.source_url);
        if !matches_doc_or_src {
            continue;
        }

        let exact_span = var.evidence.iter().any(|ev| {
            ev.doc_id == u.location.doc_id
                && (ev
                    .block_id
                    .as_deref()
                    .is_some_and(|bid| u.location.block_ids.iter().any(|b| b == bid))
                    || ev
                        .start
                        .zip(ev.end)
                        .is_some_and(|(s, e)| s < u.location.byte_end && e > u.location.byte_start))
        });
        let has_value = !val_hint.is_empty() && u.text.contains(val_hint);
        let non_heading = u.kind != UnitKind::SectionHeading;

        let tier: u8 = if exact_span {
            4 // [策略] 精确块/字节区间对齐优先级 4
        } else if has_value && non_heading {
            3 // [策略] 正文包含冲突值优先级 3
        } else if non_heading {
            2
        } else {
            1
        };

        if best.is_none_or(|(bt, _)| tier > bt) {
            best = Some((tier, i));
        }
    }
    best.map(|(_, idx)| idx)
}

/// Find a standalone table-header unit (e.g. a malformed/header-only table block with `row_count == 0`
/// in the same document & section) that multiple row units may share without double-counting!
fn find_shared_header_unit_index(
    target_i: usize,
    items: &[CandidateItem],
    hits: &[ContextSearchHit],
) -> Option<usize> {
    let target_u = &hits[items[target_i].hit_idx].unit;
    if target_u.kind != UnitKind::Table {
        return None;
    }
    // If target already has non-empty headers injected, it is self-contained
    if target_u
        .location
        .table_meta
        .as_ref()
        .is_some_and(|m| !m.headers.is_empty() && target_u.text.contains("---"))
    {
        return None;
    }
    (0..items.len()).find(|&j| {
        if j == target_i {
            return false;
        }
        let cand = &hits[items[j].hit_idx].unit;
        cand.location.doc_id == target_u.location.doc_id
            && cand.location.heading_path == target_u.location.heading_path
            && cand.location.byte_end <= target_u.location.byte_start
            && cand
                .location
                .table_meta
                .as_ref()
                .is_some_and(|m| m.row_count == 0 && !m.headers.is_empty())
    })
}

fn order_selected_items(
    selected: &mut Vec<(usize, f32, bool, bool)>,
    items: &[CandidateItem],
    hits: &[ContextSearchHit],
    ordering: PackOrdering,
) {
    match ordering {
        PackOrdering::DocumentOrder => {
            selected.sort_by(|a, b| {
                let ua = &hits[items[a.0].hit_idx].unit;
                let ub = &hits[items[b.0].hit_idx].unit;
                ua.location
                    .resource_key
                    .cmp(&ub.location.resource_key)
                    .then_with(|| ua.location.byte_start.cmp(&ub.location.byte_start))
            });
        }
        PackOrdering::ScoreDescending => {
            selected.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        }
        PackOrdering::EdgeSalience => {
            selected.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            // [推导] 至少 3 个片段时才存在“中部位置”与首尾边界之分（Liu et al. 2024）
            if selected.len() >= 3 {
                let mut head = Vec::with_capacity(selected.len().div_ceil(2));
                let mut tail = Vec::with_capacity(selected.len() / 2);
                for (i, entry) in selected.drain(..).enumerate() {
                    if i % 2 == 0 {
                        head.push(entry);
                    } else {
                        tail.push(entry);
                    }
                }
                tail.reverse();
                head.extend(tail);
                *selected = head;
            }
        }
    }
}

fn render_conflict_banner(conflicts: &[ConflictRecord]) -> String {
    if conflicts.is_empty() {
        return String::new();
    }
    let mut lines = Vec::with_capacity(conflicts.len() + 1);
    lines.push("[CONFLICTS DETECTED — opposing evidence preserved]".to_owned());
    // [策略] 顶部冲突提示摘要最多列出前 6 条冲突防止头部过长
    for c in conflicts.iter().take(6) {
        let vars: Vec<String> = c
            .variants
            .iter()
            .map(|v| {
                format!(
                    "{:?} (independent_sources={})",
                    v.raw_values.first(),
                    v.independent_source_count
                )
            })
            .collect();
        lines.push(format!(
            "- {} / {}: {}",
            c.subject_key,
            c.normalized_field,
            vars.join(" vs ")
        ));
    }
    lines.join("\n")
}
