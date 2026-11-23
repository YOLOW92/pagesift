//! Claim-to-evidence alignment helpers: citation resolution, table row/header binding,
//! entity guards, dual-axis temporal checks, polarity/qualifier preservation, causal conservatism,
//! and comparative/negative-existential counterexample scanning.

use std::collections::BTreeSet;

use pagesift_graph::context::{KnowledgeUnit, UnitKind, ValidityStatus};
use pagesift_graph::norm::{MoneyPeriod, NormalizedValue};
use pagesift_graph::resolve::{
    AtomicClaim, ClaimQuantifier, EpistemicKind, EvidencePolarity, EvidenceRef, QuantitativeCheck,
    QuantitativeMismatchKind,
};

use crate::context::ContextIndex;
use crate::context::temporal_eval::{
    canonical_origin_root, collect_unit_years, evaluate_validity, utc_year_from_unix_ms,
};
use crate::context::tokenize_unified::{analyze_query, analyze_text};
use crate::resolve::decompose::{detect_claim_negation, extract_qualifier_clauses};
use crate::resolve::quant::{ExtractedQuantity, compare_quantities, extract_quantities};

/// Result of evaluating one candidate [`KnowledgeUnit`] against an [`AtomicClaim`].
#[derive(Debug, Clone)]
pub(crate) struct UnitAlignmentEval {
    pub polarity: EvidencePolarity,
    pub alignment_score: f32,
    pub temporal_valid: Option<bool>,
    pub quantitative_check: Option<QuantitativeCheck>,
    pub polarity_consistent: Option<bool>,
    pub qualifier_preserved: bool,
    pub causal_identifiable: Option<bool>,
    pub excerpt: String,
    pub match_reason: String,
}

/// Resolve an explicit inline citation string (`unt_...`, URL, `doc_...`, `[ref-N]`, `[N]`)
/// against `index` while strictly enforcing `as_of_ms` observation cutoff (prevents future leakage!).
pub(crate) fn resolve_explicit_citations<'a>(
    cited_refs: &[String],
    index: &'a ContextIndex,
    as_of_ms: Option<u64>,
    include_unverified: bool,
) -> (Vec<&'a KnowledgeUnit>, Vec<String>) {
    let mut resolved: Vec<&'a KnowledgeUnit> = Vec::new();
    let mut seen_ids: BTreeSet<&'a str> = BTreeSet::new();
    let mut inaccessible: Vec<String> = Vec::new();

    // Deterministic ordered list of visible units for `[ref-1]` / `[1]` positional citations.
    let ordered_visible: Vec<&'a KnowledgeUnit> = index
        .units
        .values()
        .filter(|u| {
            as_of_ms.is_none_or(|c| u.observed_at_ms <= c)
                && (include_unverified || u.status.is_verified_active())
                && u.status.is_searchable_default()
        })
        .collect();

    for raw_ref in cited_refs {
        let mut matched_any = false;
        // 1. Direct `unit_id` match
        if let Some(u) = index.units.get(raw_ref)
            && is_unit_visible(u, as_of_ms, include_unverified)
        {
            if seen_ids.insert(u.unit_id.as_str()) {
                resolved.push(u);
            }
            matched_any = true;
        }
        // 2. Positional `[ref-N]` or `[N]` (1-based)
        if !matched_any
            && let Some(num_str) = raw_ref
                .strip_prefix("ref-")
                .or_else(|| raw_ref.strip_prefix('E'))
                .or_else(|| {
                    raw_ref
                        .chars()
                        .all(|c| c.is_ascii_digit())
                        .then_some(raw_ref.as_str())
                })
            && let Ok(one_based) = num_str.parse::<usize>()
            && one_based >= 1
            && let Some(&u) = ordered_visible.get(one_based - 1)
        {
            if seen_ids.insert(u.unit_id.as_str()) {
                resolved.push(u);
            }
            matched_any = true;
        }
        // 3. URL / resource_key / doc_id match
        if !matched_any {
            let norm_ref = pagesift_graph::normalize_resource_key(raw_ref);
            for u in &ordered_visible {
                if u.location.source_url == *raw_ref
                    || u.location.resource_key == norm_ref
                    || u.location.doc_id == *raw_ref
                {
                    if seen_ids.insert(u.unit_id.as_str()) {
                        resolved.push(u);
                    }
                    matched_any = true;
                }
            }
        }
        if !matched_any {
            inaccessible.push(raw_ref.clone());
        }
    }

    (resolved, inaccessible)
}

