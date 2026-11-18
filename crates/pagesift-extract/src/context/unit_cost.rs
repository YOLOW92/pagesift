//! Pairwise [`KnowledgeUnit`] similarity cost, deterministic entity/year/table-row conflict guards,
//! and complementary split/merge lexical predicates for cross-version reconciliation.

use std::collections::BTreeMap;

use pagesift_graph::context::{KnowledgeUnit, UnitKind};
use pagesift_graph::parse_timestamp_ms;

use crate::context::tokenize_unified::{
    LexicalFeatures, analyze_text, lexical_jaccard, sketch_similarity,
};
use crate::context::unit_scan::extract_table_primary_keys;
use crate::context::uot::SparseCostEdge;

// [策略] 每个未匹配单元最多向 8 个最高词法重叠的新单元连边，彻底杜绝 O(N*M) 全量代价矩阵
pub(super) const MAX_CANDIDATES_PER_UNIT: usize = 8;
// [推导] 允许有效配对的最大综合距离上界（超过 0.72 即相似度 < 0.28 时拒绝建立伪继承关系）
pub(super) const MAX_ACCEPTABLE_COST: f64 = 0.72;

pub(super) fn build_pruned_edges(
    prev_live: &[KnowledgeUnit],
    fresh: &[KnowledgeUnit],
    sub_old: &[usize],
    sub_new: &[usize],
    old_feats: &[LexicalFeatures],
    new_feats: &[LexicalFeatures],
) -> Vec<SparseCostEdge> {
    let mut edges = Vec::new();
    let mut row: Vec<(usize, f64)> = Vec::with_capacity(sub_new.len());
    for (li, &oi) in sub_old.iter().enumerate() {
        row.clear();
        for (lj, &nj) in sub_new.iter().enumerate() {
            if let Some(cost) =
                pair_cost(&prev_live[oi], &fresh[nj], &old_feats[li], &new_feats[lj])
                && cost <= MAX_ACCEPTABLE_COST
            {
                row.push((lj, cost));
            }
        }
        row.sort_by(|a, b| a.1.total_cmp(&b.1));
        row.truncate(MAX_CANDIDATES_PER_UNIT);
        for &(lj, cost) in &row {
            edges.push(SparseCostEdge {
                old_idx: li,
                new_idx: lj,
                cost,
            });
        }
    }
    edges
}

fn pair_cost(
    old_u: &KnowledgeUnit,
    new_u: &KnowledgeUnit,
    old_f: &LexicalFeatures,
    new_f: &LexicalFeatures,
) -> Option<f64> {
    // Hard guard 1: Incompatible structural classes (e.g., CodeBlock vs Table) never inherit identity
    if !compatible_kinds(old_u.kind, new_u.kind) {
        return None;
    }
    // Hard guard 2: Explicit entity / SKU / year / temporal / table-row-key conflict -> infinite cost!
    if has_entity_conflict(old_u, new_u) {
        return None;
    }

    let lex_sim = f64::from(lexical_jaccard(&old_f.term_freqs, &new_f.term_freqs));
    let sk_sim = f64::from(sketch_similarity(&old_f.sketch, &new_f.sketch));
    let heading_sim =
        heading_path_similarity(&old_u.location.heading_path, &new_u.location.heading_path);

    // Containment score helps Split (child subset of parent) and Merge (parent subset of child)
    let contain = lexical_containment(&old_f.term_freqs, &new_f.term_freqs);

    // [推导] 综合相似度：0.50 词法 Jaccard + 0.25 双向包含率 + 0.15 标题树一致性 + 0.10 子词草图相似度
    let combined_sim = 0.50 * lex_sim + 0.25 * contain + 0.15 * heading_sim + 0.10 * sk_sim;
    Some((1.0 - combined_sim).clamp(0.0, 1.0))
}

