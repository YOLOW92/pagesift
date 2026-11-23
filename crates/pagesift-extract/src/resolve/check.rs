//! Seven-layer deterministic claim verification engine (`Check`), Carneades proof-standard
//! evaluation, provenance semiring + Minimal Cut Set construction, and ScopeDependency extraction.

use std::collections::{BTreeMap, BTreeSet};

use pagesift_graph::ConflictRecord;
use pagesift_graph::context::{ContextSearchRequest, KnowledgeUnit};
use pagesift_graph::resolve::{
    AtomicClaim, ClaimQuantifier, ClaimVerdict, ConclusionRecord, DEFAULT_VERIFIER_VERSION,
    EpistemicKind, EvidenceBelnap, EvidencePolarity, EvidenceRef, OriginEvent, PremiseRole,
    ProofStandard, ProvenanceExpr, QuantitativeCheck, QuantitativeMismatchKind,
    RESOLVE_SCHEMA_VERSION, ResolutionState, ResolveCheckRequest, ResolveGap, ResolveGapKind,
    ScopeDependency, ScopeDomainKind, compute_minimal_cut_sets,
};
use pagesift_graph::sha256_hex;

use crate::context::calib::ContextLearnState;
use crate::context::search::ContextIndex;
use crate::context::temporal_eval::canonical_origin_root;
use crate::context::tokenize_unified::{analyze_query, analyze_text};
use crate::resolve::decompose::decompose_claims;
use crate::resolve::evidence_eval::{
    build_evidence_ref, evaluate_unit_against_claim, extract_distinctive_entity_labels,
    is_unit_visible, resolve_explicit_citations, scan_comparative_counterexamples,
};
use crate::resolve::learn::ResolveLearnState;

// [策略] 每条声明在本地 ContextIndex 中最多检索前 12 个候选单元进入七层精核
const MAX_CANDIDATE_UNITS_PER_CLAIM: usize = 12;

/// Execute full deterministic Check over a [`ResolveCheckRequest`] against an in-memory [`ContextIndex`].
/// Produces a complete [`ConclusionRecord`] with per-claim Belnap verdicts, ProvenanceExpr,
/// Minimal Cut Sets, ScopeDependency predicates, and prioritized ResolveGaps.
pub fn verify_answer_deterministic(
    req: &ResolveCheckRequest,
    index: &ContextIndex,
    conflicts: &[ConflictRecord],
    learn: &mut ResolveLearnState,
    checked_at_ms: u64,
) -> ConclusionRecord {
    let claims = decompose_claims(&req.answer, &req.claims);
    let mut verdicts = Vec::with_capacity(claims.len());
    let mut ctx_learn = ContextLearnState::default();
    ctx_learn.enabled = false;

    for claim in claims {
        let v = verify_single_claim(claim, req, index, conflicts, &mut ctx_learn, learn);
        verdicts.push(v);
    }

    build_conclusion_record(req, verdicts, index, checked_at_ms, None)
}

