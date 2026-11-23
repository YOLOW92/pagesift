//! Active counter-evidence planning (driven by Minimal Cut Set vulnerability & learned probe rates),
//! deterministic local counter-evidence search, and systematic-review-style coverage auditing.

use std::collections::BTreeSet;

use pagesift_graph::context::KnowledgeUnit;
use pagesift_graph::resolve::{
    ClaimQuantifier, ConclusionRecord, CounterHypothesis, CounterProbeKind, CounterProbePlan,
    CounterProbeResult, CounterSearchAudit, EvidenceBelnap, EvidencePolarity, OriginEvent,
    ResolutionState, ScopeDomainKind,
};

use crate::context::ContextIndex;
use crate::context::temporal_eval::canonical_origin_root;
use crate::context::tokenize_unified::analyze_query;
use crate::resolve::check::short_id;
use crate::resolve::evidence_eval::{
    build_evidence_ref, extract_distinctive_entity_labels, is_unit_visible,
};
use crate::resolve::learn::ResolveLearnState;

// [策略] 每条声明最多规划 4 类结构化反证探针，避免组合爆炸与冗余检索
const MAX_PROBES_PER_CLAIM: usize = 4;

/// Plan adversarial counter-evidence probes prioritized by Minimal Cut Set SPOF criticality
/// and learned probe discovery rates.
pub fn plan_counter_probes(
    conclusion: &ConclusionRecord,
    learn: &ResolveLearnState,
) -> Vec<CounterProbePlan> {
    let spof_roots: BTreeSet<String> = conclusion
        .cut_set_analysis
        .as_ref()
        .map(|csa| {
            csa.single_points_of_failure
                .iter()
                .filter_map(|ev| match ev {
                    OriginEvent::OriginRoot { root } => Some(root.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    let mut plans = Vec::new();
    for verdict in &conclusion.claims {
        if !verdict.claim.verifiable {
            continue;
        }
        let entities = extract_distinctive_entity_labels(&verdict.claim);
        let focus = if !entities.is_empty() {
            entities.join(" ")
        } else {
            let qa = analyze_query(&verdict.claim.text);
            qa.lexical_concepts
                .iter()
                .take(4) // [策略] 无显式实体时取前 4 个查询概念词构造反证聚焦主题
                .map(|(s, _)| s.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        };
        if focus.trim().is_empty() {
            continue;
        }

        let is_spof_claim = verdict
            .independent_support_roots
            .iter()
            .any(|r| spof_roots.contains(r))
            || verdict.claim.quantifier.is_scope_sensitive();
        let spof_boost = if is_spof_claim { 0.25f32 } else { 0.0f32 }; // [策略] 单点脆弱前提提升反证优先级 +0.25

        let mut claim_plans = Vec::with_capacity(MAX_PROBES_PER_CLAIM);

        // 1. Official errata / correction / deprecation / price-change probe
        let errata_rate =
            learn.probe_discovery_rate(CounterProbeKind::OfficialErrataOrUpdate) as f32;
        claim_plans.push(CounterProbePlan {
            probe_id: short_id("cprb", &[&verdict.claim.claim_id, "errata"]),
            kind: CounterProbeKind::OfficialErrataOrUpdate,
            claim_id: verdict.claim.claim_id.clone(),
            query: format!("{focus} errata correction updated deprecated discontinued"),
            target_premise: "source_still_current_without_errata".to_owned(),
            priority: (0.60 + 0.30 * errata_rate + spof_boost).min(1.0), // [策略] 勘误探针基权 0.60 + 学习发现率 0.30
        });

        // 2. Exception & hidden qualifier probe
        let exc_rate = learn.probe_discovery_rate(CounterProbeKind::ExceptionOrQualifier) as f32;
        claim_plans.push(CounterProbePlan {
            probe_id: short_id("cprb", &[&verdict.claim.claim_id, "exception"]),
            kind: CounterProbeKind::ExceptionOrQualifier,
            claim_id: verdict.claim.claim_id.clone(),
            query: format!("{focus} except unless excluding limitation restriction"),
            target_premise: "no_unreported_exceptions_or_conditions".to_owned(),
            priority: (0.55 + 0.30 * exc_rate + spof_boost).min(1.0), // [策略] 例外探针基权 0.55 + 学习发现率 0.30
        });

        // 3. Direct contradiction / competing value probe
        let contra_rate = learn.probe_discovery_rate(CounterProbeKind::DirectContradiction) as f32;
        claim_plans.push(CounterProbePlan {
            probe_id: short_id("cprb", &[&verdict.claim.claim_id, "contradiction"]),
            kind: CounterProbeKind::DirectContradiction,
            claim_id: verdict.claim.claim_id.clone(),
            query: format!("{focus} false incorrect dispute audit discrepancy"),
            target_premise: "claim_uncontested_by_independent_sources".to_owned(),
            priority: (0.50 + 0.30 * contra_rate + spof_boost).min(1.0), // [策略] 直接反驳探针基权 0.50
        });

        // 4. Comparative domain scan for universal / superlative / negative-existential claims
        if verdict.claim.quantifier.is_scope_sensitive() {
            let cmp_rate =
                learn.probe_discovery_rate(CounterProbeKind::ComparativeDomainScan) as f32;
            claim_plans.push(CounterProbePlan {
                probe_id: short_id("cprb", &[&verdict.claim.claim_id, "domain_scan"]),
                kind: CounterProbeKind::ComparativeDomainScan,
                claim_id: verdict.claim.claim_id.clone(),
                query: format!("{focus} competitor alternative cheaper faster incident"),
                target_premise: format!("scope_completeness:{:?}", verdict.claim.quantifier),
                priority: (0.70 + 0.25 * cmp_rate + spof_boost).min(1.0), // [策略] 全称/极值范围扫描最高基权 0.70
            });
        }

        plans.extend(claim_plans);
    }

    plans.sort_by(|a, b| {
        b.priority
            .total_cmp(&a.priority)
            .then_with(|| a.probe_id.cmp(&b.probe_id))
    });
    plans
}

/// Execute deterministic local-corpus counter-evidence probes to surface errata, hidden exceptions,
/// deprecations, and competing hypotheses already present in the corpus without spending network budget.
pub fn run_local_counter_probes(
    conclusion: &mut ConclusionRecord,
    plans: &[CounterProbePlan],
    index: &ContextIndex,
    as_of_ms: Option<u64>,
    include_unverified: bool,
) -> CounterSearchAudit {
    let mut audit = CounterSearchAudit {
        enabled: true,
        probes_planned: plans.len(),
        probes_executed: 0,
        counter_evidence_found: 0,
        independent_roots_checked: Vec::new(),
        domain_kind: conclusion.scope.domain_kind,
        search_failures: Vec::new(),
        blocked_targets: Vec::new(),
        probe_results: Vec::with_capacity(plans.len()),
        competing_hypotheses: Vec::new(),
        coverage_statement: String::new(),
    };

    let visible_units: Vec<&KnowledgeUnit> = index
        .units
        .values()
        .filter(|u| is_unit_visible(u, as_of_ms, include_unverified))
        .collect();

    let mut checked_roots: BTreeSet<String> = BTreeSet::new();
    for u in &visible_units {
        let r = if u.origin_root.trim().is_empty() {
            canonical_origin_root(&u.location.resource_key)
        } else {
            canonical_origin_root(&u.origin_root)
        };
        if !r.is_empty() {
            checked_roots.insert(r);
        }
    }

    for plan in plans {
        let Some(verdict) = conclusion
            .claims
            .iter_mut()
            .find(|v| v.claim.claim_id == plan.claim_id)
        else {
            continue;
        };
        let entities = extract_distinctive_entity_labels(&verdict.claim);
        let qa = analyze_query(&verdict.claim.text);
        // Extract non-entity attribute/predicate stems so exceptions on an unrelated attribute of the
        // same product (e.g. SSO exception) do not falsely qualify a pricing claim on that product.
        let predicate_terms: Vec<&str> = qa
            .lexical_concepts
            .iter()
            .map(|(s, _)| s.as_str())
            .filter(|s| {
                // [策略] 过滤实体词与短词，仅保留 >= 3 字节的属性/谓词概念词做主题相关性守卫
                s.len() >= 3
                    && !entities.iter().any(|e| {
                        e.split_whitespace()
                            .any(|part| part == *s || part.starts_with(s))
                    })
            })
            .collect();
        let mut found_in_probe = 0usize;

        let mut seen_counter_units: BTreeSet<String> = BTreeSet::new();
        for &u in &visible_units {
            let u_low = u.text.to_ascii_lowercase();
            let topic_hit = if !entities.is_empty() {
                entities.iter().any(|e| u_low.contains(e.as_str()))
            } else {
                qa.lexical_concepts
                    .iter()
                    .any(|(s, _)| u_low.contains(s.as_str()))
            };
            if !topic_hit {
                continue;
            }
            let attribute_hit =
                predicate_terms.is_empty() || predicate_terms.iter().any(|p| u_low.contains(p));
            if !attribute_hit
                && matches!(
                    plan.kind,
                    CounterProbeKind::ExceptionOrQualifier | CounterProbeKind::DirectContradiction
                )
            {
                continue;
            }
            if !attribute_hit
                && plan.kind == CounterProbeKind::OfficialErrataOrUpdate
                && !u_low.contains("discontinued")
                && !u_low.contains("retracted")
                && !u.text.contains("已停用")
                && !u.text.contains("已撤回")
            {
                continue;
            }

            let trigger = match plan.kind {
                CounterProbeKind::OfficialErrataOrUpdate => {
                    detect_errata_or_deprecation(&u_low, &u.text)
                }
                CounterProbeKind::ExceptionOrQualifier => {
                    detect_exception_counter(&u_low, &u.text, &verdict.claim.qualifiers)
                }
                CounterProbeKind::DirectContradiction => detect_direct_dispute(&u_low, &u.text),
                CounterProbeKind::ComparativeDomainScan | CounterProbeKind::CompetingHypothesis => {
                    detect_competing_explanation(&u_low, &u.text, verdict.claim.quantifier)
                }
            };

            if let Some((is_refutation, reason_tag)) = trigger {
                let already_refuting = verdict
                    .refuting_evidence
                    .iter()
                    .any(|e| e.unit_id == u.unit_id);
                let already_qualifying = verdict
                    .qualifying_evidence
                    .iter()
                    .any(|e| e.unit_id == u.unit_id);
                if already_refuting
                    || (!is_refutation
                        && !seen_counter_units.insert(u.unit_id.clone())
                        && already_qualifying)
                {
                    continue;
                }
                seen_counter_units.insert(u.unit_id.clone());
                found_in_probe += 1;
                audit.counter_evidence_found += 1;
                let pol = if is_refutation {
                    EvidencePolarity::Refutes
                } else {
                    EvidencePolarity::Qualifies
                };
                let ev_ref = build_evidence_ref(
                    u,
                    pol,
                    false,
                    pagesift_graph::compact_excerpt(&u.text, 220), // [策略] 反证摘录长度上限 220 字符
                    format!("counter_probe:{reason_tag}"),
                );
                let root = ev_ref.origin_root.clone();
                if is_refutation {
                    // An official errata / direct refutation from the same origin root withdraws that root's prior support.
                    verdict.independent_support_roots.retain(|r| r != &root);
                    if !verdict.independent_refute_roots.contains(&root) {
                        verdict.independent_refute_roots.push(root.clone());
                    }
                    verdict.refuting_evidence.push(ev_ref.clone());
                    verdict.total_refute_hits = verdict.refuting_evidence.len();
                    verdict.belnap = EvidenceBelnap {
                        has_support: !verdict.independent_support_roots.is_empty(),
                        has_refutation: true,
                    };
                    verdict.state = if verdict.belnap.has_support {
                        ResolutionState::Conflicted
                    } else {
                        ResolutionState::Refuted
                    };
                    verdict.semantic_supported = false;
                    verdict
                        .reasons
                        .push(format!("counter_probe_found:{reason_tag}"));
                } else {
                    if !already_qualifying {
                        verdict.qualifying_evidence.push(ev_ref.clone());
                    }
                    verdict.qualifier_preserved = false;
                    if verdict.state == ResolutionState::Supported {
                        verdict.state = ResolutionState::QualifiedInObservedScope;
                    }
                    verdict
                        .reasons
                        .push(format!("counter_exception_found:{reason_tag}"));
                }
                let hyp_id = short_id("chyp", &[&plan.claim_id, &u.unit_id]);
                if !audit
                    .competing_hypotheses
                    .iter()
                    .any(|h| h.hypothesis_id == hyp_id)
                {
                    audit.competing_hypotheses.push(CounterHypothesis {
                        hypothesis_id: hyp_id,
                        claim_id: plan.claim_id.clone(),
                        description: format!("{reason_tag}: {}", ev_ref.excerpt),
                        supporting_roots: vec![root],
                        evidence: vec![ev_ref],
                    });
                }
            }
        }

        audit.probes_executed += 1;
        audit.probe_results.push(CounterProbeResult {
            probe_id: plan.probe_id.clone(),
            kind: plan.kind,
            claim_id: plan.claim_id.clone(),
            query: plan.query.clone(),
            executed: true,
            searched_local_units: visible_units.len(),
            searched_network_requests: 0,
            counter_evidence_found: found_in_probe,
            note: if found_in_probe > 0 {
                format!("found_{found_in_probe}_local_counter_item(s)")
            } else {
                "no_local_counterevidence_matched".to_owned()
            },
        });
    }

    // Re-sync top-level conclusion Belnap, roots, state, and dependent units if counter search updated any claim!
    sync_conclusion_after_counter(conclusion, &audit);

    audit.independent_roots_checked = checked_roots.into_iter().collect();
    audit.coverage_statement = if audit.counter_evidence_found > 0 {
        format!(
            "found_{}_counter_or_qualifying_item(s)_across_{}_independent_root(s)",
            audit.counter_evidence_found,
            audit.independent_roots_checked.len()
        )
    } else if audit.domain_kind == ScopeDomainKind::ClosedObservedSet {
        format!(
            "no_counterevidence_found_in_closed_observed_set_of_{}_independent_root(s)_does_not_prove_unobserved_domains",
            audit.independent_roots_checked.len()
        )
    } else {
        format!(
            "no_counterevidence_found_within_searched_open_world_scope_of_{}_independent_root(s)_absence_is_not_proof",
            audit.independent_roots_checked.len()
        )
    };
    audit
}

fn sync_conclusion_after_counter(conclusion: &mut ConclusionRecord, audit: &CounterSearchAudit) {
    if audit.counter_evidence_found == 0 {
        return;
    }
    let mut belnap = EvidenceBelnap::UNKNOWN;
    let mut sup = BTreeSet::new();
    let mut refu = BTreeSet::new();
    let mut dep_u: BTreeSet<String> = conclusion.dependent_unit_ids.iter().cloned().collect();
    let mut dep_r: BTreeSet<String> = conclusion.dependent_resource_keys.iter().cloned().collect();
    let mut roots: BTreeSet<String> = conclusion.origin_roots.iter().cloned().collect();

    for v in &conclusion.claims {
        belnap = belnap.combine(v.belnap);
        sup.extend(v.independent_support_roots.iter().cloned());
        refu.extend(v.independent_refute_roots.iter().cloned());
        for ev in v
            .refuting_evidence
            .iter()
            .chain(v.qualifying_evidence.iter())
        {
            dep_u.insert(ev.unit_id.clone());
            dep_r.insert(ev.resource_key.clone());
            if !ev.origin_root.is_empty() {
                roots.insert(ev.origin_root.clone());
            }
        }
    }

    conclusion.belnap = belnap;
    conclusion.independent_support_roots = sup.into_iter().collect();
    conclusion.independent_refute_roots = refu.into_iter().collect();
    conclusion.dependent_unit_ids = dep_u.into_iter().collect();
    conclusion.dependent_resource_keys = dep_r.into_iter().collect();
    conclusion.origin_roots = roots.into_iter().collect();

    if conclusion
        .claims
        .iter()
        .any(|v| v.state == ResolutionState::Refuted)
    {
        conclusion.state = ResolutionState::Refuted;
    } else if conclusion
        .claims
        .iter()
        .any(|v| v.state == ResolutionState::Conflicted)
    {
        conclusion.state = ResolutionState::Conflicted;
    } else if conclusion
        .claims
        .iter()
        .any(|v| v.state == ResolutionState::QualifiedInObservedScope)
        && conclusion.state == ResolutionState::Supported
    {
        conclusion.state = ResolutionState::QualifiedInObservedScope;
    }
    conclusion.content_digest = ConclusionRecord::compute_digest(
        &conclusion.answer_text,
        conclusion.state,
        conclusion.belnap,
        &conclusion.dependent_unit_ids,
        &conclusion.independent_support_roots,
        &conclusion.independent_refute_roots,
    );
}

fn detect_errata_or_deprecation(low: &str, raw: &str) -> Option<(bool, &'static str)> {
    for pat in [
        "errata:",
        "correction:",
        "retracted",
        "deprecated",
        "discontinued",
        "superseded by",
        "price increased",
        "no longer offered",
        "勘误",
        "更正",
        "已撤回",
        "已停用",
        "涨价",
    ] {
        if low.contains(pat) || raw.contains(pat) {
            return Some((true, "official_errata_or_deprecation"));
        }
    }
    None
}

fn detect_exception_counter(
    low: &str,
    raw: &str,
    claim_qualifiers: &[String],
) -> Option<(bool, &'static str)> {
    if !claim_qualifiers.is_empty() {
        return None;
    }
    for pat in [
        "except ",
        "excluding ",
        "unless ",
        "does not apply to",
        "only applies to",
        "caveat:",
        "limitation:",
        "除外",
        "不含",
        "除非",
        "仅适用于",
        "限制条件",
    ] {
        if low.contains(pat) || raw.contains(pat) {
            return Some((false, "unreported_exception_or_limitation"));
        }
    }
    None
}

fn detect_direct_dispute(low: &str, raw: &str) -> Option<(bool, &'static str)> {
    for pat in [
        "fact check: false",
        "incorrectly claimed",
        "disputed",
        "refuted",
        "actually costs",
        "misleading",
        "不实",
        "辟谣",
        "实际售价",
    ] {
        if low.contains(pat) || raw.contains(pat) {
            return Some((true, "direct_dispute_or_fact_check"));
        }
    }
    None
}

fn detect_competing_explanation(
    low: &str,
    raw: &str,
    quantifier: ClaimQuantifier,
) -> Option<(bool, &'static str)> {
    if quantifier == ClaimQuantifier::NegativeExistential {
        for pat in [
            "incident report",
            "outage",
            "disclosure",
            "published record",
            "公开记录",
            "故障报告",
        ] {
            if low.contains(pat) || raw.contains(pat) {
                return Some((true, "public_record_counterexample"));
            }
        }
    }
    for pat in [
        "alternative explanation",
        "confounding factor",
        "competing hypothesis",
        "另一种解释",
        "混杂因素",
    ] {
        if low.contains(pat) || raw.contains(pat) {
            return Some((false, "competing_hypothesis_identified"));
        }
    }
    None
}
