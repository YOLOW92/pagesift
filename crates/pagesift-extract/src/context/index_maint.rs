//! Dependency-governed incremental vs compact-rebuild postings maintenance for [`ContextIndex`].

use std::collections::{BTreeMap, BTreeSet};

use pagesift_graph::context::{
    AssetDependency, AssetInvalidation, AssetKind, AssetSpecStamp, KnowledgeUnit, MaintenanceMode,
    choose_maintenance_mode,
};

use crate::context::search::{ContextIndex, IndexUpdateSummary};
use crate::context::tokenize_unified::{LexicalFeatures, analyze_text};

impl ContextIndex {
    pub fn new(spec_stamp: AssetSpecStamp) -> Self {
        Self {
            spec_stamp,
            ..Self::default()
        }
    }

    /// Apply unit updates and retirements, reusing compatible derived assets when valid.
    ///
    /// Coalesces duplicate `unit_id` entries within the same batch (last-write-wins) and routes
    /// non-searchable terminal states (`Withdrawn` / `Superseded` / `Expired`) to the removal path
    /// so dead units never pollute `units`, `inverted_terms`, `df`, or `total_terms_sum`!
    pub fn apply_updates(
        &mut self,
        upserts: &[KnowledgeUnit],
        retired_unit_ids: &[String],
        target_spec: &AssetSpecStamp,
    ) -> IndexUpdateSummary {
        let mut invalidated = Vec::new();
        let mut reused = 0usize;
        let mut recomputed = 0usize;

        // 1. Coalesce duplicate `unit_id`s in `upserts` (last occurrence in slice wins)
        let mut coalesced_map: BTreeMap<&str, &KnowledgeUnit> = BTreeMap::new();
        for u in upserts {
            coalesced_map.insert(u.unit_id.as_str(), u);
        }

        // 2. Partition into mutually exclusive `retired_set` vs searchable `active_upserts`
        let mut retired_set: BTreeSet<&str> = BTreeSet::new();
        for uid in retired_unit_ids {
            retired_set.insert(uid.as_str());
        }
        let mut active_upserts: Vec<&KnowledgeUnit> = Vec::with_capacity(coalesced_map.len());
        for (uid, u) in coalesced_map {
            if u.status.is_searchable_default() {
                retired_set.remove(uid);
                active_upserts.push(u);
            } else {
                retired_set.insert(uid);
            }
        }

        let mut touched_ids: BTreeSet<&str> = retired_set.clone();
        for u in &active_upserts {
            touched_ids.insert(u.unit_id.as_str());
        }
        let untouched_existing_units = self
            .units
            .keys()
            .filter(|uid| !touched_ids.contains(uid.as_str()))
            .count();

        let existing_retired_count = retired_set
            .iter()
            .filter(|&&uid| self.units.contains_key(uid))
            .count();
        let spec_changed = &self.spec_stamp != target_spec;
        let churn = active_upserts.len() + existing_retired_count;
        let mode = if spec_changed && !self.units.is_empty() {
            MaintenanceMode::FullRebuild {
                reason: "asset_spec_stamp_updated".to_owned(),
            }
        } else {
            choose_maintenance_mode(self.units.len(), churn, self.accumulated_tombstones)
        };

        for &uid in &retired_set {
            if self.units.contains_key(uid) {
                for kind in [
                    AssetKind::LexicalIndex,
                    AssetKind::SemanticSketch,
                    AssetKind::StructureIndex,
                    AssetKind::GraphIndex,
                ] {
                    invalidated.push(AssetInvalidation {
                        asset_kind: kind,
                        unit_id: uid.to_owned(),
                        reason: "unit_retired_or_withdrawn".to_owned(),
                    });
                }
            }
        }

        let mut cached_lex_to_reuse: BTreeMap<String, LexicalFeatures> = BTreeMap::new();
        for &u in &active_upserts {
            let prev_deps = self.deps.get(&u.unit_id);
            for kind in [
                AssetKind::LexicalIndex,
                AssetKind::SemanticSketch,
                AssetKind::StructureIndex,
                AssetKind::GraphIndex,
            ] {
                let valid_res = prev_deps
                    .and_then(|list| list.iter().find(|d| d.asset_kind == kind))
                    .map(|d| d.validate(u, target_spec));
                match valid_res {
                    Some(Ok(())) => {
                        reused += 1;
                        if matches!(kind, AssetKind::LexicalIndex)
                            && let Some(existing_lex) = self.unit_lex.get(&u.unit_id)
                        {
                            cached_lex_to_reuse.insert(u.unit_id.clone(), existing_lex.clone());
                        }
                    }
                    Some(Err(reason)) => {
                        recomputed += 1;
                        invalidated.push(AssetInvalidation {
                            asset_kind: kind,
                            unit_id: u.unit_id.clone(),
                            reason,
                        });
                    }
                    None => {
                        recomputed += 1;
                    }
                }
            }
        }

        self.spec_stamp = target_spec.clone();

        match &mode {
            MaintenanceMode::FullRebuild { .. } => {
                for &uid in &retired_set {
                    self.units.remove(uid);
                    self.unit_lex.remove(uid);
                    self.deps.remove(uid);
                }
                for &u in &active_upserts {
                    self.units.insert(u.unit_id.clone(), u.clone());
                }
                self.rebuild_postings(&cached_lex_to_reuse);
            }
            MaintenanceMode::Incremental => {
                // [推导] 未触碰的存量活跃单元全部复用其 4 类派生索引资产（词法/草图/结构/图锚点）
                reused += untouched_existing_units * 4;
                for &uid in &retired_set {
                    if self.remove_unit_incremental(uid) {
                        self.accumulated_tombstones += 1;
                    }
                }
                for &u in &active_upserts {
                    let reused_lex = cached_lex_to_reuse.remove(&u.unit_id);
                    self.upsert_unit_incremental(u.clone(), reused_lex);
                }
            }
        }

        IndexUpdateSummary {
            mode,
            invalidated_assets: invalidated,
            reused_asset_count: reused,
            recomputed_asset_count: recomputed,
        }
    }