/// Assemble a [`ConclusionRecord`] from verified [`ClaimVerdict`] entries (shared by pure deterministic
/// and hybrid LLM-assisted paths).
pub fn build_conclusion_record(
    req: &ResolveCheckRequest,
    verdicts: Vec<ClaimVerdict>,
    index: &ContextIndex,
    checked_at_ms: u64,
    model_id: Option<String>,
) -> ConclusionRecord {
    let (agg_belnap, agg_sup_roots, agg_ref_roots) = aggregate_claim_verdicts(&verdicts);
    let conclusion_state = aggregate_conclusion_state(&verdicts, req.proof_standard);

    let mut dep_units = BTreeSet::new();
    let mut dep_resources = BTreeSet::new();
    let mut all_origin_roots = BTreeSet::new();
    let mut unresolved_gaps = Vec::new();
    let mut exceptions = Vec::new();
    let mut claim_provenances = Vec::new();

    for v in &verdicts {
        for ev in v
            .supporting_evidence
            .iter()
            .chain(v.refuting_evidence.iter())
            .chain(v.qualifying_evidence.iter())
        {
            dep_units.insert(ev.unit_id.clone());
            dep_resources.insert(ev.resource_key.clone());
            if !ev.origin_root.is_empty() {
                all_origin_roots.insert(ev.origin_root.clone());
            }
        }
        for qev in &v.qualifying_evidence {
            exceptions.push(format!("{}: {}", qev.unit_id, qev.match_reason));
        }
        unresolved_gaps.extend(v.gaps.iter().cloned());
        if let Some(p) = &v.provenance {
            claim_provenances.push(p.clone());
        }
    }

    let mut top_and_children = claim_provenances;
    for (idx, a) in req.assumptions.iter().enumerate() {
        top_and_children.push(ProvenanceExpr::Atom {
            event: OriginEvent::Premise {
                premise_id: format!("assumption:{idx}"),
                role: PremiseRole::Assumption,
                label: a.clone(),
            },
            unit_ids: Vec::new(),
        });
    }
    for pid in &req.parent_conclusion_ids {
        top_and_children.push(ProvenanceExpr::Atom {
            event: OriginEvent::UpstreamConclusion {
                conclusion_id: pid.clone(),
            },
            unit_ids: Vec::new(),
        });
    }

    let provenance = match top_and_children.len() {
        0 => None,
        1 => top_and_children.pop(),
        _ => Some(ProvenanceExpr::And {
            children: top_and_children,
        }),
    };
    let cut_set_analysis = provenance.as_ref().map(compute_minimal_cut_sets);

    // Elevate gap criticality for single points of failure discovered by Minimal Cut Set analysis!
    if let Some(ref csa) = cut_set_analysis {
        let spof_keys: BTreeSet<String> = csa
            .single_points_of_failure
            .iter()
            .map(OriginEvent::key)
            .collect();
        for g in &mut unresolved_gaps {
            let is_scope_spof = g.kind == ResolveGapKind::ScopeCompletenessGap
                && spof_keys.iter().any(|k| k.contains("ScopeCompleteness"));
            if is_scope_spof {
                g.criticality = 1.0;
            }
        }
    }
    unresolved_gaps.sort_by(|a, b| {
        b.criticality
            .total_cmp(&a.criticality)
            .then_with(|| a.gap_id.cmp(&b.gap_id))
    });

    let scope = build_scope_dependency(req, &verdicts, index);
    let dep_unit_vec: Vec<String> = dep_units.into_iter().collect();
    let dep_res_vec: Vec<String> = dep_resources.into_iter().collect();
    let answer_text = if !req.answer.trim().is_empty() {
        req.answer.trim().to_owned()
    } else {
        req.claims.join(" ")
    };
    let conclusion_id = req.conclusion_id.clone().unwrap_or_else(|| {
        ConclusionRecord::mint_id(req.query.as_deref(), &answer_text, &req.source_ids)
    });
    let content_digest = ConclusionRecord::compute_digest(
        &answer_text,
        conclusion_state,
        agg_belnap,
        &dep_unit_vec,
        &agg_sup_roots,
        &agg_ref_roots,
    );

    ConclusionRecord {
        schema_version: RESOLVE_SCHEMA_VERSION,
        conclusion_id,
        version: 1,
        content_digest,
        epistemic_kind: EpistemicKind::Conclusion,
        domain: req.domain.clone(),
        query: req.query.clone(),
        answer_text,
        belnap: agg_belnap,
        state: conclusion_state,
        proof_standard: req.proof_standard,
        claims: verdicts,
        scope,
        assumptions: req.assumptions.clone(),
        exceptions,
        parent_conclusion_ids: req.parent_conclusion_ids.clone(),
        dependent_unit_ids: dep_unit_vec,
        dependent_resource_keys: dep_res_vec,
        origin_roots: all_origin_roots.into_iter().collect(),
        independent_support_roots: agg_sup_roots,
        independent_refute_roots: agg_ref_roots,
        provenance,
        cut_set_analysis,
        unresolved_gaps,
        counter_audit: None,
        corrections: Vec::new(),
        stale_reason: None,
        verifier_version: DEFAULT_VERIFIER_VERSION.to_owned(),
        model_id,
        created_at_ms: checked_at_ms,
        updated_at_ms: checked_at_ms,
        checked_at_ms,
    }
}