pub(crate) fn is_unit_visible(
    u: &KnowledgeUnit,
    as_of_ms: Option<u64>,
    include_unverified: bool,
) -> bool {
    if let Some(cutoff) = as_of_ms
        && u.observed_at_ms > cutoff
    {
        return false;
    }
    if !u.status.is_searchable_default() {
        return false;
    }
    include_unverified || u.status.is_verified_active()
}

/// Evaluate whether `unit` supports, refutes, qualifies, or is merely contextual for `claim`.
pub(crate) fn evaluate_unit_against_claim(
    claim: &AtomicClaim,
    unit: &KnowledgeUnit,
    as_of_ms: Option<u64>,
    effective_at_ms: Option<u64>,
) -> UnitAlignmentEval {
    // Extract the most relevant row/segment inside `unit` (binding table headers to row cells!).
    let (focused_text, table_entity_hit) = focus_unit_text_for_claim(claim, unit);
    let claim_q = analyze_query(&claim.text);
    let ev_lex = analyze_text(&focused_text);

    // 1. Lexical concept overlap (excluding pure stopwords & citation tags)
    let total_concepts = claim_q.lexical_concepts.len().max(1);
    let matched_concepts = claim_q
        .lexical_concepts
        .iter()
        .filter(|(stem, alt)| {
            ev_lex.term_freqs.contains_key(stem)
                || alt
                    .as_deref()
                    .is_some_and(|a| ev_lex.term_freqs.contains_key(a))
        })
        .count();
    let concept_overlap = matched_concepts as f32 / total_concepts as f32;

    // 2. Entity & subject alignment guard (prevents cross-entity misleading citations!)
    let claim_distinctive_entities = extract_distinctive_entity_labels(claim);
    let unit_text_low = focused_text.to_ascii_lowercase();
    let heading_low = unit.location.heading_path.join(" ").to_ascii_lowercase();
    let unit_subj_low = unit
        .subject_key
        .as_ref()
        .map(|s| s.key.to_ascii_lowercase())
        .unwrap_or_default();

    let mut entity_matched = true;
    let mut entity_conflict = false;
    if !claim_distinctive_entities.is_empty() {
        let strong_entities: Vec<&String> = claim_distinctive_entities
            .iter()
            .filter(|e| e.contains(' ') || e.chars().any(|c| c.is_ascii_digit()))
            .collect();
        let check_list: &[&String] = if !strong_entities.is_empty() {
            &strong_entities
        } else {
            &[]
        };
        let ent_hits_unit = |ent: &str| {
            unit_text_low.contains(ent) || heading_low.contains(ent) || unit_subj_low.contains(ent)
        };
        let any_hit = if !check_list.is_empty() {
            table_entity_hit || check_list.iter().all(|ent| ent_hits_unit(ent.as_str()))
        } else {
            table_entity_hit
                || claim_distinctive_entities
                    .iter()
                    .any(|ent| ent_hits_unit(ent.as_str()))
        };
        entity_matched = any_hit;
        if !any_hit {
            // Check if unit carries an explicitly different identifier/entity anchor (SKU-B vs SKU-A).
            let claim_ids: BTreeSet<&str> = claim
                .entity_anchors
                .iter()
                .filter_map(|a| a.strip_prefix("id:"))
                .collect();
            let unit_ids: BTreeSet<&str> = unit
                .entity_anchors
                .iter()
                .filter_map(|a| a.strip_prefix("id:"))
                .collect();
            if !claim_ids.is_empty() && !unit_ids.is_empty() && claim_ids.is_disjoint(&unit_ids) {
                entity_conflict = true;
            }
        }
    }

    // [策略] 实体冲突或实体未命中且概念重叠 < 0.35 时直接判定实体失配
    if entity_conflict || (!entity_matched && concept_overlap < 0.35) {
        return UnitAlignmentEval {
            polarity: EvidencePolarity::ContextualOnly,
            alignment_score: 0.10, // [策略] 实体失配证据仅保留低相关性分 0.10，绝不标为支持
            temporal_valid: None,
            quantitative_check: None,
            polarity_consistent: None,
            qualifier_preserved: true,
            causal_identifiable: None,
            excerpt: pagesift_graph::compact_excerpt(&focused_text, 220), // [策略] 证据摘要上限 220 字符
            match_reason: "entity_or_subject_mismatch".to_owned(),
        };
    }

    // 3. Temporal interval & asserted-year check (Allen 1983 + dual-axis temporal_eval)
    let claim_years: BTreeSet<u64> = claim.asserted_years.iter().copied().collect();
    let validity = evaluate_validity(unit, effective_at_ms, as_of_ms, &claim_years);
    let unit_years = collect_unit_years(unit);
    let claims_current_time = claim_asserts_present_tense(&claim.text);
    let temporal_valid = match validity {
        ValidityStatus::ExpiredAtTarget | ValidityStatus::NotYetValidAtTarget => Some(false),
        ValidityStatus::ValidAtTarget => Some(true),
        ValidityStatus::UnknownValidity => {
            if let Some(target_ms) = effective_at_ms {
                let target_yr = utc_year_from_unix_ms(target_ms);
                if !unit_years.is_empty() && unit_years.iter().all(|&y| y < target_yr) {
                    Some(false)
                } else {
                    None
                }
            } else if !claim_years.is_empty() && !unit_years.is_empty() {
                if !claim_years.is_disjoint(&unit_years) {
                    Some(true)
                } else {
                    Some(false)
                }
            } else if claims_current_time && !unit_years.is_empty() {
                // Claim says "currently/still leads" while evidence is explicitly anchored to an older year
                // relative to `as_of_ms` or unit observation year.
                let ref_ms = as_of_ms.unwrap_or(unit.observed_at_ms);
                let ref_yr = if ref_ms > 0 {
                    utc_year_from_unix_ms(ref_ms)
                } else {
                    0
                };
                if ref_yr > 0 && unit_years.iter().all(|&y| y < ref_yr) {
                    Some(false)
                } else {
                    None
                }
            } else {
                None
            }
        }
    };

    if temporal_valid == Some(false) {
        return UnitAlignmentEval {
            polarity: EvidencePolarity::ContextualOnly,
            alignment_score: 0.20, // [策略] 时间失效/年份过时证据降为 ContextualOnly（0.20）拒绝作为有效支持
            temporal_valid: Some(false),
            quantitative_check: None,
            polarity_consistent: None,
            qualifier_preserved: true,
            causal_identifiable: None,
            excerpt: pagesift_graph::compact_excerpt(&focused_text, 220), // [策略] 证据摘要上限 220 字符
            match_reason: "temporal_validity_mismatch_or_stale_year".to_owned(),
        };
    }

    // 4. Deterministic quantitative, currency, billing-period, and unit check
    let claim_quants = extract_quantities(&claim.text);
    let ev_quants = extract_quantities(&focused_text);
    let mut best_quant_check: Option<QuantitativeCheck> = None;

    if !claim_quants.is_empty() && !ev_quants.is_empty() {
        for cq in &claim_quants {
            // Find the closest comparable quantity in evidence (prefer same monetary/unit class)
            let mut candidate_check: Option<QuantitativeCheck> = None;
            for eq in &ev_quants {
                if cq.is_money != eq.is_money
                    && cq.canonical_unit_dim.is_none()
                    && eq.canonical_unit_dim.is_none()
                {
                    continue;
                }
                let cmp = compare_quantities(cq, eq);
                if cmp.kind == QuantitativeMismatchKind::Matched {
                    candidate_check = Some(cmp);
                    break;
                }
                if candidate_check.is_none()
                    || matches!(
                        cmp.kind,
                        QuantitativeMismatchKind::BillingPeriodMismatch
                            | QuantitativeMismatchKind::UnitOrCurrencyMismatch
                    )
                {
                    candidate_check = Some(cmp);
                }
            }
            if let Some(chk) = candidate_check {
                let is_hard_mismatch = matches!(
                    chk.kind,
                    QuantitativeMismatchKind::BillingPeriodMismatch
                        | QuantitativeMismatchKind::UnitOrCurrencyMismatch
                        | QuantitativeMismatchKind::ValueMismatch
                );
                best_quant_check = Some(chk);
                if is_hard_mismatch {
                    break;
                }
            }
        }
    }

    if let Some(ref qchk) = best_quant_check {
        match qchk.kind {
            QuantitativeMismatchKind::BillingPeriodMismatch
            | QuantitativeMismatchKind::UnitOrCurrencyMismatch
            | QuantitativeMismatchKind::ValueMismatch => {
                return UnitAlignmentEval {
                    polarity: EvidencePolarity::Refutes,
                    alignment_score: 0.15, // [策略] 数值/币种/周期硬矛盾判定为 Refutes
                    temporal_valid,
                    quantitative_check: Some(qchk.clone()),
                    polarity_consistent: Some(false),
                    qualifier_preserved: true,
                    causal_identifiable: None,
                    excerpt: pagesift_graph::compact_excerpt(&focused_text, 220), // [策略] 证据摘要上限 220 字符
                    match_reason: qchk.detail.clone(),
                };
            }
            QuantitativeMismatchKind::UnitOrPeriodUnspecified => {
                return UnitAlignmentEval {
                    polarity: EvidencePolarity::Qualifies,
                    alignment_score: 0.40, // [策略] 币种或周期缺失不判支持，标为 Qualifies 触发补证缺口
                    temporal_valid,
                    quantitative_check: Some(qchk.clone()),
                    polarity_consistent: None,
                    qualifier_preserved: false,
                    causal_identifiable: None,
                    excerpt: pagesift_graph::compact_excerpt(&focused_text, 220), // [策略] 证据摘要上限 220 字符
                    match_reason: qchk.detail.clone(),
                };
            }
            QuantitativeMismatchKind::Matched
            | QuantitativeMismatchKind::ComparativeCounterexample => {}
        }
    }

    // 5. Polarity (negation flip) & qualifier omission guard
    let ev_low = focused_text.to_ascii_lowercase();
    let ev_negated = detect_claim_negation(&ev_low, &focused_text);
    let ev_qualifiers = extract_qualifier_clauses(&focused_text, &ev_low);

    // Check if evidence restricts scope (`in tests`, `in benchmark`, `only for enterprise`, `unless ...`)
    // while the claim dropped those qualifiers or escalated to universal scope.
    let qualifier_omitted = (!ev_qualifiers.is_empty() && claim.qualifiers.is_empty())
        || (evidence_is_benchmark_scoped(&ev_low, &focused_text)
            && claim.quantifier.is_scope_sensitive());

    let polarity_consistent = Some(claim.negated == ev_negated);
    // [策略] 概念重叠 >= 0.35 且主体命中时，正负逻辑极性反转判定为反驳或例外限定
    if claim.negated != ev_negated && concept_overlap >= 0.35 && entity_matched {
        // Distinguish direct polarity contradiction from qualifier-induced negation (`unless` / `except`).
        let negation_only_in_qualifier = ev_qualifiers
            .iter()
            .any(|q| detect_claim_negation(&q.to_ascii_lowercase(), q))
            && !claim.negated;
        if negation_only_in_qualifier {
            return UnitAlignmentEval {
                polarity: EvidencePolarity::Qualifies,
                alignment_score: 0.45, // [策略] 原文含否定性例外从句而被声明省略，标为 Qualifies
                temporal_valid,
                quantitative_check: best_quant_check,
                polarity_consistent: Some(false),
                qualifier_preserved: false,
                causal_identifiable: None,
                excerpt: pagesift_graph::compact_excerpt(&focused_text, 220), // [策略] 证据摘要上限 220 字符
                match_reason: "evidence_contains_omitted_exception_qualifier".to_owned(),
            };
        }
        return UnitAlignmentEval {
            polarity: EvidencePolarity::Refutes,
            alignment_score: 0.15, // [策略] 主体一致但正负逻辑极性反转判定为 Refutes
            temporal_valid,
            quantitative_check: best_quant_check,
            polarity_consistent: Some(false),
            qualifier_preserved: !qualifier_omitted,
            causal_identifiable: None,
            excerpt: pagesift_graph::compact_excerpt(&focused_text, 220), // [策略] 证据摘要上限 220 字符
            match_reason: "polarity_contradiction_between_claim_and_evidence".to_owned(),
        };
    }

    // [策略] 概念重叠 >= 0.35 且声明省略了原文限定从句时标为 Qualifies
    if qualifier_omitted && concept_overlap >= 0.35 {
        return UnitAlignmentEval {
            polarity: EvidencePolarity::Qualifies,
            alignment_score: 0.48, // [策略] 原文带限定条件而声明省略条件，拒标无条件支持
            temporal_valid,
            quantitative_check: best_quant_check,
            polarity_consistent,
            qualifier_preserved: false,
            causal_identifiable: None,
            excerpt: pagesift_graph::compact_excerpt(&focused_text, 220), // [策略] 证据摘要上限 220 字符
            match_reason: format!(
                "omitted_evidence_qualifier:{}",
                ev_qualifiers
                    .first()
                    .map(String::as_str)
                    .unwrap_or("scoped_benchmark_condition")
            ),
        };
    }

    // 6. Causal conservatism guard (Hernan & Robins 2020)
    let causal_identifiable = if claim.asserts_causality {
        let ev_has_identification = evidence_supports_causal_identification(&ev_low, &focused_text);
        let ev_only_correlational = evidence_is_correlational(&ev_low, &focused_text);
        if ev_has_identification {
            Some(true)
        // [策略] 无因果识别设计且仅相关或重叠 >= 0.30 时保守判为不可识别（CausalUnidentifiable）
        } else if ev_only_correlational || concept_overlap >= 0.30 {
            Some(false)
        } else {
            None
        }
    } else {
        None
    };

    if causal_identifiable == Some(false) {
        return UnitAlignmentEval {
            polarity: EvidencePolarity::ContextualOnly,
            alignment_score: 0.35, // [策略] 相关性证据不能证明因果声明，标为 ContextualOnly
            temporal_valid,
            quantitative_check: best_quant_check,
            polarity_consistent,
            qualifier_preserved: true,
            causal_identifiable: Some(false),
            excerpt: pagesift_graph::compact_excerpt(&focused_text, 220), // [策略] 证据摘要上限 220 字符
            match_reason: "correlation_does_not_establish_causation".to_owned(),
        };
    }

    // 7. Negative-existential refutation: claim asserts "no public record / does not exist"
    // while an active affirmative record matching the topic exists in the corpus!
    // [策略] 存在概念重叠 >= 0.45 且主体命中的肯定记录时直接证伪负存在性声明
    if claim.quantifier == ClaimQuantifier::NegativeExistential
        && !ev_negated
        && concept_overlap >= 0.45
        && entity_matched
    {
        return UnitAlignmentEval {
            polarity: EvidencePolarity::Refutes,
            alignment_score: 0.20, // [策略] 存在正向公开记录直接反驳“无记录/不存在”声明
            temporal_valid,
            quantitative_check: best_quant_check,
            polarity_consistent: Some(false),
            qualifier_preserved: true,
            causal_identifiable,
            excerpt: pagesift_graph::compact_excerpt(&focused_text, 220), // [策略] 证据摘要上限 220 字符
            match_reason: "affirmative_record_refutes_negative_existential".to_owned(),
        };
    }

    // 8. Support threshold check
    let quant_bonus = if best_quant_check
        .as_ref()
        .is_some_and(|q| q.kind == QuantitativeMismatchKind::Matched)
    {
        0.35f32 // [策略] 确定性数值+单位+周期精确命中加权 0.35
    } else {
        0.0f32
    };
    let entity_bonus = if entity_matched && !claim_distinctive_entities.is_empty() {
        0.20f32 // [策略] 判别实体锚点命中加权 0.20
    } else {
        0.0f32
    };
    // [推导] 原始对齐分 = 0.55*概念覆盖 + 数值精确命中奖(0.35) + 实体锚点奖(0.20)，截断至 [0,1]
    let score = (0.55 * concept_overlap + quant_bonus + entity_bonus).clamp(0.0, 1.0);

    // Require BOTH sufficient lexical overlap AND matched quantitative value when claim specifies numbers!
    let quant_requirement_met = if claim_quants.is_empty() {
        true
    } else {
        best_quant_check
            .as_ref()
            .is_some_and(|q| q.kind == QuantitativeMismatchKind::Matched)
    };
    // [策略] 支持门槛：语义覆盖 >= 0.45 且满足数值要求，或实体+数值同时精确命中且覆盖 >= 0.25
    let supported = entity_matched
        && quant_requirement_met
        && ((concept_overlap >= 0.45)
            || (quant_bonus > 0.0 && entity_matched && concept_overlap >= 0.25));

    if supported {
        UnitAlignmentEval {
            polarity: EvidencePolarity::Supports,
            alignment_score: score,
            temporal_valid,
            quantitative_check: best_quant_check,
            polarity_consistent,
            qualifier_preserved: true,
            causal_identifiable,
            excerpt: pagesift_graph::compact_excerpt(&focused_text, 220), // [策略] 证据摘要上限 220 字符
            match_reason: if quant_bonus > 0.0 {
                "grounded_entity_and_quantitative_match".to_owned()
            } else {
                "grounded_lexical_and_entity_match".to_owned()
            },
        }
    } else {
        UnitAlignmentEval {
            polarity: EvidencePolarity::ContextualOnly,
            alignment_score: score * 0.5,
            temporal_valid,
            quantitative_check: best_quant_check,
            polarity_consistent,
            qualifier_preserved: true,
            causal_identifiable,
            excerpt: pagesift_graph::compact_excerpt(&focused_text, 220), // [策略] 证据摘要上限 220 字符
            match_reason: if !quant_requirement_met {
                "claim_quantity_not_grounded_in_evidence".to_owned()
            } else {
                "insufficient_semantic_overlap".to_owned()
            },
        }
    }
}

