//! Evidence-, temporal-, and structure-aware multi-document [`ContextIndex`] and search engine.

use std::collections::{BTreeMap, BTreeSet};

use pagesift_graph::context::{
    AssetDependency, AssetInvalidation, AssetSpecStamp, ContextSearchHit, ContextSearchReport,
    ContextSearchRequest, IndexVisibility, KnowledgeUnit, MaintenanceMode, SearchDimensionScores,
    ValidityStatus,
};
use pagesift_graph::{ConflictRecord, EvidenceSpan, FactCluster};
use serde::{Deserialize, Serialize};

use crate::context::calib::ContextLearnState;
use crate::context::temporal_eval::{
    canonical_origin_root, compute_unit_support, evaluate_validity, extract_query_years,
    freshness_score_for_validity, is_genuine_independent_conflict,
};
use crate::context::tokenize_unified::{LexicalFeatures, analyze_query, sketch_similarity};

// [推导] Okapi BM25 标准词频饱和参数 k1 = 1.5（Robertson & Zaragoza 2009）
const BM25_K1: f32 = 1.5;
// [推导] Okapi BM25 标准长度归一化参数 b = 0.75（Robertson & Zaragoza 2009）
const BM25_B: f32 = 0.75;

const STRAT_LEXICAL: u8 = 1 << 0;
const STRAT_SYMBOL: u8 = 1 << 1;
const STRAT_STRUCTURE: u8 = 1 << 2;
const STRAT_SKETCH: u8 = 1 << 3; // [协议] 第 3 位草图召回策略位掩码
const STRAT_EVIDENCE: u8 = 1 << 4; // [协议] 第 4 位证据接地策略位掩码

/// Serializable incremental multi-document Context index with dependency-governed derived assets.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextIndex {
    pub spec_stamp: AssetSpecStamp,
    pub units: BTreeMap<String, KnowledgeUnit>,
    pub unit_lex: BTreeMap<String, LexicalFeatures>,
    pub deps: BTreeMap<String, Vec<AssetDependency>>,
    pub inverted_terms: BTreeMap<String, BTreeMap<String, u32>>,
    pub inverted_symbols: BTreeMap<String, BTreeSet<String>>,
    pub subject_index: BTreeMap<String, BTreeSet<String>>,
    pub heading_terms: BTreeMap<String, BTreeSet<String>>,
    pub total_terms_sum: u64,
    pub accumulated_tombstones: usize,
}

/// Summary of incremental vs rebuild index maintenance for a [`ContextDelta`].
#[derive(Debug, Clone)]
pub struct IndexUpdateSummary {
    pub mode: MaintenanceMode,
    pub invalidated_assets: Vec<AssetInvalidation>,
    pub reused_asset_count: usize,
    pub recomputed_asset_count: usize,
}