fn verify_single_claim(
    claim: AtomicClaim,
    req: &ResolveCheckRequest,
    index: &ContextIndex,
    conflicts: &[ConflictRecord],
    ctx_learn: &mut ContextLearnState,
    learn: &mut ResolveLearnState,
) -> ClaimVerdict {
    if !claim.verifiable {
        return ClaimVerdict {
            claim,
            belnap: EvidenceBelnap::UNKNOWN,
            state: ResolutionState::Unverifiable,
            proof_standard: req.proof_standard,
            citation_present: false,
            citation_accessible: false,
            semantic_supported: false,
            temporal_valid: None,
            quantitative_valid: None,
            polarity_consistent: None,
            qualifier_preserved: true,
            causal_identifiable: None,
            scope_sufficient: true,
            calibrated_probability: None,
            raw_alignment_score: 0.0,
            independent_support_roots: Vec::new(),
            independent_refute_roots: Vec::new(),
            total_support_hits: 0,
            total_refute_hits: 0,
            supporting_evidence: Vec::new(),
            refuting_evidence: Vec::new(),
            qualifying_evidence: Vec::new(),
            quantitative_check: None,
            reasons: vec!["statement_classified_as_unverifiable".to_owned()],
            gaps: Vec::new(),
            provenance: None,
        };
    }

    let citation_present = !claim.cited_refs.is_empty();
    let (cited_units, inaccessible_refs) = resolve_explicit_citations(
        &claim.cited_refs,
        index,
        req.as_of_ms,
        req.include_unverified,
    );
    let citation_accessible =
        citation_present && !cited_units.is_empty() && inaccessible_refs.is_empty();

    // Gather candidate units: explicit citations first + ContextIndex multi-strategy retrieval.
    let sreq = ContextSearchRequest {
        query: claim.text.clone(),
        source_ids: req.source_ids.clone(),
        subject_key: req.subject_key.clone(),
        required_fields: Vec::new(),
        as_of_ms: req.as_of_ms,
        effective_at_ms: None, // Keep expired units visible so L4 explicitly catches stale citations!
        limit: MAX_CANDIDATE_UNITS_PER_CLAIM,
        include_unverified: req.include_unverified,
        include_conflicts: true,
        enable_semantic_sketch: true,
        learn_enabled: false,
    };
    let search_report = index.search(&sreq, conflicts, &[], Vec::new(), ctx_learn);

    let mut candidate_units: Vec<(&KnowledgeUnit, bool)> = Vec::new();
    let mut seen_uids: BTreeSet<&str> = BTreeSet::new();
    for u in cited_units {
        if seen_uids.insert(u.unit_id.as_str()) {
            candidate_units.push((u, true));
        }
    }
    for hit in &search_report.hits {
        if let Some(u) = index.units.get(&hit.unit.unit_id)
            && is_unit_visible(u, req.as_of_ms, req.include_unverified)
            && seen_uids.insert(u.unit_id.as_str())
        {
            candidate_units.push((u, false));
        }
    }

    let mut supporting = Vec::new();
    let mut refuting = Vec::new();
    let mut qualifying = Vec::new();
    let mut reasons = Vec::new();
    let mut gaps = Vec::new();

    let mut temporal_valid: Option<bool> = None;
    let mut quantitative_valid: Option<bool> = None;
    let mut polarity_consistent: Option<bool> = None;
    let mut qualifier_preserved = true;
    let mut causal_identifiable: Option<bool> = None;
    let mut quantitative_check: Option<QuantitativeCheck> = None;
    let mut max_support_score = 0.0f32;
    let mut any_temporal_stale = false;
    let mut any_quant_unspecified = false;

    for (u, explicitly_cited) in candidate_units {
        let eval = evaluate_unit_against_claim(&claim, u, req.as_of_ms, req.effective_at_ms);
        if eval.temporal_valid == Some(false) {
            any_temporal_stale = true;
            if temporal_valid != Some(true) {
                temporal_valid = Some(false);
            }
        } else if eval.temporal_valid == Some(true) {
            temporal_valid = Some(true);
        }

        if let Some(ref qchk) = eval.quantitative_check {
            match qchk.kind {
                QuantitativeMismatchKind::Matched => {
                    if quantitative_valid != Some(false) {
                        quantitative_valid = Some(true);
                    }
                    if quantitative_check.is_none() {
                        quantitative_check = Some(qchk.clone());
                    }
                }
                QuantitativeMismatchKind::BillingPeriodMismatch
                | QuantitativeMismatchKind::UnitOrCurrencyMismatch
                | QuantitativeMismatchKind::ValueMismatch => {
                    quantitative_valid = Some(false);
                    quantitative_check = Some(qchk.clone());
                }
                QuantitativeMismatchKind::UnitOrPeriodUnspecified => {
                    any_quant_unspecified = true;
                    if quantitative_check.is_none() {
                        quantitative_check = Some(qchk.clone());
                    }
                }
                QuantitativeMismatchKind::ComparativeCounterexample => {}
            }
        }

        if eval.polarity_consistent == Some(false) {
            polarity_consistent = Some(false);
        } else if eval.polarity_consistent == Some(true) && polarity_consistent.is_none() {
            polarity_consistent = Some(true);
        }

        if !eval.qualifier_preserved {
            qualifier_preserved = false;
        }

        if let Some(ci) = eval.causal_identifiable {
            causal_identifiable = match (causal_identifiable, ci) {
                (Some(true), _) | (_, true) => Some(true),
                _ => Some(false),
            };
        }

        match eval.polarity {
            EvidencePolarity::Supports => {
                max_support_score = max_support_score.max(eval.alignment_score);
                supporting.push(build_evidence_ref(
                    u,
                    EvidencePolarity::Supports,
                    explicitly_cited,
                    eval.excerpt,
                    eval.match_reason,
                ));
            }
            EvidencePolarity::Refutes => {
                reasons.push(eval.match_reason.clone());
                refuting.push(build_evidence_ref(
                    u,
                    EvidencePolarity::Refutes,
                    explicitly_cited,
                    eval.excerpt,
                    eval.match_reason,
                ));
            }
            EvidencePolarity::Qualifies => {
                reasons.push(eval.match_reason.clone());
                qualifying.push(build_evidence_ref(
                    u,
                    EvidencePolarity::Qualifies,
                    explicitly_cited,
                    eval.excerpt,
                    eval.match_reason,
                ));
            }
            EvidencePolarity::ContextualOnly => {
                if explicitly_cited {
                    reasons.push(format!(
                        "cited_unit_does_not_entail_claim:{}",
                        eval.match_reason
                    ));
                }
            }
        }
    }

    // Comparative superlative counterexample scan across the visible corpus!
    let comp_counters = scan_comparative_counterexamples(
        &claim,
        index,
        req.as_of_ms,
        req.effective_at_ms,
        req.include_unverified,
    );
    let has_comparative_refutation = !comp_counters.is_empty();
    for (ev_ref, qchk) in comp_counters {
        reasons.push(qchk.detail.clone());
        quantitative_valid = Some(false);
        quantitative_check = Some(qchk);
        if !refuting
            .iter()
            .any(|r| r.unit_id == ev_ref.unit_id && r.excerpt == ev_ref.excerpt)
        {
            refuting.push(ev_ref);
        }
    }

    // Also inspect active cross-source DataGraph ConflictRecords for this claim's entities/units.
    let supporting_docs: BTreeSet<&str> = supporting.iter().map(|e| e.doc_id.as_str()).collect();
    for conf in conflicts {
        let touches = conf.variants.iter().any(|v| {
            v.evidence
                .iter()
                .any(|ev| supporting_docs.contains(ev.doc_id.as_str()))
        });
        if touches && conf.variants.len() >= 2 {
            for var in &conf.variants {
                for ev in &var.evidence {
                    if !supporting_docs.contains(ev.doc_id.as_str())
                        && let Some(cu) = index.units.values().find(|u| {
                            u.location.doc_id == ev.doc_id
                                && is_unit_visible(u, req.as_of_ms, req.include_unverified)
                        })
                        && !refuting.iter().any(|r| r.unit_id == cu.unit_id)
                    {
                        refuting.push(build_evidence_ref(
                            cu,
                            EvidencePolarity::Refutes,
                            false,
                            pagesift_graph::compact_excerpt(&cu.text, 220), // [策略] 冲突反驳证据摘录上限 220 字符
                            format!("active_datagraph_conflict:{:?}", conf.kind),
                        ));
                        reasons.push(format!("active_datagraph_conflict:{}", conf.conflict_id));
                    }
                }
            }
        }
    }

    // Common-Cause Failure collapse: fold all mirrors/reprints sharing the same `origin_root`!
    let (mut indep_sup_roots, mut sup_units_by_root) = collapse_independent_roots(&supporting);
    let (indep_ref_roots, _) = collapse_independent_roots(&refuting);

    // Logical falsification rules:
    // 1. A concrete comparative counterexample logically falsifies a Superlative claim (supporting
    //    only the entity's own baseline value does NOT establish a multi-source conflict!).
    // 2. When the same `origin_root` itself emits a direct refutation (e.g. sibling table row or
    //    errata from the same publisher), that origin does not simultaneously count as independent
    //    support — genuine `Conflicted` requires distinct independent roots on opposing sides.
    if has_comparative_refutation {
        indep_sup_roots.clear();
        sup_units_by_root.clear();
        reasons.push("comparative_counterexample_falsifies_superlative".to_owned());
    } else if !indep_ref_roots.is_empty() {
        let ref_set: BTreeSet<&str> = indep_ref_roots.iter().map(String::as_str).collect();
        indep_sup_roots.retain(|r| !ref_set.contains(r.as_str()));
        sup_units_by_root.retain(|r, _| !ref_set.contains(r.as_str()));
    }

    let belnap = EvidenceBelnap {
        has_support: !indep_sup_roots.is_empty(),
        has_refutation: !indep_ref_roots.is_empty(),
    };

    let effective_min_indep = match req.proof_standard {
        ProofStandard::ClearAndConvincing | ProofStandard::StrictUniversal => {
            req.min_independent_sources.max(2)
        }
        _ => req.min_independent_sources.max(1),
    };

    let scope_sufficient = !claim.quantifier.is_scope_sensitive()
        || req.domain_kind == ScopeDomainKind::ClosedObservedSet;

    // Determine final auditable ResolutionState honoring all 12 Resolve invariants!
    let state = if !inaccessible_refs.is_empty() && !belnap.has_support && !belnap.has_refutation {
        reasons.push(format!(
            "cited_references_inaccessible:{}",
            inaccessible_refs.join(",")
        ));
        ResolutionState::Insufficient
    } else if claim.asserts_causality && causal_identifiable == Some(false) {
        reasons.push("causal_claim_lacks_identification_evidence".to_owned());
        ResolutionState::CausalUnidentifiable
    } else if belnap.has_support && belnap.has_refutation {
        reasons.push("independent_support_and_refutation_coexist".to_owned());
        ResolutionState::Conflicted
    } else if belnap.has_refutation {
        ResolutionState::Refuted
    } else if claim.quantifier == ClaimQuantifier::NegativeExistential {
        if req.domain_kind == ScopeDomainKind::ClosedObservedSet && !index.units.is_empty() {
            reasons.push("no_contradicting_record_in_closed_observed_set".to_owned());
            ResolutionState::QualifiedInObservedScope
        } else {
            reasons.push("negative_existential_cannot_be_proven_under_open_world".to_owned());
            ResolutionState::ScopeIncomplete
        }
    } else if !belnap.has_support {
        if any_temporal_stale {
            reasons.push("only_temporally_stale_evidence_found".to_owned());
        } else if !qualifier_preserved {
            reasons.push("candidate_evidence_carries_omitted_qualifiers".to_owned());
        } else if any_quant_unspecified {
            reasons.push("unit_currency_or_billing_period_unspecified".to_owned());
        } else {
            reasons.push("no_supporting_evidence_found".to_owned());
        }
        ResolutionState::Insufficient
    } else if !qualifier_preserved {
        reasons.push("evidence_qualifiers_or_exceptions_not_preserved_in_claim".to_owned());
        ResolutionState::QualifiedInObservedScope
    } else if indep_sup_roots.len() < effective_min_indep {
        reasons.push(format!(
            "independent_support_roots_below_standard:observed={},required={effective_min_indep}",
            indep_sup_roots.len()
        ));
        ResolutionState::Insufficient
    } else if claim.quantifier.is_scope_sensitive() && !scope_sufficient {
        if req.proof_standard == ProofStandard::StrictUniversal {
            reasons.push("universal_or_superlative_requires_closed_scope".to_owned());
            ResolutionState::ScopeIncomplete
        } else {
            reasons.push("supported_within_observed_scope_only_open_world".to_owned());
            ResolutionState::QualifiedInObservedScope
        }
    } else {
        ResolutionState::Supported
    };

    // Reverse-generate actionable Gaps from any failure, omission, or incompleteness!
    generate_claim_gaps(
        &claim,
        state,
        &inaccessible_refs,
        any_temporal_stale,
        any_quant_unspecified,
        !qualifier_preserved,
        indep_sup_roots.len(),
        effective_min_indep,
        scope_sufficient,
        &mut gaps,
    );

    // Build symbolic ProvenanceExpr: alternative independent roots in OR, mandatory scope premise in AND.
    let provenance = build_claim_provenance(&claim, &sup_units_by_root);

    let semantic_supported = state.is_supported_like();
    let domain_label = req.domain.as_deref().unwrap_or("general");
    let claim_kind_label = format!("{:?}", claim.quantifier);
    let calibrated_probability = if semantic_supported || belnap.has_support {
        learn.calibrated_probability(
            domain_label,
            &claim_kind_label,
            max_support_score,
            req.learn_enabled,
        )
    } else {
        None
    };

    ClaimVerdict {
        claim,
        belnap,
        state,
        proof_standard: req.proof_standard,
        citation_present,
        citation_accessible,
        semantic_supported,
        temporal_valid,
        quantitative_valid,
        polarity_consistent,
        qualifier_preserved,
        causal_identifiable,
        scope_sufficient,
        calibrated_probability,
        raw_alignment_score: max_support_score,
        independent_support_roots: indep_sup_roots,
        independent_refute_roots: indep_ref_roots,
        total_support_hits: supporting.len(),
        total_refute_hits: refuting.len(),
        supporting_evidence: supporting,
        refuting_evidence: refuting,
        qualifying_evidence: qualifying,
        quantitative_check,
        reasons,
        gaps,
        provenance,
    }
}

