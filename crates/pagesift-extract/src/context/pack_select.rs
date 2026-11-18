//! Optimization selectors (`SubmodularClosure`, `BoundedDpp`, `MmrGreedy`, `TopKGreedy`) for
//! [`ContextPack`] compilation with exact union-marginal token accounting over shared closure items.

use std::collections::BTreeMap;

use pagesift_graph::context::ContextSearchHit;

use crate::context::dpp::{DppCandidate, select_bounded_dpp_with_costs};
use crate::context::temporal_eval::canonical_origin_root;
use crate::context::tokenize_unified::{LexicalFeatures, QueryAnalysis, lexical_jaccard};

// [推导] 次模目标中独立源根边际覆盖凹奖励系数 alpha = 0.35（同源重复仅在 ln(1 + sum) 内缓增）
const INDEP_ROOT_REWARD_WEIGHT: f64 = 0.35;
// [策略] MMR 相关性与多样性折衷系数 lambda = 0.70（Carbonell & Goldstein 1998 常用基线）
const MMR_LAMBDA: f64 = 0.70;

pub(super) struct CandidateItem {
    pub hit_idx: usize,
    pub header_suffix: String,
    pub rendered_body: String,
    pub cost_tokens: usize,
    pub base_quality: f64,
    pub support_weight: f64,
    pub indep_root_id: Option<usize>,
    pub table_header_injected: bool,
    pub feats: LexicalFeatures,
}

/// Super-unit closure bundle grouping complementary items (e.g. claim + opposing conflict counterpart,
/// or child unit + shared table/section header). Multiple bundles may share common anchor items;
/// `primary_item` points to the substantive payload member (never the prepended shared header),
/// and marginal cost only charges items not yet in `selected_items`!
pub(super) struct ClosureBundle {
    pub members: Vec<usize>,
    pub primary_item: usize,
    pub must_keep: bool,
    pub paired_conflict: bool,
}

#[inline]
pub(super) fn marginal_bundle_cost(
    b: &ClosureBundle,
    items: &[CandidateItem],
    selected_items: &[bool],
) -> usize {
    let mut cost = 0usize;
    for &mi in &b.members {
        if !selected_items[mi] {
            cost = cost.saturating_add(items[mi].cost_tokens);
        }
    }
    cost
}

#[inline]
fn mark_bundle_selected(b: &ClosureBundle, selected_items: &mut [bool]) {
    for &mi in &b.members {
        selected_items[mi] = true;
    }
}

pub(super) fn assign_independent_root_ids(
    hits: &[ContextSearchHit],
) -> (Vec<Option<usize>>, usize) {
    let mut root_to_id: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = Vec::with_capacity(hits.len());
    for h in hits {
        if h.unit.origin_independent && !h.unit.origin_root.trim().is_empty() {
            let canon = canonical_origin_root(&h.unit.origin_root);
            let next_id = root_to_id.len();
            let id = *root_to_id.entry(canon).or_insert(next_id);
            out.push(Some(id));
        } else {
            out.push(None);
        }
    }
    let count = root_to_id.len();
    (out, count)
}

pub(super) fn select_topk_bundles(
    bundles: &[ClosureBundle],
    items: &[CandidateItem],
    budget: usize,
) -> Vec<(usize, f32)> {
    let mut selected_items = vec![false; items.len()];
    let mut spent = 0usize;
    let mut out = Vec::new();

    // Pass 1: lock in affordable `must_keep` closures first
    for (idx, b) in bundles.iter().enumerate() {
        if !b.must_keep {
            continue;
        }
        let m_cost = marginal_bundle_cost(b, items, &selected_items);
        if m_cost > 0 && spent + m_cost <= budget {
            spent += m_cost;
            mark_bundle_selected(b, &mut selected_items);
            out.push((idx, 1.0));
        }
    }

    // Pass 2: rank-order greedy fill on remaining items
    for (idx, b) in bundles.iter().enumerate() {
        if b.must_keep {
            continue;
        }
        let m_cost = marginal_bundle_cost(b, items, &selected_items);
        if m_cost > 0 && spent + m_cost <= budget {
            spent += m_cost;
            mark_bundle_selected(b, &mut selected_items);
            out.push((idx, 1.0 / ((out.len() + 1) as f32)));
        }
    }
    out
}