    fn rebuild_postings(&mut self, reusable_lex: &BTreeMap<String, LexicalFeatures>) {
        self.inverted_terms.clear();
        self.inverted_symbols.clear();
        self.subject_index.clear();
        self.heading_terms.clear();
        self.total_terms_sum = 0;
        self.accumulated_tombstones = 0;

        let mut next_lex = BTreeMap::new();
        let mut next_deps = BTreeMap::new();

        for (uid, u) in &self.units {
            let lex = reusable_lex
                .get(uid)
                .cloned()
                .or_else(|| {
                    self.deps
                        .get(uid)
                        .and_then(|ds| ds.iter().find(|d| d.asset_kind == AssetKind::LexicalIndex))
                        .and_then(|d| d.validate(u, &self.spec_stamp).ok())
                        .and_then(|()| self.unit_lex.get(uid).cloned())
                })
                .unwrap_or_else(|| analyze_text(&u.text));

            Self::index_unit_into_maps(
                u,
                &lex,
                &mut self.inverted_terms,
                &mut self.inverted_symbols,
                &mut self.subject_index,
                &mut self.heading_terms,
                &mut self.total_terms_sum,
            );
            next_deps.insert(uid.clone(), build_all_deps(u, &self.spec_stamp));
            next_lex.insert(uid.clone(), lex);
        }
        self.unit_lex = next_lex;
        self.deps = next_deps;
    }

    fn remove_unit_incremental(&mut self, uid: &str) -> bool {
        let Some(old_u) = self.units.remove(uid) else {
            return false;
        };
        if let Some(old_lex) = self.unit_lex.remove(uid) {
            self.total_terms_sum = self
                .total_terms_sum
                .saturating_sub(u64::from(old_lex.total_terms));
            for term in old_lex.term_freqs.keys() {
                if let Some(postings) = self.inverted_terms.get_mut(term) {
                    postings.remove(uid);
                    if postings.is_empty() {
                        self.inverted_terms.remove(term);
                    }
                }
            }
            for sym in &old_lex.exact_symbols {
                if let Some(set) = self.inverted_symbols.get_mut(sym) {
                    set.remove(uid);
                    if set.is_empty() {
                        self.inverted_symbols.remove(sym);
                    }
                }
            }
        }
        if let Some(sk) = &old_u.subject_key
            && let Some(set) = self.subject_index.get_mut(&sk.key)
        {
            set.remove(uid);
            if set.is_empty() {
                self.subject_index.remove(&sk.key);
            }
        }
        let h_lex = analyze_heading_and_headers(&old_u);
        for ht in h_lex.term_freqs.keys() {
            if let Some(set) = self.heading_terms.get_mut(ht) {
                set.remove(uid);
                if set.is_empty() {
                    self.heading_terms.remove(ht);
                }
            }
        }
        self.deps.remove(uid);
        true
    }