fn collapse_independent_roots(
    evidence: &[EvidenceRef],
) -> (Vec<String>, BTreeMap<String, Vec<String>>) {
    let mut by_root: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for ev in evidence {
        let root = if ev.origin_root.trim().is_empty() {
            canonical_origin_root(&ev.resource_key)
        } else {
            canonical_origin_root(&ev.origin_root)
        };
        let slot = by_root.entry(root).or_default();
        if !slot.contains(&ev.unit_id) {
            slot.push(ev.unit_id.clone());
        }
    }
    let roots: Vec<String> = by_root.keys().cloned().collect();
    (roots, by_root)
}

fn build_claim_provenance(
    claim: &AtomicClaim,
    sup_units_by_root: &BTreeMap<String, Vec<String>>,
) -> Option<ProvenanceExpr> {
    let mut root_atoms: Vec<ProvenanceExpr> = sup_units_by_root
        .iter()
        .map(|(root, unit_ids)| ProvenanceExpr::Atom {
            event: OriginEvent::OriginRoot { root: root.clone() },
            unit_ids: unit_ids.clone(),
        })
        .collect();
    let support_branch = match root_atoms.len() {
        0 => None,
        1 => root_atoms.pop(),
        _ => Some(ProvenanceExpr::Or {
            children: root_atoms,
        }),
    };

    if claim.quantifier.is_scope_sensitive() {
        let scope_atom = ProvenanceExpr::Atom {
            event: OriginEvent::Premise {
                premise_id: format!("scope:{}", claim.claim_id),
                role: PremiseRole::ScopeCompleteness,
                label: format!("complete comparison domain for {:?}", claim.quantifier),
            },
            unit_ids: Vec::new(),
        };
        return match support_branch {
            Some(sb) => Some(ProvenanceExpr::And {
                children: vec![sb, scope_atom],
            }),
            None => Some(scope_atom),
        };
    }
    support_branch
}

