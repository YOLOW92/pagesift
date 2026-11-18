//! Cross-version [`KnowledgeUnit`] identity reconciliation, counterexample guards,
//! split/merge detection via bounded UOT, and explicit ambiguity preservation.

use std::collections::BTreeMap;

use pagesift_graph::ObservationOutcome;
use pagesift_graph::context::{
    AmbiguousCandidate, KnowledgeUnit, UnitChangeKind, UnitKind, UnitLineage, UnitLineageKind,
    UnitMatchMode, UnitStatus, UnitTransition,
};

use crate::context::tokenize_unified::{LexicalFeatures, analyze_text};
use crate::context::unit_cost::{
    MAX_ACCEPTABLE_COST, build_pruned_edges, has_entity_conflict, is_true_content_merge,
    is_true_content_split,
};
use crate::context::uot::{MAX_UOT_NODES, SparseCostEdge, solve_bounded_uot};

// [策略] 次优竞争候选传输质量达到最优项的 75% 以上且非拆合互补时，拒绝强猜，标记 Ambiguous
const AMBIGUITY_RUNNER_UP_RATIO: f64 = 0.75;
// [策略] 拆分/合并判定中每个子分支至少承载源/目标权重 25% 以上的传输质量且至少存在 2 个分支
const SPLIT_MERGE_SHARE_MIN: f64 = 0.25;

#[derive(Clone, Copy)]
enum UniqueSlot {
    One(usize),
    Many,
}

impl UniqueSlot {
    fn record(slot: &mut Option<Self>, idx: usize) {
        *slot = Some(match *slot {
            None => Self::One(idx),
            Some(_) => Self::Many,
        });
    }
}

/// Output of reconciling a resource's previous active units against a new observation.
#[derive(Debug, Clone)]
pub struct ReconciledUnits {
    /// Active or unverified units representing the resource after this observation.
    pub live_units: Vec<KnowledgeUnit>,
    /// Retired/withdrawn/superseded historical units updated by this observation.
    pub retired_units: Vec<KnowledgeUnit>,
    /// Granular unit-level transitions for [`pagesift_graph::context::ContextDelta`].
    pub transitions: Vec<UnitTransition>,
}