/// Scan the visible corpus for comparative counterexamples to a `Superlative` claim
/// (e.g. "Vendor X is the cheapest at $49/mo" refuted by "Vendor Y costs $29/mo").
pub(crate) fn scan_comparative_counterexamples(
    claim: &AtomicClaim,
    index: &ContextIndex,
    as_of_ms: Option<u64>,
    effective_at_ms: Option<u64>,
    include_unverified: bool,
) -> Vec<(EvidenceRef, QuantitativeCheck)> {
    if claim.quantifier != ClaimQuantifier::Superlative {
        return Vec::new();
    }
    let low = claim.text.to_ascii_lowercase();
    let wants_min = low.contains("cheapest")
        || low.contains("lowest")
        || low.contains("least expensive")
        || low.contains("fastest")
        || claim.text.contains("最便宜")
        || claim.text.contains("最低")
        || claim.text.contains("最快");
    let wants_max = low.contains("highest")
        || low.contains("most expensive")
        || low.contains("largest")
        || claim.text.contains("最高")
        || claim.text.contains("最大");
    if !wants_min && !wants_max {
        return Vec::new();
    }

    let claim_quants = extract_quantities(&claim.text);
    // Find the benchmark quantity from either the claim text itself or the supporting unit of the claimed entity.
    let claim_entities = extract_distinctive_entity_labels(claim);
    let mut ref_quant: Option<ExtractedQuantity> = claim_quants.into_iter().next();

    if ref_quant.is_none() && !claim_entities.is_empty() {
        for u in index.units.values() {
            if !is_unit_visible(u, as_of_ms, include_unverified) {
                continue;
            }
            let (focused, hit) = focus_unit_text_for_claim(claim, u);
            let fl = focused.to_ascii_lowercase();
            if hit || claim_entities.iter().any(|e| fl.contains(e.as_str())) {
                let qs = extract_quantities(&focused);
                if let Some(q) = qs.into_iter().next() {
                    ref_quant = Some(q);
                    break;
                }
            }
        }
    }
    let Some(target_q) = ref_quant else {
        return Vec::new();
    };

    let claim_years: BTreeSet<u64> = claim.asserted_years.iter().copied().collect();
    let mut counterexamples = Vec::new();

    for u in index.units.values() {
        if !is_unit_visible(u, as_of_ms, include_unverified) {
            continue;
        }
        let val = evaluate_validity(u, effective_at_ms, as_of_ms, &claim_years);
        if matches!(
            val,
            ValidityStatus::ExpiredAtTarget | ValidityStatus::NotYetValidAtTarget
        ) {
            continue;
        }

        // For tables, inspect each row individually so sibling competitors in the same table are caught!
        let segments = unit_comparison_segments(u);
        for seg in segments {
            let seg_low = seg.to_ascii_lowercase();
            // Skip rows/units describing the exact same entity claimed as the superlative winner.
            if !claim_entities.is_empty()
                && claim_entities.iter().any(|e| seg_low.contains(e.as_str()))
            {
                continue;
            }
            for cand_q in extract_quantities(&seg) {
                if let Some(better_detail) =
                    is_strictly_better_competitor(&target_q, &cand_q, wants_min)
                {
                    let qchk = QuantitativeCheck {
                        kind: QuantitativeMismatchKind::ComparativeCounterexample,
                        claimed_value: Some(target_q.value.clone()),
                        observed_value: Some(cand_q.value.clone()),
                        detail: better_detail.clone(),
                    };
                    let ev_ref = build_evidence_ref(
                        u,
                        EvidencePolarity::Refutes,
                        false,
                        pagesift_graph::compact_excerpt(&seg, 220), // [策略] 反例证据摘要上限 220 字符
                        format!("comparative_counterexample:{better_detail}"),
                    );
                    counterexamples.push((ev_ref, qchk));
                    break;
                }
            }
        }
    }

    counterexamples
}