#[allow(clippy::too_many_arguments)]
fn generate_claim_gaps(
    claim: &AtomicClaim,
    state: ResolutionState,
    inaccessible_refs: &[String],
    any_temporal_stale: bool,
    any_quant_unspecified: bool,
    qualifier_omitted: bool,
    indep_observed: usize,
    min_indep_req: usize,
    scope_sufficient: bool,
    gaps: &mut Vec<ResolveGap>,
) {
    let mk = |kind: ResolveGapKind, target: String, crit: f32, detail: String| ResolveGap {
        gap_id: short_id("rgap", &[&claim.claim_id, &format!("{kind:?}"), &target]),
        kind,
        claim_id: claim.claim_id.clone(),
        target_query: target,
        criticality: crit,
        detail,
        resolved: false,
    };

    if !inaccessible_refs.is_empty() {
        gaps.push(mk(
            ResolveGapKind::InvalidOrInaccessibleCitation,
            inaccessible_refs.join(" "),
            0.95, // [策略] 无效/越时点引用高优先级缺口 0.95
            format!(
                "explicit citation(s) missing or unavailable at requested asOf: {}",
                inaccessible_refs.join(", ")
            ),
        ));
    }
    if state == ResolutionState::CausalUnidentifiable {
        gaps.push(mk(
            ResolveGapKind::CausalIdentificationMissing,
            format!("{} controlled study causal mechanism", claim.text),
            0.90, // [策略] 因果识别缺失优先级 0.90
            "only correlational/co-occurrence text found; causal identification evidence required"
                .to_owned(),
        ));
    }
    if any_temporal_stale {
        gaps.push(mk(
            ResolveGapKind::TemporalCoverageGap,
            format!("{} latest update current", claim.text),
            0.90, // [策略] 时效性过期缺口优先级 0.90
            "existing evidence predates the requested effective time or asserted year".to_owned(),
        ));
    }
    if any_quant_unspecified {
        gaps.push(mk(
            ResolveGapKind::UnitOrPeriodAmbiguous,
            format!("{} currency billing period unit", claim.text),
            0.85, // [策略] 货币/周期/单位缺失优先级 0.85
            "observed number lacks explicit currency, billing period, or measurement unit"
                .to_owned(),
        ));
    }
    if qualifier_omitted {
        gaps.push(mk(
            ResolveGapKind::QualifierOrExceptionUnchecked,
            format!("{} exceptions conditions terms", claim.text),
            0.80, // [策略] 遗漏限定条件/例外核查优先级 0.80
            "source evidence includes conditions or exceptions omitted from the candidate claim"
                .to_owned(),
        ));
    }
    if state == ResolutionState::Conflicted {
        gaps.push(mk(
            ResolveGapKind::UnresolvedConflict,
            format!("{} official correction errata", claim.text),
            0.95, // [策略] 独立信源冲突消解优先级 0.95
            "independent sources disagree; primary authoritative confirmation or errata needed"
                .to_owned(),
        ));
    }
    if state == ResolutionState::Insufficient
        && indep_observed == 0
        && !any_temporal_stale
        && !any_quant_unspecified
        && inaccessible_refs.is_empty()
    {
        gaps.push(mk(
            ResolveGapKind::MissingEvidence,
            claim.text.clone(),
            1.0,
            "no verified grounded unit supports this claim".to_owned(),
        ));
    } else if indep_observed > 0 && indep_observed < min_indep_req {
        gaps.push(mk(
            ResolveGapKind::InsufficientIndependentSources,
            claim.text.clone(),
            0.85, // [策略] 独立信源不足优先级 0.85
            format!(
                "observed {indep_observed} independent origin root(s), below required {min_indep_req}"
            ),
        ));
    }
    if claim.quantifier.is_scope_sensitive() && !scope_sufficient {
        gaps.push(mk(
            ResolveGapKind::ScopeCompletenessGap,
            format!("{} comparison alternatives competitors", claim.text),
            0.95, // [策略] 比较全集/负存在闭合性缺口作为单点脆弱前提赋高优先级 0.95
            format!(
                "{:?} claim evaluated over open-world scope; unobserved entities may falsify it",
                claim.quantifier
            ),
        ));
    }
}