/// Reconcile cross-version [`KnowledgeUnit`]s for one resource, honoring 1.5 observation caution.
pub fn reconcile_resource_units(
    prev_live: &[KnowledgeUnit],
    fresh_scanned: Vec<KnowledgeUnit>,
    outcome: &ObservationOutcome,
    capture_unreliable: bool,
    now_ms: u64,
    mode: UnitMatchMode,
) -> ReconciledUnits {
    // 1. Handle non-content or unreliable observations without false withdrawals!
    if matches!(outcome, ObservationOutcome::NotModified304) && !capture_unreliable {
        let mut live = Vec::with_capacity(prev_live.len());
        let mut transitions = Vec::with_capacity(prev_live.len());
        for u in prev_live {
            let mut next = u.clone();
            next.observed_at_ms = now_ms;
            if matches!(next.status, UnitStatus::Unverified { .. }) {
                next.status = UnitStatus::Active;
            }
            transitions.push(UnitTransition {
                unit_id: next.unit_id.clone(),
                change_kind: UnitChangeKind::Reconfirmed,
                prev_revision_id: Some(u.revision_id.clone()),
                curr_revision_id: Some(next.revision_id.clone()),
                resource_key: next.location.resource_key.clone(),
                doc_id: Some(next.location.doc_id.clone()),
                parent_unit_ids: Vec::new(),
                child_unit_ids: Vec::new(),
                reason: Some("http_304_not_modified".to_owned()),
            });
            live.push(next);
        }
        return ReconciledUnits {
            live_units: live,
            retired_units: Vec::new(),
            transitions,
        };
    }

    if capture_unreliable
        || matches!(
            outcome,
            ObservationOutcome::SuspectEmptyOrBarrier { .. }
                | ObservationOutcome::TransientFetchError { .. }
                | ObservationOutcome::SuspectedGone { .. }
        )
    {
        let reason = if capture_unreliable {
            "capture_unreliable_or_truncated".to_owned()
        } else {
            outcome_reason(outcome)
        };
        let mut live = Vec::with_capacity(prev_live.len());
        let mut transitions = Vec::with_capacity(prev_live.len());
        for u in prev_live {
            let mut next = u.clone();
            next.status = UnitStatus::Unverified {
                since_ms: now_ms,
                reason: reason.clone(),
            };
            transitions.push(UnitTransition {
                unit_id: next.unit_id.clone(),
                change_kind: UnitChangeKind::ObservationLost,
                prev_revision_id: Some(u.revision_id.clone()),
                curr_revision_id: Some(next.revision_id.clone()),
                resource_key: next.location.resource_key.clone(),
                doc_id: Some(next.location.doc_id.clone()),
                parent_unit_ids: Vec::new(),
                child_unit_ids: Vec::new(),
                reason: Some(reason.clone()),
            });
            live.push(next);
        }
        return ReconciledUnits {
            live_units: live,
            retired_units: Vec::new(),
            transitions,
        };
    }

    if let ObservationOutcome::ConfirmedGone { reason } = outcome {
        let mut retired = Vec::with_capacity(prev_live.len());
        let mut transitions = Vec::with_capacity(prev_live.len());
        for u in prev_live {
            let mut dead = u.clone();
            dead.status = UnitStatus::Withdrawn {
                at_ms: now_ms,
                reason: reason.clone(),
            };
            transitions.push(UnitTransition {
                unit_id: dead.unit_id.clone(),
                change_kind: UnitChangeKind::ConfirmedWithdrawn,
                prev_revision_id: Some(u.revision_id.clone()),
                curr_revision_id: None,
                resource_key: dead.location.resource_key.clone(),
                doc_id: None,
                parent_unit_ids: Vec::new(),
                child_unit_ids: Vec::new(),
                reason: Some(reason.clone()),
            });
            retired.push(dead);
        }
        return ReconciledUnits {
            live_units: Vec::new(),
            retired_units: retired,
            transitions,
        };
    }

    // 2. VerifiedContent: first observation fast path
    if prev_live.is_empty() {
        let transitions = fresh_scanned
            .iter()
            .map(|u| UnitTransition {
                unit_id: u.unit_id.clone(),
                change_kind: UnitChangeKind::Appeared,
                prev_revision_id: None,
                curr_revision_id: Some(u.revision_id.clone()),
                resource_key: u.location.resource_key.clone(),
                doc_id: Some(u.location.doc_id.clone()),
                parent_unit_ids: Vec::new(),
                child_unit_ids: Vec::new(),
                reason: None,
            })
            .collect();
        return ReconciledUnits {
            live_units: fresh_scanned,
            retired_units: Vec::new(),
            transitions,
        };
    }

    reconcile_verified_content(prev_live, fresh_scanned, now_ms, mode)
}

