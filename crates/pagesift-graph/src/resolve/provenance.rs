//! Symbolic provenance semiring (Green et al., PODS 2007) combined with Reliability Engineering
//! Fault-Tree Minimal Cut Set & Common-Cause Failure analysis (Vesely et al., NUREG-0492) and
//! deterministic dependency cycle detection.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::resolve::model::PremiseRole;

// [策略] 单个结论最小割集展开硬上限 64（防止深层复杂布尔表达式组合爆炸）
pub const MAX_CUT_SETS: usize = 64;
// [策略] 溯源表达式递归求值最大嵌套深度 16
pub const MAX_PROVENANCE_DEPTH: usize = 16;

/// Basic evidentiary or logical event after Common-Cause Failure (CCF) folding by `origin_root`.
/// Ten syndicated mirrors citing the same primary report collapse to ONE `OriginRoot` event!
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum OriginEvent {
    /// Independent external evidentiary origin root (`canonical_origin_root`).
    OriginRoot { root: String },
    /// Explicit assumption or scope completeness premise whose falsification breaks the conclusion.
    Premise {
        premise_id: String,
        role: PremiseRole,
        label: String,
    },
    /// Upstream derived conclusion (tracked for invalidation, never counted as an independent observation).
    UpstreamConclusion { conclusion_id: String },
}

impl OriginEvent {
    pub fn key(&self) -> String {
        match self {
            Self::OriginRoot { root } => format!("origin:{root}"),
            Self::Premise {
                premise_id, role, ..
            } => format!("premise:{role:?}:{premise_id}"),
            Self::UpstreamConclusion { conclusion_id } => format!("conclusion:{conclusion_id}"),
        }
    }
}

/// Positive Boolean provenance semiring expression `P(c)` (`And` = joint dependency, `Or` = alternative derivation).
/// Note: symbolic provenance algebra is NOT probability addition or multiplication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum ProvenanceExpr {
    Atom {
        event: OriginEvent,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        unit_ids: Vec<String>,
    },
    And {
        children: Vec<ProvenanceExpr>,
    },
    Or {
        children: Vec<ProvenanceExpr>,
    },
}

/// One irreducible cut set: if every event in `events` is falsified/withdrawn, the conclusion loses support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MinimalCutSet {
    pub events: Vec<OriginEvent>,
    pub order: usize,
}

/// Reliability-engineering structural vulnerability analysis of a conclusion's provenance.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutSetAnalysis {
    pub cut_sets: Vec<MinimalCutSet>,
    /// Single-point-of-failure events (`order == 1`): highest priority targets for Counter & Gap.
    pub single_points_of_failure: Vec<OriginEvent>,
    /// Structural criticality score per event key (`sum_{K ni e} 1 / |K|^2`), descending.
    pub event_criticality: BTreeMap<String, f32>,
    /// True when combinatorial expansion hit `MAX_CUT_SETS` or `MAX_PROVENANCE_DEPTH` and degraded conservatively.
    pub truncated: bool,
}

/// Compute common-cause-folded Minimal Cut Sets (MOCUS-style top-down expansion with absorption).
///
/// Gate duality for FAILURE (losing support):
/// - An `And(A, B)` support gate fails if `A` fails OR `B` fails (union of cut sets).
/// - An `Or(A, B)` support gate fails only if `A` fails AND `B` fails (Cartesian product of cut sets).
pub fn compute_minimal_cut_sets(expr: &ProvenanceExpr) -> CutSetAnalysis {
    let mut truncated = false;
    let raw_sets = expand_cut_sets(expr, 0, &mut truncated);
    let minimal = minimize_cut_sets(raw_sets, &mut truncated);

    let mut spof = Vec::new();
    let mut crit: BTreeMap<String, f32> = BTreeMap::new();
    let mut cut_sets = Vec::with_capacity(minimal.len());

    for set in minimal {
        let order = set.len();
        let inv_sq = if order > 0 {
            1.0f32 / ((order * order) as f32)
        } else {
            0.0
        };
        let events: Vec<OriginEvent> = set.into_iter().collect();
        if order == 1
            && let Some(ev) = events.first()
            && !spof.contains(ev)
        {
            spof.push(ev.clone());
        }
        for ev in &events {
            *crit.entry(ev.key()).or_insert(0.0) += inv_sq;
        }
        cut_sets.push(MinimalCutSet { events, order });
    }

    CutSetAnalysis {
        cut_sets,
        single_points_of_failure: spof,
        event_criticality: crit,
        truncated,
    }
}