fn aggregate_conclusion_state(
    verdicts: &[ClaimVerdict],
    proof_standard: ProofStandard,
) -> ResolutionState {
    if verdicts.is_empty() {
        return ResolutionState::Unverifiable;
    }
    let verifiable: Vec<&ClaimVerdict> = verdicts.iter().filter(|v| v.claim.verifiable).collect();
    if verifiable.is_empty() {
        return ResolutionState::Unverifiable;
    }
    if verifiable
        .iter()
        .any(|v| v.state == ResolutionState::Refuted)
    {
        return ResolutionState::Refuted;
    }
    if verifiable
        .iter()
        .any(|v| v.state == ResolutionState::Conflicted)
    {
        return ResolutionState::Conflicted;
    }
    if verifiable
        .iter()
        .any(|v| v.state == ResolutionState::CausalUnidentifiable)
    {
        return ResolutionState::CausalUnidentifiable;
    }
    if verifiable
        .iter()
        .any(|v| v.state == ResolutionState::ScopeIncomplete)
    {
        return ResolutionState::ScopeIncomplete;
    }
    if verifiable
        .iter()
        .any(|v| v.state == ResolutionState::Insufficient)
    {
        return ResolutionState::Insufficient;
    }
    if verifiable
        .iter()
        .any(|v| v.state == ResolutionState::QualifiedInObservedScope)
    {
        return if proof_standard == ProofStandard::StrictUniversal {
            ResolutionState::ScopeIncomplete
        } else {
            ResolutionState::QualifiedInObservedScope
        };
    }
    ResolutionState::Supported
}