    fn upsert_unit_incremental(&mut self, u: KnowledgeUnit, reused_lex: Option<LexicalFeatures>) {
        let uid = u.unit_id.clone();
        if self.units.contains_key(&uid) {
            if let Some(lex) = reused_lex {
                if let Some(old_u) = self.units.get(&uid) {
                    if let Some(sk) = &old_u.subject_key
                        && let Some(set) = self.subject_index.get_mut(&sk.key)
                    {
                        set.remove(&uid);
                        if set.is_empty() {
                            self.subject_index.remove(&sk.key);
                        }
                    }
                    let old_h = analyze_heading_and_headers(old_u);
                    for ht in old_h.term_freqs.keys() {
                        if let Some(set) = self.heading_terms.get_mut(ht) {
                            set.remove(&uid);
                            if set.is_empty() {
                                self.heading_terms.remove(ht);
                            }
                        }
                    }
                }
                if let Some(sk) = &u.subject_key {
                    self.subject_index
                        .entry(sk.key.clone())
                        .or_default()
                        .insert(uid.clone());
                }
                let new_h = analyze_heading_and_headers(&u);
                for ht in new_h.term_freqs.keys() {
                    self.heading_terms
                        .entry(ht.clone())
                        .or_default()
                        .insert(uid.clone());
                }
                self.deps
                    .insert(uid.clone(), build_all_deps(&u, &self.spec_stamp));
                self.unit_lex.insert(uid.clone(), lex);
                self.units.insert(uid, u);
                return;
            }
            self.remove_unit_incremental(&uid);
        }

        let lex = analyze_text(&u.text);
        Self::index_unit_into_maps(
            &u,
            &lex,
            &mut self.inverted_terms,
            &mut self.inverted_symbols,
            &mut self.subject_index,
            &mut self.heading_terms,
            &mut self.total_terms_sum,
        );
        self.deps
            .insert(uid.clone(), build_all_deps(&u, &self.spec_stamp));
        self.unit_lex.insert(uid.clone(), lex);
        self.units.insert(uid, u);
    }

    fn index_unit_into_maps(
        u: &KnowledgeUnit,
        lex: &LexicalFeatures,
        inv_terms: &mut BTreeMap<String, BTreeMap<String, u32>>,
        inv_syms: &mut BTreeMap<String, BTreeSet<String>>,
        subj_idx: &mut BTreeMap<String, BTreeSet<String>>,
        head_idx: &mut BTreeMap<String, BTreeSet<String>>,
        total_sum: &mut u64,
    ) {
        *total_sum = total_sum.saturating_add(u64::from(lex.total_terms));
        for (term, &tf) in &lex.term_freqs {
            inv_terms
                .entry(term.clone())
                .or_default()
                .insert(u.unit_id.clone(), tf);
        }
        for sym in &lex.exact_symbols {
            inv_syms
                .entry(sym.clone())
                .or_default()
                .insert(u.unit_id.clone());
        }
        if let Some(sk) = &u.subject_key {
            subj_idx
                .entry(sk.key.clone())
                .or_default()
                .insert(u.unit_id.clone());
        }
        let h_lex = analyze_heading_and_headers(u);
        for ht in h_lex.term_freqs.keys() {
            head_idx
                .entry(ht.clone())
                .or_default()
                .insert(u.unit_id.clone());
        }
    }
}

fn build_all_deps(u: &KnowledgeUnit, spec: &AssetSpecStamp) -> Vec<AssetDependency> {
    [
        AssetKind::LexicalIndex,
        AssetKind::SemanticSketch,
        AssetKind::StructureIndex,
        AssetKind::GraphIndex,
    ]
    .into_iter()
    .map(|k| AssetDependency::from_unit(k, u, spec.clone()))
    .collect()
}

fn analyze_heading_and_headers(u: &KnowledgeUnit) -> LexicalFeatures {
    let mut combined = u.location.heading_path.join(" ");
    if let Some(tm) = &u.location.table_meta {
        if let Some(cap) = &tm.caption {
            combined.push(' ');
            combined.push_str(cap);
        }
        for h in &tm.headers {
            combined.push(' ');
            combined.push_str(h);
        }
    }
    analyze_text(&combined)
}