fn is_strictly_better_competitor(
    claimed: &ExtractedQuantity,
    competitor: &ExtractedQuantity,
    wants_min: bool,
) -> Option<String> {
    match (&claimed.value, &competitor.value) {
        (
            NormalizedValue::Money {
                amount_e4: a1,
                currency: c1,
                period: p1,
                ..
            },
            NormalizedValue::Money {
                amount_e4: a2,
                currency: c2,
                period: p2,
                ..
            },
        ) => {
            if c1 == "UNKNOWN" || c2 == "UNKNOWN" || c1 != c2 {
                return None;
            }
            if *p1 != MoneyPeriod::Unknown && *p2 != MoneyPeriod::Unknown && p1 != p2 {
                return None;
            }
            if (wants_min && a2 < a1) || (!wants_min && a2 > a1) {
                return Some(format!(
                    "competitor '{}' beats claimed superlative '{}'",
                    competitor.raw_span, claimed.raw_span
                ));
            }
            None
        }
        _ => {
            if let (Some(d1), Some(d2), Some(s1), Some(s2)) = (
                claimed.canonical_unit_dim,
                competitor.canonical_unit_dim,
                claimed.base_scalar,
                competitor.base_scalar,
            ) && d1 == d2
                && ((wants_min && s2 < s1) || (!wants_min && s2 > s1))
            {
                return Some(format!(
                    "competitor '{}' beats claimed superlative '{}'",
                    competitor.raw_span, claimed.raw_span
                ));
            }
            None
        }
    }
}