impl ContextIndex {
    /// Execute adaptive multi-dimensional retrieval across lexical, symbol, structure, entity,
    /// temporal validity (`as_of_ms` & `effective_at_ms`), source independence, and conflicts.
    pub fn search(
        &self,
        req: &ContextSearchRequest,
        conflicts: &[ConflictRecord],
        fact_clusters: &[FactCluster],
        visibilities: Vec<IndexVisibility>,
        learn: &mut ContextLearnState,
    ) -> ContextSearchReport {
        let q_analysis = analyze_query(&req.query);
        let q_feat = &q_analysis.features;
        let q_concepts = &q_analysis.lexical_concepts;
        let q_years = extract_query_years(&q_feat.entity_anchors);

        // Deduplicate query symbols that are purely camelCase spellings of an existing lexical concept
        // so `overall_cov` denominator never double-counts a single token like `BurstRps`!
        let distinct_symbols: Vec<&String> = q_feat
            .exact_symbols
            .iter()
            .filter(|sym| {
                !q_concepts
                    .iter()
                    .any(|(stem, alt)| stem == sym.as_str() || alt.as_deref() == Some(sym.as_str()))
            })
            .collect();
        let total_query_facets = q_concepts.len() + distinct_symbols.len();

        let mut strategies_used = Vec::with_capacity(6); // [策略] 检索维度策略标签数上限 6
        let mut warnings = Vec::new();

        for vis in &visibilities {
            if !vis.instance_matched {
                warnings.push(format!(
                    "index_instance_mismatch:{}:rebuilding_required",
                    vis.source_id
                ));
            } else if !vis.is_caught_up {
                warnings.push(format!(
                    "index_lagging:{}:lag_ticks={}",
                    vis.source_id, vis.lag_ticks
                ));
            }
        }

        let n_docs = self.units.len().max(1) as f32;
        let avg_dl = (self.total_terms_sum as f32 / n_docs).max(1.0);

        if !q_feat.exact_symbols.is_empty() {
            strategies_used.push("exact_symbol".to_owned());
        }
        if !q_concepts.is_empty() {
            strategies_used.push("lexical_bm25".to_owned());
            strategies_used.push("structure_path_and_headers".to_owned());
        }
        if req.subject_key.is_some() || !q_feat.entity_anchors.is_empty() {
            strategies_used.push("entity_subject".to_owned());
        }
        if req.as_of_ms.is_some() || req.effective_at_ms.is_some() || !q_years.is_empty() {
            strategies_used.push("temporal_as_of_and_validity".to_owned());
        }
        if req.enable_semantic_sketch {
            strategies_used.push("semantic_sketch".to_owned());
        }
        strategies_used.push("evidence_and_lineage".to_owned());

        struct ConceptPostings<'a> {
            p_main: Option<&'a BTreeMap<String, u32>>,
            p_alt: Option<&'a BTreeMap<String, u32>>,
            h_main: Option<&'a BTreeSet<String>>,
            h_alt: Option<&'a BTreeSet<String>>,
        }

        // Pre-bind inverted postings for query concepts once (eliminating inner-loop BTreeMap lookups per candidate!)
        let concept_postings: Vec<ConceptPostings<'_>> = q_concepts
            .iter()
            .map(|(stem, alt)| ConceptPostings {
                p_main: self.inverted_terms.get(stem.as_str()),
                p_alt: alt.as_deref().and_then(|a| self.inverted_terms.get(a)),
                h_main: self.heading_terms.get(stem.as_str()),
                h_alt: alt.as_deref().and_then(|a| self.heading_terms.get(a)),
            })
            .collect();

        // Zero-clone borrowed candidate gathering (`BTreeSet<&str>`)
        let mut candidate_ids: BTreeSet<&str> = BTreeSet::new();
        for sym in &q_feat.exact_symbols {
            if let Some(ids) = self.inverted_symbols.get(sym) {
                candidate_ids.extend(ids.iter().map(String::as_str));
            }
        }
        for cp in &concept_postings {
            if let Some(p) = cp.p_main {
                candidate_ids.extend(p.keys().map(String::as_str));
            }
            if let Some(p) = cp.p_alt {
                candidate_ids.extend(p.keys().map(String::as_str));
            }
            if let Some(h) = cp.h_main {
                candidate_ids.extend(h.iter().map(String::as_str));
            }
            if let Some(h) = cp.h_alt {
                candidate_ids.extend(h.iter().map(String::as_str));
            }
        }
        if let Some(subj) = &req.subject_key
            && let Some(s_ids) = self.subject_index.get(subj)
        {
            candidate_ids.extend(s_ids.iter().map(String::as_str));
        }
        let limit = req.limit.max(1);
        // [策略] 候选不足至少 4 条时放开特征草图全扫兜底召回近似单元
        if req.enable_semantic_sketch && candidate_ids.len() < limit.max(4) {
            candidate_ids.extend(self.units.keys().map(String::as_str));
        }
        if q_concepts.is_empty() && q_feat.exact_symbols.is_empty() && candidate_ids.is_empty() {
            candidate_ids.extend(self.units.keys().map(String::as_str));
        }