fn expand_cut_sets(
    expr: &ProvenanceExpr,
    depth: usize,
    truncated: &mut bool,
) -> Vec<BTreeSet<OriginEvent>> {
    if depth >= MAX_PROVENANCE_DEPTH {
        *truncated = true;
        return collect_leaf_singletons(expr);
    }
    match expr {
        ProvenanceExpr::Atom { event, .. } => {
            let mut s = BTreeSet::new();
            s.insert(event.clone());
            vec![s]
        }
        ProvenanceExpr::And { children } => {
            // Support requires ALL children => ANY child failing cuts support => union of child cut sets.
            let mut out = Vec::new();
            for child in children {
                let sub = expand_cut_sets(child, depth + 1, truncated);
                out.extend(sub);
                if out.len() > MAX_CUT_SETS * 2 {
                    out = minimize_cut_sets(out, truncated);
                }
            }
            minimize_cut_sets(out, truncated)
        }
        ProvenanceExpr::Or { children } => {
            // Support requires ANY child => ALL children must fail to cut support => Cartesian product.
            let non_empty: Vec<&ProvenanceExpr> = children.iter().collect();
            if non_empty.is_empty() {
                return Vec::new();
            }
            let mut acc = expand_cut_sets(non_empty[0], depth + 1, truncated);
            for child in &non_empty[1..] {
                let right = expand_cut_sets(child, depth + 1, truncated);
                if right.is_empty() {
                    continue;
                }
                if acc.is_empty() {
                    acc = right;
                    continue;
                }
                let mut next =
                    Vec::with_capacity(acc.len().saturating_mul(right.len()).min(MAX_CUT_SETS * 2));
                'outer: for left_set in &acc {
                    for right_set in &right {
                        let mut merged = left_set.clone();
                        merged.extend(right_set.iter().cloned());
                        next.push(merged);
                        if next.len() >= MAX_CUT_SETS * 2 {
                            *truncated = true;
                            break 'outer;
                        }
                    }
                }
                acc = minimize_cut_sets(next, truncated);
            }
            acc
        }
    }
}

fn collect_leaf_singletons(expr: &ProvenanceExpr) -> Vec<BTreeSet<OriginEvent>> {
    let mut leaves = BTreeSet::new();
    fn visit(e: &ProvenanceExpr, acc: &mut BTreeSet<OriginEvent>) {
        match e {
            ProvenanceExpr::Atom { event, .. } => {
                acc.insert(event.clone());
            }
            ProvenanceExpr::And { children } | ProvenanceExpr::Or { children } => {
                for c in children {
                    visit(c, acc);
                }
            }
        }
    }
    visit(expr, &mut leaves);
    leaves
        .into_iter()
        .take(MAX_CUT_SETS)
        .map(|ev| {
            let mut s = BTreeSet::new();
            s.insert(ev);
            s
        })
        .collect()
}

/// Apply absorption law (`A subseteq B => drop B`), preferring lowest-order (most vulnerable) cut sets.
fn minimize_cut_sets(
    mut raw: Vec<BTreeSet<OriginEvent>>,
    truncated: &mut bool,
) -> Vec<BTreeSet<OriginEvent>> {
    raw.retain(|s| !s.is_empty());
    raw.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    let mut kept: Vec<BTreeSet<OriginEvent>> = Vec::new();
    for cand in raw {
        if kept.iter().any(|existing| existing.is_subset(&cand)) {
            continue;
        }
        if kept.len() >= MAX_CUT_SETS {
            *truncated = true;
            break;
        }
        kept.push(cand);
    }
    kept
}