fn reconcile_verified_content(
    prev_live: &[KnowledgeUnit],
    mut fresh: Vec<KnowledgeUnit>,
    now_ms: u64,
    mode: UnitMatchMode,
) -> ReconciledUnits {
    let n_old = prev_live.len();
    let n_new = fresh.len();
    let mut old_matched = vec![false; n_old];
    let mut new_matched = vec![false; n_new];
    let mut transitions = Vec::with_capacity(n_new + 4); // [策略] 预留全量新单元 + 少量撤回迁移记录容量
    let mut retired_units = Vec::new();

    // Stage 1A: Zero-clone borrowed `(kind, content_hash, heading_path)` unique match
    let mut stage1a_pairs: Vec<(usize, usize)> = Vec::new();
    {
        let mut old_exact: BTreeMap<(UnitKind, &str, &[String]), Option<UniqueSlot>> =
            BTreeMap::new();
        for (i, u) in prev_live.iter().enumerate() {
            let slot = old_exact
                .entry((
                    u.kind,
                    u.content_hash.as_str(),
                    u.location.heading_path.as_slice(),
                ))
                .or_insert(None);
            UniqueSlot::record(slot, i);
        }
        let mut new_exact: BTreeMap<(UnitKind, &str, &[String]), Option<UniqueSlot>> =
            BTreeMap::new();
        for (j, u) in fresh.iter().enumerate() {
            let slot = new_exact
                .entry((
                    u.kind,
                    u.content_hash.as_str(),
                    u.location.heading_path.as_slice(),
                ))
                .or_insert(None);
            UniqueSlot::record(slot, j);
        }
        for (key, new_slot) in &new_exact {
            if let (Some(UniqueSlot::One(j)), Some(Some(UniqueSlot::One(i)))) =
                (new_slot, old_exact.get(key))
                && !has_entity_conflict(&prev_live[*i], &fresh[*j])
            {
                stage1a_pairs.push((*i, *j));
            }
        }
    }
    for (i, j) in stage1a_pairs {
        old_matched[i] = true;
        new_matched[j] = true;
        inherit_exact_or_relocated(&prev_live[i], &mut fresh[j], &mut transitions);
    }

    // Stage 1B: Patience-style globally unique `(kind, content_hash)` across relocated sections
    let mut stage1b_pairs: Vec<(usize, usize)> = Vec::new();
    {
        let mut rem_old_hash: BTreeMap<(UnitKind, &str), Option<UniqueSlot>> = BTreeMap::new();
        for (i, u) in prev_live.iter().enumerate() {
            if !old_matched[i] {
                let slot = rem_old_hash
                    .entry((u.kind, u.content_hash.as_str()))
                    .or_insert(None);
                UniqueSlot::record(slot, i);
            }
        }
        let mut rem_new_hash: BTreeMap<(UnitKind, &str), Option<UniqueSlot>> = BTreeMap::new();
        for (j, u) in fresh.iter().enumerate() {
            if !new_matched[j] {
                let slot = rem_new_hash
                    .entry((u.kind, u.content_hash.as_str()))
                    .or_insert(None);
                UniqueSlot::record(slot, j);
            }
        }
        for (key, new_slot) in &rem_new_hash {
            if let (Some(UniqueSlot::One(j)), Some(Some(UniqueSlot::One(i)))) =
                (new_slot, rem_old_hash.get(key))
                && !has_entity_conflict(&prev_live[*i], &fresh[*j])
            {
                stage1b_pairs.push((*i, *j));
            }
        }
    }
    for (i, j) in stage1b_pairs {
        old_matched[i] = true;
        new_matched[j] = true;
        inherit_exact_or_relocated(&prev_live[i], &mut fresh[j], &mut transitions);
    }

    // Stage 2 & 3: Local candidate subgraph matching for remaining unmatched units
    let rem_old: Vec<usize> = (0..n_old).filter(|&i| !old_matched[i]).collect();
    let rem_new: Vec<usize> = (0..n_new).filter(|&j| !new_matched[j]).collect();

    if !rem_old.is_empty() && !rem_new.is_empty() && mode != UnitMatchMode::ExactAnchorOnly {
        let old_feats: Vec<LexicalFeatures> = rem_old
            .iter()
            .map(|&i| analyze_text(&prev_live[i].text))
            .collect();
        let new_feats: Vec<LexicalFeatures> = rem_new
            .iter()
            .map(|&j| analyze_text(&fresh[j].text))
            .collect();

        match mode {
            UnitMatchMode::ExactAnchorOnly => {}
            UnitMatchMode::GreedyDice => {
                run_greedy_dice(
                    prev_live,
                    &mut fresh,
                    &rem_old,
                    &rem_new,
                    &old_feats,
                    &new_feats,
                    &mut old_matched,
                    &mut new_matched,
                    &mut transitions,
                );
            }
            UnitMatchMode::BoundedUot => {
                run_chunked_bounded_uot(
                    prev_live,
                    &mut fresh,
                    &rem_old,
                    &rem_new,
                    &old_feats,
                    &new_feats,
                    now_ms,
                    &mut old_matched,
                    &mut new_matched,
                    &mut retired_units,
                    &mut transitions,
                );
            }
        }
    }

    // Finalize unmatched old units -> ConfirmedWithdrawn, unmatched new units -> Appeared
    for (i, old_u) in prev_live.iter().enumerate() {
        if !old_matched[i] {
            let mut dead = old_u.clone();
            dead.status = UnitStatus::Withdrawn {
                at_ms: now_ms,
                reason: "omitted_in_new_version".to_owned(),
            };
            transitions.push(UnitTransition {
                unit_id: dead.unit_id.clone(),
                change_kind: UnitChangeKind::ConfirmedWithdrawn,
                prev_revision_id: Some(old_u.revision_id.clone()),
                curr_revision_id: None,
                resource_key: dead.location.resource_key.clone(),
                doc_id: None,
                parent_unit_ids: Vec::new(),
                child_unit_ids: Vec::new(),
                reason: Some("omitted_in_new_version".to_owned()),
            });
            retired_units.push(dead);
        }
    }

    for (j, new_u) in fresh.iter().enumerate() {
        if !new_matched[j] {
            transitions.push(UnitTransition {
                unit_id: new_u.unit_id.clone(),
                change_kind: UnitChangeKind::Appeared,
                prev_revision_id: None,
                curr_revision_id: Some(new_u.revision_id.clone()),
                resource_key: new_u.location.resource_key.clone(),
                doc_id: Some(new_u.location.doc_id.clone()),
                parent_unit_ids: Vec::new(),
                child_unit_ids: Vec::new(),
                reason: None,
            });
        }
    }

    ReconciledUnits {
        live_units: fresh,
        retired_units,
        transitions,
    }
}