/// Build explicit [`ScopeDependency`] predicates for incremental negative/range invalidation in Live.
pub fn build_scope_dependency(
    req: &ResolveCheckRequest,
    verdicts: &[ClaimVerdict],
    index: &ContextIndex,
) -> ScopeDependency {
    let mut subject_keys: BTreeSet<String> = BTreeSet::new();
    if let Some(sk) = &req.subject_key {
        subject_keys.insert(sk.clone());
    }
    let mut observed_entities: BTreeSet<String> = BTreeSet::new();
    let mut predicates: BTreeSet<String> = BTreeSet::new();
    let mut has_neg_or_univ = false;

    for sid in &req.source_ids {
        predicates.insert(format!("source:{sid}"));
    }

    for v in verdicts {
        for ent in extract_distinctive_entity_labels(&v.claim) {
            observed_entities.insert(ent);
        }
        for ev in v
            .supporting_evidence
            .iter()
            .chain(v.refuting_evidence.iter())
        {
            if let Some(u) = index.units.get(&ev.unit_id)
                && let Some(sk) = &u.subject_key
            {
                subject_keys.insert(sk.key.clone());
            }
        }
        if v.claim.quantifier.is_scope_sensitive() {
            has_neg_or_univ = true;
            for sid in &req.source_ids {
                predicates.insert(format!("scope_source:{sid}"));
            }
            if req.source_ids.is_empty() {
                predicates.insert("scope_source:*".to_owned());
            }
            let qa = analyze_query(&v.claim.text);
            // [策略] 取前 6 个核心概念词建立范围负依赖谓词索引，兼顾 100% 召回与极低误触发率
            for (stem, _) in qa.lexical_concepts.iter().take(6) {
                // [策略] 过滤长度 < 3 的极短停用词避免范围索引误触发
                if stem.len() >= 3 {
                    predicates.insert(format!("scope_term:{stem}"));
                }
            }
            if qa.features.has_units_or_currency
                || v.claim.text.to_ascii_lowercase().contains("cheap")
                || v.claim.text.to_ascii_lowercase().contains("price")
                || v.claim.text.contains("便宜")
                || v.claim.text.contains("价格")
            {
                predicates.insert("scope_metric:price".to_owned());
            }
        }
    }

    // Record all observed entities in the current domain snapshot when scope-sensitive.
    if has_neg_or_univ {
        for u in index.units.values() {
            if let Some(sk) = &u.subject_key {
                observed_entities.insert(sk.key.clone());
            }
            for a in &u.entity_anchors {
                if let Some(id) = a.strip_prefix("id:") {
                    observed_entities.insert(id.to_owned());
                }
            }
        }
    }

    ScopeDependency {
        domain_kind: req.domain_kind,
        source_ids: req.source_ids.clone(),
        subject_keys: subject_keys.into_iter().collect(),
        observed_entities: observed_entities.into_iter().collect(),
        scope_predicates: predicates.into_iter().collect(),
        has_negative_or_universal_dependency: has_neg_or_univ,
        as_of_ms: req.as_of_ms,
        effective_at_ms: req.effective_at_ms,
    }
}