/// Bind Markdown table headers + captions to individual rows so numeric verification aligns
/// strictly to the matching entity's row and column semantics (prevents misleading citations!).
fn focus_unit_text_for_claim(claim: &AtomicClaim, unit: &KnowledgeUnit) -> (String, bool) {
    let claim_entities = extract_distinctive_entity_labels(claim);
    if unit.kind != UnitKind::Table {
        let focused = crate::resolve::decompose::focus_paragraph_sentence(
            &claim.text,
            &claim_entities,
            &unit.text,
        );
        return (focused, false);
    }
    let headers: Vec<String> = unit
        .location
        .table_meta
        .as_ref()
        .map(|tm| tm.headers.clone())
        .unwrap_or_else(|| parse_markdown_table_headers(&unit.text));
    let caption = unit
        .location
        .table_meta
        .as_ref()
        .and_then(|tm| tm.caption.as_deref())
        .unwrap_or_default();

    let rows = parse_markdown_table_rows(&unit.text);
    if rows.is_empty() {
        return (unit.text.clone(), false);
    }

    // Score each row by entity match first, then query concept overlap.
    let q = analyze_query(&claim.text);
    let mut best_row: Option<(usize, usize, String)> = None;
    for row in &rows {
        let row_enriched = render_row_with_headers(caption, &headers, row);
        let row_low = row_enriched.to_ascii_lowercase();
        let ent_hits = claim_entities
            .iter()
            .filter(|e| row_low.contains(e.as_str()))
            .count();
        let lex = analyze_text(&row_enriched);
        let term_hits = q
            .lexical_concepts
            .iter()
            .filter(|(s, a)| {
                lex.term_freqs.contains_key(s)
                    || a.as_deref()
                        .is_some_and(|al| lex.term_freqs.contains_key(al))
            })
            .count();
        if best_row
            .as_ref()
            .is_none_or(|(be, bt, _)| (ent_hits, term_hits) > (*be, *bt))
        {
            best_row = Some((ent_hits, term_hits, row_enriched));
        }
    }
    match best_row {
        Some((ent_hits, _, text)) => (text, ent_hits > 0),
        None => (unit.text.clone(), false),
    }
}