fn inherit_exact_or_relocated(
    old_u: &KnowledgeUnit,
    new_u: &mut KnowledgeUnit,
    transitions: &mut Vec<UnitTransition>,
) {
    let same_pos = old_u.location.byte_start == new_u.location.byte_start
        && old_u.location.heading_path == new_u.location.heading_path;
    new_u.unit_id = old_u.unit_id.clone();
    new_u.revision_id = KnowledgeUnit::compute_revision_id(
        &new_u.unit_id,
        &new_u.content_hash,
        &new_u.claim_ids,
        new_u.temporal.as_ref(),
    );
    new_u.lineage = UnitLineage {
        relation: if same_pos {
            UnitLineageKind::Unchanged
        } else {
            UnitLineageKind::Relocated
        },
        parent_unit_ids: vec![old_u.unit_id.clone()],
        prev_revision_id: Some(old_u.revision_id.clone()),
        confidence: 1.0,
        ambiguous: false,
        candidates: Vec::new(),
        notes: None,
    };
    let change_kind = if matches!(old_u.status, UnitStatus::Unverified { .. })
        || (same_pos && old_u.revision_id != new_u.revision_id)
    {
        UnitChangeKind::EvidenceStateChanged
    } else if same_pos {
        UnitChangeKind::Reconfirmed
    } else {
        UnitChangeKind::RelocatedOnly
    };
    transitions.push(UnitTransition {
        unit_id: new_u.unit_id.clone(),
        change_kind,
        prev_revision_id: Some(old_u.revision_id.clone()),
        curr_revision_id: Some(new_u.revision_id.clone()),
        resource_key: new_u.location.resource_key.clone(),
        doc_id: Some(new_u.location.doc_id.clone()),
        parent_unit_ids: vec![old_u.unit_id.clone()],
        child_unit_ids: Vec::new(),
        reason: None,
    });
}

#[allow(clippy::too_many_arguments)]
fn run_greedy_dice(
    prev_live: &[KnowledgeUnit],
    fresh: &mut [KnowledgeUnit],
    rem_old: &[usize],
    rem_new: &[usize],
    old_feats: &[LexicalFeatures],
    new_feats: &[LexicalFeatures],
    old_matched: &mut [bool],
    new_matched: &mut [bool],
    transitions: &mut Vec<UnitTransition>,
) {
    let mut sorted = build_pruned_edges(prev_live, fresh, rem_old, rem_new, old_feats, new_feats);
    sorted.sort_by(|a, b| a.cost.total_cmp(&b.cost));

    let mut per_new_costs: Vec<Vec<(usize, f64)>> = vec![Vec::new(); rem_new.len()];
    for e in &sorted {
        per_new_costs[e.new_idx].push((e.old_idx, e.cost));
    }

    for e in sorted {
        let oi = rem_old[e.old_idx];
        let nj = rem_new[e.new_idx];
        if old_matched[oi] || new_matched[nj] {
            continue;
        }
        let sim_best = (1.0 - e.cost).max(0.0);
        let runner_up_sim = per_new_costs[e.new_idx]
            .iter()
            .filter(|&&(other_oi, _)| other_oi != e.old_idx)
            .map(|&(_, c)| (1.0 - c).max(0.0))
            .fold(0.0f64, f64::max);

        if sim_best > 0.0 && runner_up_sim / sim_best > AMBIGUITY_RUNNER_UP_RATIO {
            new_matched[nj] = true;
            mark_ambiguous_unit(
                prev_live,
                &mut fresh[nj],
                rem_old,
                &per_new_costs[e.new_idx],
                transitions,
            );
            continue;
        }

        old_matched[oi] = true;
        new_matched[nj] = true;
        inherit_rewritten(&prev_live[oi], &mut fresh[nj], sim_best as f32, transitions);
    }
}