/// Extract matching scope-predicate keys for a newly appearing or modified [`KnowledgeUnit`]
/// during Live change propagation (dual of [`build_scope_dependency`]).
pub fn unit_scope_trigger_predicates(source_id: &str, unit: &KnowledgeUnit) -> Vec<String> {
    let mut preds = BTreeSet::new();
    preds.insert(format!("scope_source:{source_id}"));
    preds.insert("scope_source:*".to_owned());
    let lex = analyze_text(&unit.text);
    for term in lex.term_freqs.keys() {
        // [策略] 与 build_scope_dependency 对偶，仅索引长度 >= 3 的实质概念词
        if term.len() >= 3 {
            preds.insert(format!("scope_term:{term}"));
        }
    }
    if lex.has_units_or_currency || unit.has_units_or_currency {
        let low = unit.text.to_ascii_lowercase();
        if low.contains('$')
            || low.contains('€')
            || low.contains("usd")
            || low.contains("price")
            || low.contains("cost")
            || unit.text.contains("元")
            || unit.text.contains("价")
        {
            preds.insert("scope_metric:price".to_owned());
        }
    }
    preds.into_iter().collect()
}

pub(crate) fn short_id(prefix: &str, parts: &[&str]) -> String {
    // [策略] 预分配 64 字节初始拼接缓冲区减少小对象扩容
    let mut joined = String::with_capacity(64);
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            joined.push('\x1f');
        }
        joined.push_str(part);
    }
    let hex = sha256_hex(joined.as_bytes());
    // [协议] 取 SHA-256 前 16 位 hex 拼接可读前缀生成确定性短 ID
    format!("{prefix}_{}", &hex[..16])
}

fn aggregate_claim_verdicts(
    verdicts: &[ClaimVerdict],
) -> (EvidenceBelnap, Vec<String>, Vec<String>) {
    let mut sup_roots = BTreeSet::new();
    let mut ref_roots = BTreeSet::new();
    let mut any_verifiable = false;
    let mut all_supported = true;
    let mut any_supported = false;
    let mut any_refuted = false;

    for v in verdicts {
        if !v.claim.verifiable {
            continue;
        }
        any_verifiable = true;
        for r in &v.independent_support_roots {
            sup_roots.insert(r.clone());
        }
        for r in &v.independent_refute_roots {
            ref_roots.insert(r.clone());
        }
        if v.belnap.has_support {
            any_supported = true;
        } else {
            all_supported = false;
        }
        if v.belnap.has_refutation {
            any_refuted = true;
        }
    }

    let belnap = if !any_verifiable {
        EvidenceBelnap::UNKNOWN
    } else {
        EvidenceBelnap {
            has_support: if any_refuted {
                any_supported
            } else {
                all_supported && any_supported
            },
            has_refutation: any_refuted,
        }
    };
    (
        belnap,
        sup_roots.into_iter().collect(),
        ref_roots.into_iter().collect(),
    )
}