fn unit_comparison_segments(unit: &KnowledgeUnit) -> Vec<String> {
    if unit.kind != UnitKind::Table {
        return vec![unit.text.clone()];
    }
    let headers = unit
        .location
        .table_meta
        .as_ref()
        .map(|tm| tm.headers.clone())
        .unwrap_or_else(|| parse_markdown_table_headers(&unit.text));
    let caption = unit
        .location
        .table_meta
        .as_ref()
        .and_then(|tm| tm.caption.as_deref())
        .unwrap_or_default();
    let rows = parse_markdown_table_rows(&unit.text);
    if rows.is_empty() {
        return vec![unit.text.clone()];
    }
    rows.iter()
        .map(|r| render_row_with_headers(caption, &headers, r))
        .collect()
}

fn parse_markdown_table_headers(md: &str) -> Vec<String> {
    for line in md.lines() {
        let t = line.trim();
        if t.starts_with('|') && t.ends_with('|') && !is_separator_row(t) {
            return split_table_cells(t);
        }
    }
    Vec::new()
}

fn parse_markdown_table_rows(md: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut seen_header = false;
    for line in md.lines() {
        let t = line.trim();
        if !t.starts_with('|') || !t.ends_with('|') {
            continue;
        }
        if is_separator_row(t) {
            seen_header = true;
            continue;
        }
        if !seen_header {
            seen_header = true;
            continue;
        }
        rows.push(split_table_cells(t));
    }
    rows
}