pub(super) fn has_entity_conflict(old_u: &KnowledgeUnit, new_u: &KnowledgeUnit) -> bool {
    // 1. Explicit SubjectKey conflict
    if let (Some(sa), Some(sb)) = (&old_u.subject_key, &new_u.subject_key)
        && sa.key != sb.key
    {
        return true;
    }

    // 2. Explicit TemporalSemantics disjoint validity window conflict
    if let (Some(ta), Some(tb)) = (&old_u.temporal, &new_u.temporal) {
        let a_from = ta.valid_from.as_deref().and_then(parse_timestamp_ms);
        let a_until = ta.valid_until.as_deref().and_then(parse_timestamp_ms);
        let b_from = tb.valid_from.as_deref().and_then(parse_timestamp_ms);
        let b_until = tb.valid_until.as_deref().and_then(parse_timestamp_ms);
        if let (Some(au), Some(bf)) = (a_until, b_from)
            && au < bf
        {
            return true;
        }
        if let (Some(bu), Some(af)) = (b_until, a_from)
            && bu < af
        {
            return true;
        }
    }

    // 3. Zero-allocation check on sorted `entity_anchors` (`id:sku-101` vs `id:sku-202`, `year:2024` vs `year:2025`)
    for prefix in ["id:", "year:"] {
        if sorted_prefixed_disjoint(&old_u.entity_anchors, &new_u.entity_anchors, prefix) {
            return true;
        }
    }

    // 4. Leaf section heading conflict:
    //    - If leaf headings differ and carry disjoint explicit `id:` / `year:` anchors (e.g. `## 2025 Plan` vs `## 2026 Plan`), reject!
    //    - If leaf headings differ and body is short identical template boilerplate, reject!
    if let (Some(ha), Some(hb)) = (
        old_u.location.heading_path.last(),
        new_u.location.heading_path.last(),
    ) && ha != hb
    {
        if old_u.content_hash == new_u.content_hash {
            // [策略] <= 24 tokens 的相同模板短句出现在不同实体/章节标题下时禁止跨标题串味继承
            if old_u.tokens <= 24 {
                return true;
            }
        }
        let ha_anchors = analyze_text(ha).entity_anchors;
        let hb_anchors = analyze_text(hb).entity_anchors;
        for prefix in ["id:", "year:"] {
            if sorted_prefixed_disjoint(&ha_anchors, &hb_anchors, prefix) {
                return true;
            }
        }
    }

    // 5. Same-template GFM table with disjoint primary-key rows (e.g., `| Starter | $49 |` vs `| Business | $49 |`)
    if old_u.kind == UnitKind::Table
        && new_u.kind == UnitKind::Table
        && let (Some(ma), Some(mb)) = (&old_u.location.table_meta, &new_u.location.table_meta)
        && !ma.headers.is_empty()
        && ma.headers == mb.headers
        // [策略] <= 4 行的小型明细表/分片在首列实体主键完全互斥时禁止把新商品行误继承为已删商品行
        && ma.row_count <= 4
        && mb.row_count <= 4
    {
        let keys_a = extract_table_primary_keys(&old_u.text);
        let keys_b = extract_table_primary_keys(&new_u.text);
        if !keys_a.is_empty() && !keys_b.is_empty() && keys_a.is_disjoint(&keys_b) {
            return true;
        }
    }

    false
}

/// Allocation-free check on two lexicographically sorted anchor slices:
/// returns `true` iff BOTH slices contain at least one element starting with `prefix` and their sets are disjoint.
fn sorted_prefixed_disjoint(a: &[String], b: &[String], prefix: &str) -> bool {
    let a_start = a.partition_point(|s| s.as_str() < prefix);
    let mut a_end = a_start;
    while a_end < a.len() && a[a_end].starts_with(prefix) {
        a_end += 1;
    }
    if a_start == a_end {
        return false;
    }

    let b_start = b.partition_point(|s| s.as_str() < prefix);
    let mut b_end = b_start;
    while b_end < b.len() && b[b_end].starts_with(prefix) {
        b_end += 1;
    }
    if b_start == b_end {
        return false;
    }

    let mut ia = a_start;
    let mut ib = b_start;
    while ia < a_end && ib < b_end {
        match a[ia].cmp(&b[ib]) {
            std::cmp::Ordering::Equal => return false,
            std::cmp::Ordering::Less => ia += 1,
            std::cmp::Ordering::Greater => ib += 1,
        }
    }
    true
}