/// Deterministic cycle detection before inserting `target_id -> parent_ids` into the conclusion dependency graph.
/// Prevents self-citation loops where a conclusion cites itself directly or transitively.
pub fn detect_dependency_cycle<F>(
    target_id: &str,
    proposed_parents: &[String],
    mut get_parents: F,
) -> Option<Vec<String>>
where
    F: FnMut(&str) -> Vec<String>,
{
    for p in proposed_parents {
        if p == target_id {
            return Some(vec![target_id.to_owned(), p.clone()]);
        }
    }
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut stack: Vec<(String, Vec<String>)> = proposed_parents
        .iter()
        .map(|p| (p.clone(), vec![target_id.to_owned(), p.clone()]))
        .collect();

    while let Some((curr, path)) = stack.pop() {
        if !visited.insert(curr.clone()) {
            continue;
        }
        for next in get_parents(&curr) {
            if next == target_id {
                let mut cycle = path;
                cycle.push(next);
                return Some(cycle);
            }
            if !visited.contains(&next) {
                let mut next_path = path.clone();
                next_path.push(next.clone());
                stack.push((next, next_path));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_cut_sets_expose_spof_and_common_cause_collapse() {
        let p_scope = OriginEvent::Premise {
            premise_id: "scope:cheapest".into(),
            role: PremiseRole::ScopeCompleteness,
            label: "all competitors observed".into(),
        };
        let src_a = OriginEvent::OriginRoot {
            root: "vendor-a.example".into(),
        };
        let src_b = OriginEvent::OriginRoot {
            root: "audit-b.example".into(),
        };

        // Conclusion = ScopeCompleteness AND (src_a OR src_b)
        let expr = ProvenanceExpr::And {
            children: vec![
                ProvenanceExpr::Atom {
                    event: p_scope.clone(),
                    unit_ids: vec![],
                },
                ProvenanceExpr::Or {
                    children: vec![
                        ProvenanceExpr::Atom {
                            event: src_a.clone(),
                            unit_ids: vec!["u1".into()],
                        },
                        ProvenanceExpr::Atom {
                            event: src_b.clone(),
                            unit_ids: vec!["u2".into()],
                        },
                    ],
                },
            ],
        };
        let analysis = compute_minimal_cut_sets(&expr);
        assert!(!analysis.truncated);
        assert_eq!(analysis.cut_sets.len(), 2);
        assert_eq!(analysis.single_points_of_failure, vec![p_scope.clone()]);
        assert_eq!(analysis.cut_sets[0].order, 1);
        assert_eq!(analysis.cut_sets[1].order, 2);

        // Common-cause collapse: two mirrors of `src_a` in an OR gate collapse to a single SPOF!
        let mirror_expr = ProvenanceExpr::Or {
            children: vec![
                ProvenanceExpr::Atom {
                    event: src_a.clone(),
                    unit_ids: vec!["u1".into()],
                },
                ProvenanceExpr::Atom {
                    event: src_a.clone(),
                    unit_ids: vec!["u_mirror".into()],
                },
            ],
        };
        let mirror_mcs = compute_minimal_cut_sets(&mirror_expr);
        assert_eq!(mirror_mcs.cut_sets.len(), 1);
        assert_eq!(mirror_mcs.cut_sets[0].order, 1);
        assert_eq!(mirror_mcs.single_points_of_failure, vec![src_a]);
    }

    #[test]
    fn detects_direct_and_transitive_conclusion_cycles() {
        let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
        graph.insert("c2".into(), vec!["c1".into()]);
        graph.insert("c3".into(), vec!["c2".into()]);

        let direct = detect_dependency_cycle("c1", &["c1".into()], |id| {
            graph.get(id).cloned().unwrap_or_default()
        });
        assert!(direct.is_some());

        let transitive = detect_dependency_cycle("c1", &["c3".into()], |id| {
            graph.get(id).cloned().unwrap_or_default()
        });
        assert_eq!(
            transitive,
            Some(vec!["c1".into(), "c3".into(), "c2".into(), "c1".into()])
        );

        let acyclic = detect_dependency_cycle("c4", &["c3".into()], |id| {
            graph.get(id).cloned().unwrap_or_default()
        });
        assert!(acyclic.is_none());
    }
}