fn is_separator_row(line: &str) -> bool {
    line.chars()
        .all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
}

fn split_table_cells(line: &str) -> Vec<String> {
    line.trim_matches('|')
        .split('|')
        .map(|c| c.trim().to_owned())
        .collect()
}

fn render_row_with_headers(caption: &str, headers: &[String], cells: &[String]) -> String {
    let mut parts = Vec::with_capacity(cells.len() + 1);
    if !caption.is_empty() {
        parts.push(caption.to_owned());
    }
    for (idx, cell) in cells.iter().enumerate() {
        if let Some(h) = headers.get(idx)
            && !h.is_empty()
        {
            // Inject header billing-period / currency cues directly next to the cell value!
            let h_low = h.to_ascii_lowercase();
            let suffix = if (h_low.contains("annual") || h_low.contains("year") || h.contains("年"))
                && !cell.to_ascii_lowercase().contains("yr")
                && !cell.to_ascii_lowercase().contains("year")
                && !cell.contains("年")
            {
                "/yr"
            } else if (h_low.contains("month") || h.contains("月"))
                && !cell.to_ascii_lowercase().contains("mo")
                && !cell.to_ascii_lowercase().contains("month")
                && !cell.contains("月")
            {
                "/mo"
            } else {
                ""
            };
            parts.push(format!("{h}: {cell}{suffix}"));
        } else {
            parts.push(cell.clone());
        }
    }
    parts.join(" | ")
}