#[allow(clippy::too_many_arguments)]
fn run_chunked_bounded_uot(
    prev_live: &[KnowledgeUnit],
    fresh: &mut [KnowledgeUnit],
    rem_old: &[usize],
    rem_new: &[usize],
    old_feats: &[LexicalFeatures],
    new_feats: &[LexicalFeatures],
    now_ms: u64,
    old_matched: &mut [bool],
    new_matched: &mut [bool],
    retired_units: &mut Vec<KnowledgeUnit>,
    transitions: &mut Vec<UnitTransition>,
) {
    // Proportional co-windowing guarantees both sides stay `<= MAX_UOT_NODES` while covering 100%
    // of `rem_old` and `rem_new` even when their lengths differ asymmetrically (e.g. 70 old vs 60 new)!
    let windows = rem_old
        .len()
        .div_ceil(MAX_UOT_NODES)
        .max(rem_new.len().div_ceil(MAX_UOT_NODES))
        .max(1);

    for w in 0..windows {
        let o_start = (w * rem_old.len()) / windows;
        let o_end = ((w + 1) * rem_old.len()) / windows;
        let n_start = (w * rem_new.len()) / windows;
        let n_end = ((w + 1) * rem_new.len()) / windows;
        if o_start >= o_end || n_start >= n_end {
            continue;
        }

        let sub_old = &rem_old[o_start..o_end];
        let sub_new = &rem_new[n_start..n_end];
        let sub_o_feats = &old_feats[o_start..o_end];
        let sub_n_feats = &new_feats[n_start..n_end];

        let edges =
            build_pruned_edges(prev_live, fresh, sub_old, sub_new, sub_o_feats, sub_n_feats);
        if edges.is_empty() {
            continue;
        }
        let weights_a: Vec<f64> = sub_old
            .iter()
            .map(|&oi| (prev_live[oi].tokens.max(1) as f64).sqrt())
            .collect();
        let weights_b: Vec<f64> = sub_new
            .iter()
            .map(|&nj| (fresh[nj].tokens.max(1) as f64).sqrt())
            .collect();

        let links = solve_bounded_uot(&weights_a, &weights_b, &edges);
        apply_uot_plan(
            prev_live,
            fresh,
            sub_old,
            sub_new,
            sub_o_feats,
            sub_n_feats,
            &weights_a,
            &weights_b,
            &links,
            &edges,
            now_ms,
            old_matched,
            new_matched,
            retired_units,
            transitions,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_uot_plan(
    prev_live: &[KnowledgeUnit],
    fresh: &mut [KnowledgeUnit],
    sub_old: &[usize],
    sub_new: &[usize],
    sub_o_feats: &[LexicalFeatures],
    sub_n_feats: &[LexicalFeatures],
    weights_a: &[f64],
    weights_b: &[f64],
    links: &[crate::context::uot::TransportLink],
    edges: &[SparseCostEdge],
    now_ms: u64,
    old_matched: &mut [bool],
    new_matched: &mut [bool],
    retired_units: &mut Vec<KnowledgeUnit>,
    transitions: &mut Vec<UnitTransition>,
) {
    let mut row_links: Vec<Vec<(usize, f64)>> = vec![Vec::new(); sub_old.len()];
    let mut col_links: Vec<Vec<(usize, f64)>> = vec![Vec::new(); sub_new.len()];
    for l in links {
        row_links[l.old_idx].push((l.new_idx, l.mass));
        col_links[l.new_idx].push((l.old_idx, l.mass));
    }
    for r in &mut row_links {
        r.sort_by(|a, b| b.1.total_cmp(&a.1));
    }
    for c in &mut col_links {
        c.sort_by(|a, b| b.1.total_cmp(&a.1));
    }

    // 1. Detect Split (1 old -> k >= 2 new units where old text actually covers the union of new children)
    for (li, outs) in row_links.iter().enumerate() {
        let oi = sub_old[li];
        if old_matched[oi] {
            continue;
        }
        let children: Vec<(usize, f64)> = outs
            .iter()
            .copied()
            .filter(|&(lj, mass)| {
                !new_matched[sub_new[lj]]
                    // [推导] 1e-6 分母非零保护
                    && mass / weights_b[lj].max(1e-6) >= SPLIT_MERGE_SHARE_MIN
                    && col_links[lj].first().is_some_and(|&(top_i, _)| top_i == li)
            })
            .collect();

        if children.len() >= 2
            && is_true_content_split(
                &prev_live[oi],
                &sub_o_feats[li],
                fresh,
                sub_new,
                sub_n_feats,
                &children,
            )
        {
            old_matched[oi] = true;
            let parent_id = prev_live[oi].unit_id.clone();
            let mut child_ids = Vec::with_capacity(children.len());
            for &(lj, mass) in &children {
                let nj = sub_new[lj];
                new_matched[nj] = true;
                // [策略] 置信度裁剪至 [0.30, 0.99]
                let conf = (mass / weights_b[lj].max(1e-6)).clamp(0.3, 0.99) as f32;
                fresh[nj].lineage = UnitLineage {
                    relation: UnitLineageKind::SplitFrom,
                    parent_unit_ids: vec![parent_id.clone()],
                    prev_revision_id: Some(prev_live[oi].revision_id.clone()),
                    confidence: conf,
                    ambiguous: false,
                    candidates: Vec::new(),
                    notes: Some("split_from_parent_unit".to_owned()),
                };
                child_ids.push(fresh[nj].unit_id.clone());
                transitions.push(UnitTransition {
                    unit_id: fresh[nj].unit_id.clone(),
                    change_kind: UnitChangeKind::Appeared,
                    prev_revision_id: Some(prev_live[oi].revision_id.clone()),
                    curr_revision_id: Some(fresh[nj].revision_id.clone()),
                    resource_key: fresh[nj].location.resource_key.clone(),
                    doc_id: Some(fresh[nj].location.doc_id.clone()),
                    parent_unit_ids: vec![parent_id.clone()],
                    child_unit_ids: Vec::new(),
                    reason: Some("split_child".to_owned()),
                });
            }
            let mut retired = prev_live[oi].clone();
            retired.status = UnitStatus::Superseded {
                by_revision_id: child_ids.join(","),
                at_ms: now_ms,
            };
            retired_units.push(retired);
            transitions.push(UnitTransition {
                unit_id: parent_id,
                change_kind: UnitChangeKind::Split,
                prev_revision_id: Some(prev_live[oi].revision_id.clone()),
                curr_revision_id: None,
                resource_key: prev_live[oi].location.resource_key.clone(),
                doc_id: None,
                parent_unit_ids: Vec::new(),
                child_unit_ids: child_ids,
                reason: Some("split_into_multiple_units".to_owned()),
            });
        }
    }

    // 2. Detect Merge (k >= 2 old units -> 1 new unit covering distinct old parts)
    for (lj, ins) in col_links.iter().enumerate() {
        let nj = sub_new[lj];
        if new_matched[nj] {
            continue;
        }
        let parents: Vec<(usize, f64)> = ins
            .iter()
            .copied()
            .filter(|&(li, mass)| {
                !old_matched[sub_old[li]]
                    // [推导] 1e-6 分母非零保护
                    && mass / weights_a[li].max(1e-6) >= SPLIT_MERGE_SHARE_MIN
                    && row_links[li].first().is_some_and(|&(top_j, _)| top_j == lj)
            })
            .collect();

        if parents.len() >= 2
            && is_true_content_merge(prev_live, &fresh[nj], sub_old, sub_o_feats, &parents)
        {
            new_matched[nj] = true;
            let mut parent_ids = Vec::with_capacity(parents.len());
            for &(li, _) in &parents {
                let oi = sub_old[li];
                old_matched[oi] = true;
                parent_ids.push(prev_live[oi].unit_id.clone());
                let mut retired = prev_live[oi].clone();
                retired.status = UnitStatus::Superseded {
                    by_revision_id: fresh[nj].revision_id.clone(),
                    at_ms: now_ms,
                };
                retired_units.push(retired);
            }
            fresh[nj].lineage = UnitLineage {
                relation: UnitLineageKind::MergedFrom,
                parent_unit_ids: parent_ids.clone(),
                prev_revision_id: None,
                // [策略] 多块互补合并验证通过赋 0.90 置信度
                confidence: 0.90,
                ambiguous: false,
                candidates: Vec::new(),
                notes: Some("merged_from_multiple_units".to_owned()),
            };
            transitions.push(UnitTransition {
                unit_id: fresh[nj].unit_id.clone(),
                change_kind: UnitChangeKind::Merged,
                prev_revision_id: None,
                curr_revision_id: Some(fresh[nj].revision_id.clone()),
                resource_key: fresh[nj].location.resource_key.clone(),
                doc_id: Some(fresh[nj].location.doc_id.clone()),
                parent_unit_ids: parent_ids,
                child_unit_ids: Vec::new(),
                reason: Some("merged_units".to_owned()),
            });
        }
    }

    // 3. Resolve remaining 1-to-1 Rewrites vs Explicit Ambiguity
    let mut cost_lookup: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    for e in edges {
        cost_lookup.insert((e.old_idx, e.new_idx), e.cost);
    }

    for (lj, ins) in col_links.iter().enumerate() {
        let nj = sub_new[lj];
        if new_matched[nj] || ins.is_empty() {
            continue;
        }
        let avail: Vec<(usize, f64)> = ins
            .iter()
            .copied()
            .filter(|&(li, _)| !old_matched[sub_old[li]])
            .collect();
        if avail.is_empty() {
            continue;
        }
        let (best_li, best_mass) = avail[0];
        let best_cost = *cost_lookup
            .get(&(best_li, lj))
            .unwrap_or(&MAX_ACCEPTABLE_COST);
        let best_sim = (1.0 - best_cost).max(0.0);
        let second_mass = avail.get(1).map(|x| x.1).unwrap_or(0.0);
        let row_second_mass = row_links[best_li]
            .iter()
            .filter(|&&(other_lj, _)| other_lj != lj && !new_matched[sub_new[other_lj]])
            .map(|&(_, m)| m)
            .fold(0.0f64, f64::max);
        let rival_raw_sim = (0..sub_new.len())
            .filter(|&other_lj| other_lj != lj && !new_matched[sub_new[other_lj]])
            .filter_map(|other_lj| cost_lookup.get(&(best_li, other_lj)))
            .map(|&c| (1.0 - c).max(0.0))
            .fold(0.0f64, f64::max);

        let rival_mass = second_mass.max(row_second_mass);
        // [策略] 竞争对偶传输质量 > 75% 或原始相似度比值 > 85% 时，拒绝单侧继承，显式保留 Ambiguous
        let raw_sim_ambiguous = best_sim > 0.0 && rival_raw_sim / best_sim > 0.85;
        if (best_mass > 0.0 && rival_mass / best_mass > AMBIGUITY_RUNNER_UP_RATIO)
            || raw_sim_ambiguous
        {
            new_matched[nj] = true;
            let raw_cands: Vec<(usize, f64)> = avail
                .iter()
                .map(|&(li, _)| {
                    (
                        li,
                        *cost_lookup.get(&(li, lj)).unwrap_or(&MAX_ACCEPTABLE_COST),
                    )
                })
                .collect();
            mark_ambiguous_unit(prev_live, &mut fresh[nj], sub_old, &raw_cands, transitions);
            continue;
        }

        let oi = sub_old[best_li];
        let cost = *cost_lookup.get(&(best_li, lj)).unwrap_or(&1.0);
        if cost <= MAX_ACCEPTABLE_COST {
            old_matched[oi] = true;
            new_matched[nj] = true;
            // [策略] 代价转置信度限制在 [0.30, 0.99]
            let conf = (1.0 - cost).clamp(0.30, 0.99) as f32;
            inherit_rewritten(&prev_live[oi], &mut fresh[nj], conf, transitions);
        }
    }
}

fn inherit_rewritten(
    old_u: &KnowledgeUnit,
    new_u: &mut KnowledgeUnit,
    confidence: f32,
    transitions: &mut Vec<UnitTransition>,
) {
    new_u.unit_id = old_u.unit_id.clone();
    new_u.revision_id = KnowledgeUnit::compute_revision_id(
        &new_u.unit_id,
        &new_u.content_hash,
        &new_u.claim_ids,
        new_u.temporal.as_ref(),
    );
    new_u.lineage = UnitLineage {
        relation: UnitLineageKind::Rewritten,
        parent_unit_ids: vec![old_u.unit_id.clone()],
        prev_revision_id: Some(old_u.revision_id.clone()),
        confidence,
        ambiguous: false,
        candidates: Vec::new(),
        notes: None,
    };
    transitions.push(UnitTransition {
        unit_id: new_u.unit_id.clone(),
        change_kind: UnitChangeKind::ContentModified,
        prev_revision_id: Some(old_u.revision_id.clone()),
        curr_revision_id: Some(new_u.revision_id.clone()),
        resource_key: new_u.location.resource_key.clone(),
        doc_id: Some(new_u.location.doc_id.clone()),
        parent_unit_ids: vec![old_u.unit_id.clone()],
        child_unit_ids: Vec::new(),
        reason: None,
    });
}

fn mark_ambiguous_unit(
    prev_live: &[KnowledgeUnit],
    new_u: &mut KnowledgeUnit,
    sub_old: &[usize],
    raw_cands: &[(usize, f64)],
    transitions: &mut Vec<UnitTransition>,
) {
    let cands: Vec<AmbiguousCandidate> = raw_cands
        .iter()
        // [策略] 最多展示前 4 个竞争候选防止元数据膨胀
        .take(4)
        .map(|&(li, cost)| AmbiguousCandidate {
            unit_id: prev_live[sub_old[li]].unit_id.clone(),
            score: (1.0 - cost).clamp(0.0, 1.0) as f32,
            reason: "competing_similarity_candidates".to_owned(),
        })
        .collect();
    let best_score = cands.first().map(|c| c.score).unwrap_or(0.5);
    new_u.lineage = UnitLineage {
        relation: UnitLineageKind::Ambiguous,
        parent_unit_ids: Vec::new(),
        prev_revision_id: None,
        confidence: best_score,
        ambiguous: true,
        candidates: cands,
        notes: Some("ambiguous_identity_not_inherited".to_owned()),
    };
    transitions.push(UnitTransition {
        unit_id: new_u.unit_id.clone(),
        change_kind: UnitChangeKind::AmbiguousLineage,
        prev_revision_id: None,
        curr_revision_id: Some(new_u.revision_id.clone()),
        resource_key: new_u.location.resource_key.clone(),
        doc_id: Some(new_u.location.doc_id.clone()),
        parent_unit_ids: Vec::new(),
        child_unit_ids: Vec::new(),
        reason: Some("competing_candidates_preserved".to_owned()),
    });
}

fn outcome_reason(outcome: &ObservationOutcome) -> String {
    match outcome {
        ObservationOutcome::SuspectEmptyOrBarrier { reason } => {
            format!("barrier_or_empty:{reason}")
        }
        ObservationOutcome::TransientFetchError { error } => format!("transient_error:{error}"),
        ObservationOutcome::SuspectedGone { status_code } => match status_code {
            Some(code) => format!("suspected_gone:{code}"),
            None => "suspected_gone".to_owned(),
        },
        ObservationOutcome::ConfirmedGone { reason } => reason.clone(),
        ObservationOutcome::VerifiedContent => "verified".to_owned(),
        ObservationOutcome::NotModified304 => "not_modified_304".to_owned(),
    }
}