pub(super) fn select_mmr_bundles(
    bundles: &[ClosureBundle],
    items: &[CandidateItem],
    hits: &[ContextSearchHit],
    budget: usize,
) -> Vec<(usize, f32)> {
    let mut chosen = vec![false; bundles.len()];
    let mut selected_items = vec![false; items.len()];
    let mut selected: Vec<(usize, f32)> = Vec::new();
    let mut spent = 0usize;

    loop {
        let mut best: Option<(usize, f64, usize)> = None;
        for (bi, b) in bundles.iter().enumerate() {
            if chosen[bi] {
                continue;
            }
            let m_cost = marginal_bundle_cost(b, items, &selected_items);
            if m_cost == 0 || spent + m_cost > budget {
                continue;
            }
            let rel: f64 = b
                .members
                .iter()
                .map(|&m| items[m].base_quality)
                .sum::<f64>()
                / (b.members.len().max(1) as f64);
            let max_sim = selected
                .iter()
                .map(|&(prev_bi, _)| bundle_similarity(b, &bundles[prev_bi], items, hits))
                .fold(0.0f64, f64::max);
            let mmr = MMR_LAMBDA * rel - (1.0 - MMR_LAMBDA) * max_sim;
            let score = if b.must_keep { mmr + 1.0 } else { mmr };
            if best.is_none_or(|(_, bs, _)| score > bs) {
                best = Some((bi, score, m_cost));
            }
        }
        let Some((win_bi, win_score, win_cost)) = best else {
            break;
        };
        chosen[win_bi] = true;
        spent += win_cost;
        mark_bundle_selected(&bundles[win_bi], &mut selected_items);
        selected.push((win_bi, win_score as f32));
    }
    selected
}

pub(super) fn select_dpp_bundles(
    bundles: &[ClosureBundle],
    items: &[CandidateItem],
    hits: &[ContextSearchHit],
    budget: usize,
) -> Vec<(usize, f32)> {
    let mut selected_items = vec![false; items.len()];
    let mut chosen_must = Vec::new();
    let mut rem_indices = Vec::new();
    let mut spent = 0usize;

    for (bi, b) in bundles.iter().enumerate() {
        if b.must_keep {
            let m_cost = marginal_bundle_cost(b, items, &selected_items);
            if m_cost > 0 && spent + m_cost <= budget {
                spent += m_cost;
                mark_bundle_selected(b, &mut selected_items);
                chosen_must.push((bi, 1.0f32));
            }
        } else {
            rem_indices.push(bi);
        }
    }

    let rem_budget = budget.saturating_sub(spent);
    let mut active_rem: Vec<usize> = Vec::with_capacity(rem_indices.len());
    let mut dpp_cands: Vec<DppCandidate<'_>> = Vec::with_capacity(rem_indices.len());

    for &bi in &rem_indices {
        let m_cost = marginal_bundle_cost(&bundles[bi], items, &selected_items);
        if m_cost == 0 {
            continue;
        }
        // Use `primary_item` (the substantive payload unit, NOT a prepended shared header `members[0]`)
        // so sibling data rows sharing the same header keep distinct sketches and are never killed as collinear!
        let primary = bundles[bi].primary_item;
        let hit = &hits[items[primary].hit_idx];
        active_rem.push(bi);
        dpp_cands.push(DppCandidate {
            quality: items[primary].base_quality,
            cost_tokens: m_cost,
            sketch: &items[primary].feats.sketch,
            origin_root: &hit.unit.origin_root,
        });
    }

    let mut dpp_selected_items = selected_items;
    let chosen_local = select_bounded_dpp_with_costs(
        &dpp_cands,
        rem_budget,
        &mut dpp_selected_items,
        |sel_items, local_i| marginal_bundle_cost(&bundles[active_rem[local_i]], items, sel_items),
        |sel_items, accepted_local_i| {
            mark_bundle_selected(&bundles[active_rem[accepted_local_i]], sel_items);
        },
    );

    for local_idx in chosen_local {
        let bi = active_rem[local_idx];
        let q = dpp_cands[local_idx].quality as f32;
        chosen_must.push((bi, q));
    }
    chosen_must
}

pub(super) fn select_submodular_closure(
    bundles: &[ClosureBundle],
    items: &[CandidateItem],
    hits: &[ContextSearchHit],
    q_analysis: &QueryAnalysis,
    num_indep_roots: usize,
    budget: usize,
) -> Vec<(usize, f32)> {
    // Precompute flat item-by-facet match table once (eliminating all inner-loop string/map allocations!)
    let (num_facets, item_matches) = precompute_facet_matrix(items, hits, q_analysis);

    // Twin-greedy submodular knapsack (Leskovec et al. 2007 / Lin & Bilmes 2011):
    // compares cost-normalized marginal gain vs pure marginal gain and picks higher total F(S)!
    let sol_ratio = greedy_submodular_pass(
        bundles,
        items,
        num_facets,
        &item_matches,
        num_indep_roots,
        budget,
        true,
    );
    let sol_unit = greedy_submodular_pass(
        bundles,
        items,
        num_facets,
        &item_matches,
        num_indep_roots,
        budget,
        false,
    );
    if sol_ratio.1 >= sol_unit.1 {
        sol_ratio.0
    } else {
        sol_unit.0
    }
}