pub(crate) fn extract_distinctive_entity_labels(claim: &AtomicClaim) -> Vec<String> {
    let mut out = Vec::new();
    for a in &claim.entity_anchors {
        if let Some(id) = a.strip_prefix("id:") {
            out.push(id.to_owned());
        } else if let Some(ent) = a.strip_prefix("ent:") {
            out.push(ent.to_owned());
        } else if let Some(sym) = a.strip_prefix("sym:") {
            out.push(sym.to_owned());
        }
    }
    out
}

fn claim_asserts_present_tense(text: &str) -> bool {
    let low = text.to_ascii_lowercase();
    for pat in [
        "currently",
        "right now",
        "today",
        "still leads",
        "still leading",
        "remains #1",
        "目前",
        "当前",
        "仍然领先",
        "至今",
    ] {
        if low.contains(pat) || text.contains(pat) {
            return true;
        }
    }
    false
}

fn evidence_is_benchmark_scoped(low: &str, raw: &str) -> bool {
    for pat in [
        "in benchmark",
        "in our benchmark",
        "in tests",
        "on linux only",
        "under synthetic",
        "在测试中",
        "基准测试中",
        "仅限测试",
    ] {
        if low.contains(pat) || raw.contains(pat) {
            return true;
        }
    }
    false
}

fn evidence_is_correlational(low: &str, raw: &str) -> bool {
    for pat in [
        "correlat",
        "associated with",
        "co-occur",
        "alongside",
        "at the same time",
        "observed together",
        "link between",
        "相关",
        "伴随",
        "同时出现",
    ] {
        if low.contains(pat) || raw.contains(pat) {
            return true;
        }
    }
    false
}

fn evidence_supports_causal_identification(low: &str, raw: &str) -> bool {
    for pat in [
        "randomized controlled",
        "rct ",
        "a/b test",
        "controlled experiment",
        "causal effect",
        "instrumental variable",
        "difference-in-differences",
        "ablation proved",
        "随机对照",
        "控制变量实验",
        "因果机制已证实",
    ] {
        if low.contains(pat) || raw.contains(pat) {
            return true;
        }
    }
    false
}

pub(crate) fn build_evidence_ref(
    u: &KnowledgeUnit,
    polarity: EvidencePolarity,
    explicitly_cited: bool,
    excerpt: String,
    match_reason: String,
) -> EvidenceRef {
    let root = if u.origin_root.trim().is_empty() {
        canonical_origin_root(&u.location.resource_key)
    } else {
        canonical_origin_root(&u.origin_root)
    };
    EvidenceRef {
        evidence_id: EvidenceRef::compute_id(&u.unit_id, &u.revision_id, polarity),
        epistemic_kind: EpistemicKind::Observation,
        polarity,
        unit_id: u.unit_id.clone(),
        revision_id: u.revision_id.clone(),
        content_hash: u.content_hash.clone(),
        doc_id: u.location.doc_id.clone(),
        resource_key: u.location.resource_key.clone(),
        source_url: u.location.source_url.clone(),
        origin_root: root,
        origin_independent: u.origin_independent,
        byte_start: u.location.byte_start,
        byte_end: u.location.byte_end,
        page_number: u.location.page_number,
        heading_path: u.location.heading_path.clone(),
        observed_at_ms: u.observed_at_ms,
        valid_from: u.temporal.as_ref().and_then(|t| t.valid_from.clone()),
        valid_until: u.temporal.as_ref().and_then(|t| t.valid_until.clone()),
        unit_status: u.status.clone(),
        excerpt,
        explicitly_cited,
        match_reason,
    }
}