        struct RawScored<'a> {
            uid: &'a str,
            unit: &'a KnowledgeUnit,
            bm25: f32,
            sk_sim: f32,
            term_cov: f32,
            support: f32,
            freshness_validity: f32,
            source_indep: f32,
            validity: ValidityStatus,
            strat_mask: u8,
            facet_hits: Vec<usize>,
        }

        let mut raw_scored: Vec<RawScored<'_>> = Vec::with_capacity(candidate_ids.len());
        let mut max_lex = 0.0f32;

        for uid in candidate_ids {
            let Some(u) = self.units.get(uid) else {
                continue;
            };
            if !req.include_unverified && !u.status.is_verified_active() {
                continue;
            }
            if !u.status.is_searchable_default() {
                continue;
            }
            if let Some(cutoff) = req.as_of_ms
                && u.observed_at_ms > cutoff
            {
                continue;
            }
            if let Some(target_subj) = &req.subject_key
                && u.subject_key
                    .as_ref()
                    .is_none_or(|sk| &sk.key != target_subj)
            {
                continue;
            }

            let validity = evaluate_validity(u, req.effective_at_ms, req.as_of_ms, &q_years);
            if req.effective_at_ms.is_some()
                && matches!(
                    validity,
                    ValidityStatus::ExpiredAtTarget | ValidityStatus::NotYetValidAtTarget
                )
            {
                continue;
            }

            let Some(u_lex) = self.unit_lex.get(uid) else {
                continue;
            };
            let mut strat_mask = 0u8;
            let mut facet_hits: Vec<usize> = Vec::new();

            let dl = u_lex.total_terms.max(1) as f32;
            let mut bm25 = 0.0f32;
            let mut struct_hits = 0usize;

            for (ci, cp) in concept_postings.iter().enumerate() {
                let mut concept_matched = false;
                let chosen_post = cp
                    .p_main
                    .and_then(|p| p.get(uid).map(|&tf| (p.len(), tf)))
                    .or_else(|| cp.p_alt.and_then(|p| p.get(uid).map(|&tf| (p.len(), tf))));
                if let Some((df_usize, tf)) = chosen_post {
                    let df = df_usize as f32;
                    let idf = ((n_docs - df + 0.5) / (df + 0.5) + 1.0).ln();
                    let tf_f = tf as f32;
                    let num = tf_f * (BM25_K1 + 1.0);
                    let den = tf_f + BM25_K1 * (1.0 - BM25_B + BM25_B * (dl / avg_dl));
                    bm25 += idf * (num / den);
                    concept_matched = true;
                }
                let in_heading = cp.h_main.is_some_and(|s| s.contains(uid))
                    || cp.h_alt.is_some_and(|s| s.contains(uid));
                if in_heading {
                    struct_hits += 1;
                    concept_matched = true;
                }
                if concept_matched {
                    facet_hits.push(ci);
                }
            }
            if bm25 > 0.0 {
                strat_mask |= STRAT_LEXICAL;
            }

            let mut sym_hits = 0usize;
            for sym in &q_feat.exact_symbols {
                if u_lex.exact_symbols.contains(sym) {
                    sym_hits += 1;
                }
            }
            for (si, sym) in distinct_symbols.iter().enumerate() {
                if u_lex.exact_symbols.contains(*sym) {
                    facet_hits.push(q_concepts.len() + si);
                }
            }
            if sym_hits > 0 {
                // [策略] 每个精确代码/参数符号命中叠加 3.0 BM25 等效分，保证精确 API 查询不被泛化高频词淹没
                bm25 += 3.0 * (sym_hits as f32);
                strat_mask |= STRAT_SYMBOL;
            }

            if struct_hits > 0 {
                // [策略] 标题路径或表头列名命中按每词 +1.25 加权
                bm25 += 1.25 * (struct_hits as f32);
                strat_mask |= STRAT_STRUCTURE;
            }

            let sk_sim = if req.enable_semantic_sketch {
                let s = sketch_similarity(&q_feat.sketch, &u_lex.sketch);
                // [策略] 语义草图内积 > 0.35 时计为有效近似命中
                if s > 0.35 {
                    strat_mask |= STRAT_SKETCH;
                }
                s
            } else {
                0.0
            };

            // [策略] 纯草图通道零词法/符号命中时要求有符号余弦 > 0.65 (t_15 > 3.31, p < 0.0025) 才单独保留，严控领域外 OOD 误召回
            if bm25 == 0.0
                && sym_hits == 0
                && struct_hits == 0
                && req.subject_key.is_none()
                && (!req.enable_semantic_sketch || sk_sim <= 0.65)
                && total_query_facets > 0
            {
                continue;
            }

            let term_cov = if total_query_facets == 0 {
                1.0
            } else {
                (facet_hits.len() as f32 / total_query_facets as f32).clamp(0.0, 1.0)
            };

            let support = compute_unit_support(u, &req.required_fields);
            if support > 0.5 {
                strat_mask |= STRAT_EVIDENCE;
            }
            let freshness_validity = freshness_score_for_validity(u, validity, req.effective_at_ms);
            // [策略] 转载/镜像派生源独立性分降为 0.35
            let source_indep = if u.origin_independent { 1.0 } else { 0.35 };

            max_lex = max_lex.max(bm25);
            raw_scored.push(RawScored {
                uid,
                unit: u,
                bm25,
                sk_sim,
                term_cov,
                support,
                freshness_validity,
                source_indep,
                validity,
                strat_mask,
                facet_hits,
            });
        }

        // Compute final rank_score and sort lightweight references BEFORE cloning `KnowledgeUnit`!
        let mut ranked: Vec<(f32, f32, RawScored<'_>)> = raw_scored
            .into_iter()
            .map(|r| {
                let norm_lex = if max_lex > 0.0 {
                    (r.bm25 / max_lex).clamp(0.0, 1.0)
                } else {
                    0.5
                };
                let relevance = if req.enable_semantic_sketch {
                    // [推导] 相关性分融合：0.80 词法/符号/结构分 + 0.20 特征草图分
                    (0.80 * norm_lex + 0.20 * r.sk_sim).clamp(0.0, 1.0)
                } else {
                    norm_lex
                };
                // [推导] 排序综合权重：0.45 相关性 + 0.20 证据接地支持度 + 0.20 时间有效性 + 0.15 信源独立性
                let raw_rank = 0.45 * relevance
                    + 0.20 * r.support
                    + 0.20 * r.freshness_validity
                    + 0.15 * r.source_indep;
                (raw_rank, relevance, r)
            })
            .collect();

        ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.2.uid.cmp(b.2.uid)));
        ranked.truncate(limit);

        // Materialize ONLY the top-`limit` hits (eliminating `(N - limit)` full `KnowledgeUnit` deep clones!)
        let mut hits: Vec<ContextSearchHit> = Vec::with_capacity(ranked.len() + 2);
        let mut any_learn_applied = false;
        let mut covered_facets: BTreeSet<usize> = BTreeSet::new();

        for (raw_rank, relevance, r) in ranked {
            let (calibrated_conf, used_learn) = learn.calibrate(raw_rank, req.learn_enabled);
            any_learn_applied |= used_learn;

            let mut matched_terms = Vec::with_capacity(r.facet_hits.len());
            for &fi in &r.facet_hits {
                covered_facets.insert(fi);
                if fi < q_concepts.len() {
                    matched_terms.push(q_concepts[fi].0.clone());
                } else {
                    matched_terms.push(distinct_symbols[fi - q_concepts.len()].clone());
                }
            }
            for sym in &q_feat.exact_symbols {
                if self
                    .unit_lex
                    .get(r.uid)
                    .is_some_and(|ul| ul.exact_symbols.contains(sym))
                    && !matched_terms.contains(sym)
                {
                    matched_terms.push(sym.clone());
                }
            }

            let mut matched_strats = Vec::with_capacity(5); // [策略] 单命中最多关联 5 类召回策略标签
            if r.strat_mask & STRAT_LEXICAL != 0 {
                matched_strats.push("lexical_bm25".to_owned());
            }
            if r.strat_mask & STRAT_SYMBOL != 0 {
                matched_strats.push("exact_symbol".to_owned());
            }
            if r.strat_mask & STRAT_STRUCTURE != 0 {
                matched_strats.push("structure_path_and_headers".to_owned());
            }
            if r.strat_mask & STRAT_SKETCH != 0 {
                matched_strats.push("semantic_sketch".to_owned());
            }
            if r.strat_mask & STRAT_EVIDENCE != 0 {
                matched_strats.push("evidence_grounded".to_owned());
            }

            hits.push(ContextSearchHit {
                unit: r.unit.clone(),
                rank_score: raw_rank,
                dimensions: SearchDimensionScores {
                    relevance,
                    support: r.support,
                    coverage: r.term_cov,
                    freshness_validity: r.freshness_validity,
                    calibrated_confidence: calibrated_conf,
                    source_independence: r.source_indep,
                },
                validity_status: r.validity,
                matched_terms,
                matched_strategies: matched_strats,
                conflict_counterpart: false,
            });
        }

        // Co-retrieve genuine independent conflict counterparts (strictly enforcing `as_of_ms`,
        // `effective_at_ms`, `include_unverified`, and source independence!)
        let mut matched_conflicts: Vec<ConflictRecord> = Vec::new();
        if req.include_conflicts && !conflicts.is_empty() && !hits.is_empty() {
            let hit_doc_ids: BTreeSet<&str> = hits
                .iter()
                .map(|h| h.unit.location.doc_id.as_str())
                .collect();
            let hit_subjects: BTreeSet<&str> = hits
                .iter()
                .filter_map(|h| h.unit.subject_key.as_ref().map(|s| s.key.as_str()))
                .collect();

            for c in conflicts {
                let touches_subject = hit_subjects.contains(c.subject_key.as_str());
                let touches_doc = c.variants.iter().any(|v| {
                    v.evidence
                        .iter()
                        .any(|ev| hit_doc_ids.contains(ev.doc_id.as_str()))
                });
                if (touches_subject || touches_doc)
                    && is_genuine_independent_conflict(c, |doc_id| {
                        self.units
                            .values()
                            .find(|u| {
                                u.location.doc_id == doc_id && u.status.is_searchable_default()
                            })
                            .map(|u| (u.origin_root.as_str(), u.origin_independent))
                    })
                {
                    matched_conflicts.push(c.clone());
                }
            }

            let mut existing_uids: BTreeSet<String> =
                hits.iter().map(|h| h.unit.unit_id.clone()).collect();
            for c in &matched_conflicts {
                for var in &c.variants {
                    if var.independent_source_count == 0 {
                        continue;
                    }
                    for ev in &var.evidence {
                        if let Some(counter_u) = self.find_unit_for_evidence(
                            ev,
                            req.include_unverified,
                            req.as_of_ms,
                            req.effective_at_ms,
                            &q_years,
                        ) && !existing_uids.contains(&counter_u.unit_id)
                        {
                            existing_uids.insert(counter_u.unit_id.clone());
                            let validity = evaluate_validity(
                                counter_u,
                                req.effective_at_ms,
                                req.as_of_ms,
                                &q_years,
                            );
                            let fresh_v = freshness_score_for_validity(
                                counter_u,
                                validity,
                                req.effective_at_ms,
                            );
                            let sup = compute_unit_support(counter_u, &req.required_fields);
                            let indep = if counter_u.origin_independent {
                                1.0
                            } else {
                                // [策略] 同上派生源权重 0.35
                                0.35
                            };
                            // [策略] 冲突对等证据基础排序与校准入参 0.65
                            let (cal, used_l) = learn.calibrate(0.65, req.learn_enabled);
                            any_learn_applied |= used_l;
                            hits.push(ContextSearchHit {
                                unit: counter_u.clone(),
                                // [策略] 冲突反方关联证据保底排序分 0.65
                                rank_score: 0.65,
                                dimensions: SearchDimensionScores {
                                    // [策略] 结构冲突关联召回相关性基准 0.60
                                    relevance: 0.60,
                                    support: sup,
                                    coverage: 0.50,
                                    freshness_validity: fresh_v,
                                    calibrated_confidence: cal,
                                    source_independence: indep,
                                },
                                validity_status: validity,
                                matched_terms: Vec::new(),
                                matched_strategies: vec!["conflict_counterpart_recall".to_owned()],
                                conflict_counterpart: true,
                            });
                        }
                    }
                }
            }
        }

        // Aggregate deduplicated INDEPENDENT origin roots (mirrors with `origin_independent == false`
        // NEVER inflate `independent_source_count`!)
        let mut indep_roots_map: BTreeMap<String, String> = BTreeMap::new();
        for h in &hits {
            if h.unit.origin_independent && !h.unit.origin_root.trim().is_empty() {
                let canon = canonical_origin_root(&h.unit.origin_root);
                indep_roots_map
                    .entry(canon)
                    .or_insert_with(|| h.unit.origin_root.clone());
            }
        }
        let origin_roots: Vec<String> = indep_roots_map.into_values().collect();
        let indep_count = origin_roots.len();
        let total_hits = hits.len();

        let overall_cov = if hits.is_empty() {
            0.0
        } else if total_query_facets == 0 {
            1.0
        } else {
            (covered_facets.len() as f32 / total_query_facets as f32).clamp(0.0, 1.0)
        };

        let avg_dim = if hits.is_empty() {
            SearchDimensionScores::default()
        } else {
            let inv = 1.0 / (total_hits as f32);
            SearchDimensionScores {
                relevance: hits.iter().map(|h| h.dimensions.relevance).sum::<f32>() * inv,
                support: hits.iter().map(|h| h.dimensions.support).sum::<f32>() * inv,
                coverage: overall_cov,
                freshness_validity: hits
                    .iter()
                    .map(|h| h.dimensions.freshness_validity)
                    .sum::<f32>()
                    * inv,
                calibrated_confidence: hits
                    .iter()
                    .map(|h| h.dimensions.calibrated_confidence)
                    .sum::<f32>()
                    * inv,
                source_independence: (indep_count as f32 / total_hits.max(1) as f32)
                    .clamp(0.0, 1.0),
            }
        };

        let relevant_clusters: Vec<FactCluster> = fact_clusters
            .iter()
            .filter(|fc| {
                hits.iter().any(|h| {
                    h.unit
                        .subject_key
                        .as_ref()
                        .is_some_and(|sk| sk.key == fc.subject_key)
                })
            })
            .cloned()
            .collect();

        ContextSearchReport {
            query: req.query.clone(),
            hits,
            dimensions: avg_dim,
            independent_source_count: indep_count,
            origin_roots,
            conflicts: matched_conflicts,
            fact_clusters: relevant_clusters,
            strategies_used,
            visibilities,
            learn_applied: any_learn_applied,
            warnings,
        }
    }

    fn find_unit_for_evidence(
        &self,
        ev: &EvidenceSpan,
        include_unverified: bool,
        as_of_ms: Option<u64>,
        effective_at_ms: Option<u64>,
        q_years: &BTreeSet<u64>,
    ) -> Option<&KnowledgeUnit> {
        let mut content_fallback: Option<&KnowledgeUnit> = None;
        let mut any_fallback: Option<&KnowledgeUnit> = None;

        for u in self.units.values() {
            if !include_unverified && !u.status.is_verified_active() {
                continue;
            }
            if !u.status.is_searchable_default() {
                continue;
            }
            if let Some(cutoff) = as_of_ms
                && u.observed_at_ms > cutoff
            {
                continue;
            }
            let val = evaluate_validity(u, effective_at_ms, as_of_ms, q_years);
            if effective_at_ms.is_some()
                && matches!(
                    val,
                    ValidityStatus::ExpiredAtTarget | ValidityStatus::NotYetValidAtTarget
                )
            {
                continue;
            }
            if u.location.doc_id == ev.doc_id {
                if let Some(bid) = ev.block_id.as_deref()
                    && u.location.block_ids.iter().any(|b| b == bid)
                {
                    return Some(u);
                }
                if let (Some(s), Some(e)) = (ev.start, ev.end)
                    && s < u.location.byte_end
                    && e > u.location.byte_start
                {
                    return Some(u);
                }
                if (!ev.excerpt.is_empty() && u.text.contains(&ev.excerpt))
                    || (content_fallback.is_none()
                        && u.kind != pagesift_graph::context::UnitKind::SectionHeading)
                {
                    content_fallback = Some(u);
                }
                if any_fallback.is_none() {
                    any_fallback = Some(u);
                }
            }
        }
        content_fallback.or(any_fallback)
    }
}