fn precompute_facet_matrix(
    items: &[CandidateItem],
    hits: &[ContextSearchHit],
    q_analysis: &QueryAnalysis,
) -> (usize, Vec<bool>) {
    let q_concepts = &q_analysis.lexical_concepts;
    let distinct_syms: Vec<&String> = q_analysis
        .features
        .exact_symbols
        .iter()
        .filter(|sym| {
            !q_concepts
                .iter()
                .any(|(stem, alt)| stem == sym.as_str() || alt.as_deref() == Some(sym.as_str()))
        })
        .collect();

    let raw_facets = q_concepts.len() + distinct_syms.len();
    if raw_facets == 0 {
        return (1, vec![true; items.len()]);
    }

    let mut flat = vec![false; items.len() * raw_facets];
    for (ii, it) in items.iter().enumerate() {
        let hit = &hits[it.hit_idx];
        let row = &mut flat[ii * raw_facets..(ii + 1) * raw_facets];
        for (ci, (stem, alt)) in q_concepts.iter().enumerate() {
            let matched = it.feats.term_freqs.contains_key(stem)
                || alt
                    .as_deref()
                    .is_some_and(|a| it.feats.term_freqs.contains_key(a))
                || hit.matched_terms.iter().any(|t| t == stem);
            row[ci] = matched;
        }
        for (si, sym) in distinct_syms.iter().enumerate() {
            row[q_concepts.len() + si] = it.feats.exact_symbols.contains(*sym);
        }
    }
    (raw_facets, flat)
}

fn greedy_submodular_pass(
    bundles: &[ClosureBundle],
    items: &[CandidateItem],
    num_facets: usize,
    item_matches: &[bool],
    num_indep_roots: usize,
    budget: usize,
    divide_by_cost: bool,
) -> (Vec<(usize, f32)>, f64) {
    let mut chosen = vec![false; bundles.len()];
    let mut selected_items = vec![false; items.len()];
    let mut out = Vec::new();
    let mut spent = 0usize;

    let mut facet_cov = vec![0.0f64; num_facets];
    let mut root_mass = vec![0.0f64; num_indep_roots];
    let mut scratch_cov = vec![0.0f64; num_facets];
    let mut scratch_roots = vec![0.0f64; num_indep_roots];
    let mut cur_obj = 0.0f64;

    // 1. Always lock in affordable `must_keep` closure bundles first
    for (bi, b) in bundles.iter().enumerate() {
        if !b.must_keep {
            continue;
        }
        let m_cost = marginal_bundle_cost(b, items, &selected_items);
        if m_cost > 0 && spent + m_cost <= budget {
            let gain = marginal_submodular_gain(
                b,
                items,
                &selected_items,
                num_facets,
                item_matches,
                &facet_cov,
                &root_mass,
                &mut scratch_cov,
                &mut scratch_roots,
            );
            apply_submodular_state(
                b,
                items,
                &mut selected_items,
                num_facets,
                item_matches,
                &mut facet_cov,
                &mut root_mass,
            );
            chosen[bi] = true;
            spent += m_cost;
            cur_obj += gain;
            // [策略] 最小正增益显示下界 0.01
            out.push((bi, gain.max(0.01) as f32));
        }
    }

    // 2. Zero-allocation greedy monotone submodular maximization on remaining budget
    loop {
        let mut best: Option<(usize, f64, f64, usize)> = None;
        for (bi, b) in bundles.iter().enumerate() {
            if chosen[bi] {
                continue;
            }
            let m_cost = marginal_bundle_cost(b, items, &selected_items);
            if m_cost == 0 || spent + m_cost > budget {
                continue;
            }
            let gain = marginal_submodular_gain(
                b,
                items,
                &selected_items,
                num_facets,
                item_matches,
                &facet_cov,
                &root_mass,
                &mut scratch_cov,
                &mut scratch_roots,
            );
            // [推导] 边际效用增益 <= 1e-6（完全冗余的转载副本）停止塞入，节约下游模型 Token 成本
            if gain <= 1e-6 {
                continue;
            }
            let key = if divide_by_cost {
                // [推导] Lin & Bilmes 2011 ACL 块长缩放指数 r=0.65
                gain / (m_cost as f64).powf(0.65)
            } else {
                gain
            };
            if best.is_none_or(|(_, bk, _, _)| key > bk) {
                best = Some((bi, key, gain, m_cost));
            }
        }
        let Some((win_bi, _, win_gain, win_cost)) = best else {
            break;
        };
        apply_submodular_state(
            &bundles[win_bi],
            items,
            &mut selected_items,
            num_facets,
            item_matches,
            &mut facet_cov,
            &mut root_mass,
        );
        chosen[win_bi] = true;
        spent += win_cost;
        cur_obj += win_gain;
        out.push((win_bi, win_gain as f32));
    }

    (out, cur_obj)
}