pub(super) fn is_true_content_split(
    parent: &KnowledgeUnit,
    parent_feat: &LexicalFeatures,
    fresh: &[KnowledgeUnit],
    sub_new: &[usize],
    sub_n_feats: &[LexicalFeatures],
    children: &[(usize, f64)],
) -> bool {
    // True split: ALL child pairs `(p, q)` must partition `parent` into complementary parts, NOT duplicate copies!
    if children.len() >= 2 {
        for p in 0..children.len() {
            for q in (p + 1)..children.len() {
                let fp = &sub_n_feats[children[p].0];
                let fq = &sub_n_feats[children[q].0];
                let sibling_sim = lexical_jaccard(&fp.term_freqs, &fq.term_freqs);
                // [策略] 若任意两个子分支自身词法重叠 > 0.45，说明存在重复克隆/近似改写副本而非内容互补拆分
                if sibling_sim > 0.45 {
                    return false;
                }
            }
        }
        let full_clones = children
            .iter()
            .filter(|&&(lj, _)| {
                // [策略] 单子块覆盖父块 >= 80% 词项视为整句克隆扩写
                lexical_containment(&parent_feat.term_freqs, &sub_n_feats[lj].term_freqs) >= 0.80
            })
            .count();
        if full_clones >= 2 {
            return false;
        }
    }
    let total_child_tokens: usize = children
        .iter()
        .map(|&(lj, _)| fresh[sub_new[lj]].tokens)
        .sum();
    total_child_tokens * 2 >= parent.tokens
}

pub(super) fn is_true_content_merge(
    prev_live: &[KnowledgeUnit],
    merged: &KnowledgeUnit,
    sub_old: &[usize],
    sub_o_feats: &[LexicalFeatures],
    parents: &[(usize, f64)],
) -> bool {
    if parents.len() >= 2 {
        for p in 0..parents.len() {
            for q in (p + 1)..parents.len() {
                let fp = &sub_o_feats[parents[p].0];
                let fq = &sub_o_feats[parents[q].0];
                let sibling_sim = lexical_jaccard(&fp.term_freqs, &fq.term_freqs);
                // [策略] 若任意两个旧分支自身词法重叠 > 0.70，属于重复段落消重而非内容互补合并
                if sibling_sim > 0.70 {
                    return false;
                }
            }
        }
    }
    merged.tokens * 2
        >= parents
            .iter()
            .map(|&(li, _)| prev_live[sub_old[li]].tokens)
            .sum::<usize>()
}

fn compatible_kinds(a: UnitKind, b: UnitKind) -> bool {
    if a == b {
        return true;
    }
    matches!(
        (a, b),
        (UnitKind::Paragraph, UnitKind::ListGroup)
            | (UnitKind::ListGroup, UnitKind::Paragraph)
            | (UnitKind::Paragraph, UnitKind::Quote)
            | (UnitKind::Quote, UnitKind::Paragraph)
    )
}

fn heading_path_similarity(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.5;
    }
    let common = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    common as f64 / a.len().max(b.len()) as f64
}

fn lexical_containment(a: &BTreeMap<String, u32>, b: &BTreeMap<String, u32>) -> f64 {
    let sum_a: u64 = a.values().map(|&v| u64::from(v)).sum();
    let sum_b: u64 = b.values().map(|&v| u64::from(v)).sum();
    let min_sum = sum_a.min(sum_b);
    if min_sum == 0 {
        return 0.0;
    }
    let mut inter = 0u64;
    let (smaller, larger) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    for (k, &va) in smaller {
        if let Some(&vb) = larger.get(k) {
            inter += u64::from(va.min(vb));
        }
    }
    (inter as f64 / min_sum as f64).clamp(0.0, 1.0)
}