#[allow(clippy::too_many_arguments)]
fn marginal_submodular_gain(
    b: &ClosureBundle,
    items: &[CandidateItem],
    selected_items: &[bool],
    num_facets: usize,
    item_matches: &[bool],
    facet_cov: &[f64],
    root_mass: &[f64],
    scratch_cov: &mut [f64],
    scratch_roots: &mut [f64],
) -> f64 {
    // Fast allocation-free path for singleton bundles (95%+ of candidates!)
    if b.members.len() == 1 {
        let mi = b.members[0];
        if selected_items[mi] {
            return 0.0;
        }
        let it = &items[mi];
        let q = it.base_quality;
        let row = &item_matches[mi * num_facets..(mi + 1) * num_facets];
        let mut cov_delta = 0.0f64;
        for fi in 0..num_facets {
            if row[fi] && q > facet_cov[fi] {
                cov_delta += q - facet_cov[fi];
            }
        }
        let root_delta = match it.indep_root_id {
            Some(rid) => {
                let old_m = root_mass[rid];
                let new_m = old_m + it.support_weight * q;
                new_m.ln_1p() - old_m.ln_1p()
            }
            None => 0.0,
        };
        return cov_delta + INDEP_ROOT_REWARD_WEIGHT * root_delta;
    }

    // Multi-member closure bundle: evaluate on preallocated scratch buffers (zero heap allocation!)
    scratch_cov.copy_from_slice(facet_cov);
    scratch_roots.copy_from_slice(root_mass);
    for &mi in &b.members {
        if selected_items[mi] {
            continue;
        }
        let it = &items[mi];
        let q = it.base_quality;
        let row = &item_matches[mi * num_facets..(mi + 1) * num_facets];
        for fi in 0..num_facets {
            if row[fi] && q > scratch_cov[fi] {
                scratch_cov[fi] = q;
            }
        }
        if let Some(rid) = it.indep_root_id {
            scratch_roots[rid] += it.support_weight * q;
        }
    }
    let mut cov_delta = 0.0f64;
    for fi in 0..num_facets {
        cov_delta += scratch_cov[fi] - facet_cov[fi];
    }
    let mut root_delta = 0.0f64;
    for rid in 0..root_mass.len() {
        if scratch_roots[rid] > root_mass[rid] {
            root_delta += scratch_roots[rid].ln_1p() - root_mass[rid].ln_1p();
        }
    }
    cov_delta + INDEP_ROOT_REWARD_WEIGHT * root_delta
}

fn apply_submodular_state(
    b: &ClosureBundle,
    items: &[CandidateItem],
    selected_items: &mut [bool],
    num_facets: usize,
    item_matches: &[bool],
    facet_cov: &mut [f64],
    root_mass: &mut [f64],
) {
    for &mi in &b.members {
        if selected_items[mi] {
            continue;
        }
        selected_items[mi] = true;
        let it = &items[mi];
        let q = it.base_quality;
        let row = &item_matches[mi * num_facets..(mi + 1) * num_facets];
        for fi in 0..num_facets {
            if row[fi] && q > facet_cov[fi] {
                facet_cov[fi] = q;
            }
        }
        if let Some(rid) = it.indep_root_id {
            root_mass[rid] += it.support_weight * q;
        }
    }
}

fn bundle_similarity(
    a: &ClosureBundle,
    b: &ClosureBundle,
    items: &[CandidateItem],
    hits: &[ContextSearchHit],
) -> f64 {
    if !a.paired_conflict && !b.paired_conflict {
        return item_similarity(a.primary_item, b.primary_item, items, hits);
    }
    let mut max_s = 0.0f64;
    for &ma in &a.members {
        for &mb in &b.members {
            if ma == mb {
                continue;
            }
            max_s = max_s.max(item_similarity(ma, mb, items, hits));
        }
    }
    max_s
}

fn item_similarity(
    ma: usize,
    mb: usize,
    items: &[CandidateItem],
    hits: &[ContextSearchHit],
) -> f64 {
    let lex = f64::from(lexical_jaccard(
        &items[ma].feats.term_freqs,
        &items[mb].feats.term_freqs,
    ));
    let ua = &hits[items[ma].hit_idx].unit;
    let ub = &hits[items[mb].hit_idx].unit;
    let same_root = (!ua.origin_root.is_empty()
        && canonical_origin_root(&ua.origin_root) == canonical_origin_root(&ub.origin_root))
        || (!ua.origin_independent && !ub.origin_independent);
    if same_root {
        // [策略] 同源转载镜像附加 0.35 相似度惩罚
        (lex + 0.35).min(1.0)
    } else {
        lex
    }
}
