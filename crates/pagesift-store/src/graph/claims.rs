//! Persistent conflict records and claim lifecycle (`ClaimLifecycleRecord`) storage and queries.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use pagesift_graph::{
    CitationEdge, ClaimLifecycleRecord, ClaimRecord, ClaimTransition, ConflictRecord,
    DEFAULT_CONTRACT_ID, DocNode, FactCluster, GraphLimits, ObservationOutcome, ProvenanceEdge,
    SourceLineage, build_fact_clusters_with_lineage, claims_as_of_checked, compute_source_lineage,
    detect_conflicts_with_lineage, reconcile_resource_claims_for_contract,
};

use super::bundle::{AsOfClaimsResult, StoredProvenance};
use super::{
    DEFAULT_GRAPH_CONFLICTS_LIMIT, GraphStore, PREFIX_CLAIM_LC, PREFIX_CONFLICT, PREFIX_PROV,
    decode_json, encode_json, initial_key,
};
use crate::engine::{AtomicTx, ns};
use crate::{Result, to_ms};

pub const DEFAULT_GRAPH_CLAIMS_LIMIT: usize = 4096; // [策略] 单次声明生命周期查询默认返回上限

impl<'a> GraphStore<'a> {
    /// Persist conflict records idempotently into `ns::GRAPH_FACT`.
    pub fn put_conflicts(&self, conflicts: &[ConflictRecord]) -> Result<()> {
        if conflicts.is_empty() {
            return Ok(());
        }
        let now = to_ms(SystemTime::now());
        let mut tx = self.store.atomic_tx();
        for conflict in conflicts {
            let key = self.scoped_compound(
                PREFIX_CONFLICT,
                &conflict.subject_key,
                &conflict.conflict_id,
            );
            let val = encode_json(conflict, "conflict record")?;
            tx.put(ns::GRAPH_FACT, &key, &val, now)?;
        }
        tx.commit()
    }

    /// 在原子事务内更新给定 `subjects` 集合的当前活跃冲突（已消解冲突同步清理）。
    pub(crate) fn stage_conflicts_for_subjects_in_tx(
        &self,
        tx: &mut AtomicTx<'_>,
        subjects: &BTreeSet<String>,
        conflicts: &[ConflictRecord],
        now_ms: u64,
    ) -> Result<()> {
        for subj in subjects {
            let mut prefix = self.scoped_prefixed(PREFIX_CONFLICT, subj);
            prefix.push(0);
            for key in tx.keys_matching(ns::GRAPH_FACT, &prefix)? {
                tx.delete(ns::GRAPH_FACT, &key)?;
            }
        }
        for conflict in conflicts {
            let key = self.scoped_compound(
                PREFIX_CONFLICT,
                &conflict.subject_key,
                &conflict.conflict_id,
            );
            let val = encode_json(conflict, "conflict record")?;
            tx.put(ns::GRAPH_FACT, &key, &val, now_ms)?;
        }
        Ok(())
    }

    /// 在原子事务内基于当前 `tx` 视图重算给定主体的跨源冲突，并写回持久化冲突索引（C25 / C26 / R06）。
    pub(crate) fn recompute_conflicts_for_subjects_in_tx(
        &self,
        tx: &mut AtomicTx<'_>,
        subjects: &BTreeSet<String>,
        now_ms: u64,
    ) -> Result<Vec<ConflictRecord>> {
        if subjects.is_empty() {
            return Ok(Vec::new());
        }
        let claim_prefix = self.scoped_prefixed(PREFIX_CLAIM_LC, "");
        let claim_keys = tx.keys_matching(ns::GRAPH_FACT, &claim_prefix)?;
        let mut active_claims: Vec<ClaimRecord> = Vec::new();
        for key in claim_keys {
            let Some((_, raw)) = tx.get(ns::GRAPH_FACT, &key)? else {
                continue;
            };
            let recs: Vec<ClaimLifecycleRecord> = decode_json(&raw, "claim lifecycle records")?;
            for rec in recs {
                if rec.status.is_active_or_unverified() && subjects.contains(&rec.claim.subject_key)
                {
                    active_claims.push(rec.claim);
                }
            }
        }
        if active_claims.is_empty() {
            self.stage_conflicts_for_subjects_in_tx(tx, subjects, &[], now_ms)?;
            return Ok(Vec::new());
        }

        let prov_prefix = self.scoped_prefixed(PREFIX_PROV, "");
        let prov_keys = tx.keys_matching(ns::GRAPH_FACT, &prov_prefix)?;
        let mut nodes: Vec<DocNode> = Vec::new();
        let mut edges: Vec<ProvenanceEdge> = Vec::new();
        let mut citations: Vec<CitationEdge> = Vec::new();
        for key in prov_keys {
            let Some((_, raw)) = tx.get(ns::GRAPH_FACT, &key)? else {
                continue;
            };
            let prov: StoredProvenance = decode_json(&raw, "stored provenance")?;
            if let Some(node) = prov.node {
                nodes.push(node);
            }
            edges.extend(prov.edges);
            citations.extend(prov.citations);
        }
        let lineage = compute_source_lineage(&nodes, &edges);
        let conflicts = detect_conflicts_with_lineage(&active_claims, &lineage, &citations);
        self.stage_conflicts_for_subjects_in_tx(tx, subjects, &conflicts, now_ms)?;
        Ok(conflicts)
    }

    fn load_lineage_and_citations(
        &self,
        as_of_ms: Option<u64>,
    ) -> Result<(BTreeMap<String, SourceLineage>, Vec<CitationEdge>)> {
        let prov_prefix = self.scoped_prefixed(PREFIX_PROV, "");
        let prov_keys = self
            .store
            .keys(ns::GRAPH_FACT, |k| k.starts_with(&prov_prefix))?;
        let mut nodes: Vec<DocNode> = Vec::new();
        let mut edges: Vec<ProvenanceEdge> = Vec::new();
        let mut citations: Vec<CitationEdge> = Vec::new();
        for key in prov_keys {
            let Some((_, raw)) = self.store.get(ns::GRAPH_FACT, &key)? else {
                continue;
            };
            let prov: StoredProvenance = decode_json(&raw, "stored provenance")?;
            if as_of_ms.is_some_and(|cutoff| prov.observed_at_ms > cutoff) {
                continue;
            }
            if let Some(node) = prov.node {
                nodes.push(node);
            }
            edges.extend(prov.edges);
            citations.extend(prov.citations);
        }
        Ok((compute_source_lineage(&nodes, &edges), citations))
    }

    /// 结合持久化溯源图（`PREFIX_PROV` 含二次引用 `citations`）按独立信源计算活跃声明的冲突列表（C26 / R06 / G5）。
    pub fn detect_conflicts_for_claims(
        &self,
        claims: &[ClaimRecord],
        as_of_ms: Option<u64>,
    ) -> Result<Vec<ConflictRecord>> {
        if claims.is_empty() {
            return Ok(Vec::new());
        }
        let (lineage, citations) = self.load_lineage_and_citations(as_of_ms)?;
        Ok(detect_conflicts_with_lineage(claims, &lineage, &citations))
    }

    /// 单次加载溯源与二次引用链，同时计算持久化声明冲突与跨源事实簇（`FactCluster`）。
    pub fn build_conflicts_and_clusters_for_claims(
        &self,
        claims: &[ClaimRecord],
        as_of_ms: Option<u64>,
    ) -> Result<(Vec<ConflictRecord>, Vec<FactCluster>)> {
        if claims.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let (lineage, citations) = self.load_lineage_and_citations(as_of_ms)?;
        let conflicts = detect_conflicts_with_lineage(claims, &lineage, &citations);
        let clusters = build_fact_clusters_with_lineage(
            claims,
            GraphLimits::default().max_claims_per_bucket,
            &lineage,
            &citations,
        );
        Ok((conflicts, clusters))
    }

    /// List up to [`DEFAULT_GRAPH_CONFLICTS_LIMIT`] stored conflicts, optionally filtered by `subject_key`.
    pub fn list_conflicts(&self, subject_key: Option<&str>) -> Result<Vec<ConflictRecord>> {
        let (items, _, _) =
            self.list_conflicts_bounded(subject_key, DEFAULT_GRAPH_CONFLICTS_LIMIT, 0)?;
        Ok(items)
    }

    /// Paginated conflict listing returning `(conflicts, total_matching, has_more)`.
    pub fn list_conflicts_bounded(
        &self,
        subject_key: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<ConflictRecord>, usize, bool)> {
        self.list_conflicts_as_of(subject_key, None, limit, offset)
    }

    /// Paginated conflict listing optionally reconstructed at a historical point in time (`as_of_ms`).
    pub fn list_conflicts_as_of(
        &self,
        subject_key: Option<&str>,
        as_of_ms: Option<u64>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<ConflictRecord>, usize, bool)> {
        let (items, total, has_more, _) =
            self.list_conflicts_as_of_checked(subject_key, as_of_ms, limit, offset)?;
        Ok((items, total, has_more))
    }

    /// Paginated conflict listing at `as_of_ms` with explicit `history_unavailable` reporting.
    pub fn list_conflicts_as_of_checked(
        &self,
        subject_key: Option<&str>,
        as_of_ms: Option<u64>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<ConflictRecord>, usize, bool, bool)> {
        let Some(cutoff) = as_of_ms else {
            let mut prefix = self.scoped_prefixed(PREFIX_CONFLICT, "");
            if let Some(subj) = subject_key {
                prefix.extend_from_slice(subj.as_bytes());
                prefix.push(0);
            }
            let keys = self
                .store
                .keys(ns::GRAPH_FACT, |k| k.starts_with(&prefix))?;
            let total = keys.len();
            if offset >= total || limit == 0 {
                return Ok((Vec::new(), total, offset < total, false));
            }
            let end = total.min(offset.saturating_add(limit));
            let has_more = end < total;
            let mut out = Vec::with_capacity(end - offset);
            for key in &keys[offset..end] {
                if let Some((_, raw)) = self.store.get(ns::GRAPH_FACT, key)? {
                    out.push(decode_json(&raw, "conflict record")?);
                }
            }
            return Ok((out, total, has_more, false));
        };

        // 历史查询必须从该时刻仍活跃的声明快照与当时溯源边重算，不能过滤可变 latest_observed_at_ms（C26）
        let claim_prefix = self.scoped_prefixed(PREFIX_CLAIM_LC, "");
        let claim_keys = self
            .store
            .keys(ns::GRAPH_FACT, |k| k.starts_with(&claim_prefix))?;

        if !claim_keys.is_empty() {
            let mut active_raw = Vec::new();
            let mut history_unavailable = false;
            for key in claim_keys {
                let Some((_, raw)) = self.store.get(ns::GRAPH_FACT, &key)? else {
                    continue;
                };
                let recs: Vec<ClaimLifecycleRecord> = decode_json(&raw, "claim lifecycle records")?;
                let (as_of_recs, unavail) = claims_as_of_checked(&recs, cutoff);
                history_unavailable |= unavail;
                for rec in as_of_recs {
                    if rec.status.is_active_or_unverified()
                        && subject_key.is_none_or(|s| rec.claim.subject_key == s)
                    {
                        active_raw.push(rec.claim);
                    }
                }
            }
            let filtered = self.detect_conflicts_for_claims(&active_raw, Some(cutoff))?;
            let total = filtered.len();
            if offset >= total || limit == 0 {
                return Ok((Vec::new(), total, offset < total, history_unavailable));
            }
            let end = total.min(offset.saturating_add(limit));
            let has_more = end < total;
            return Ok((
                filtered[offset..end].to_vec(),
                total,
                has_more,
                history_unavailable,
            ));
        }

        let mut prefix = self.scoped_prefixed(PREFIX_CONFLICT, "");
        if let Some(subj) = subject_key {
            prefix.extend_from_slice(subj.as_bytes());
            prefix.push(0);
        }
        let keys = self
            .store
            .keys(ns::GRAPH_FACT, |k| k.starts_with(&prefix))?;
        let history_unavailable = !keys.is_empty();
        let mut filtered = Vec::new();
        for key in &keys {
            let Some((_, raw)) = self.store.get(ns::GRAPH_FACT, key)? else {
                continue;
            };
            let mut cnf: ConflictRecord = decode_json(&raw, "conflict record")?;
            cnf.variants.retain_mut(|v| {
                v.evidence
                    .retain(|ev| ev.doc_id.is_empty() || v.latest_observed_at_ms <= cutoff);
                v.latest_observed_at_ms <= cutoff
            });
            if cnf.variants.len() >= 2 {
                if let Some(hint) = &cnf.preferred_hint
                    && hint.variant_index >= cnf.variants.len()
                {
                    cnf.preferred_hint = None;
                }
                filtered.push(cnf);
            }
        }
        let total = filtered.len();
        if offset >= total || limit == 0 {
            return Ok((Vec::new(), total, offset < total, history_unavailable));
        }
        let end = total.min(offset.saturating_add(limit));
        let has_more = end < total;
        Ok((
            filtered[offset..end].to_vec(),
            total,
            has_more,
            history_unavailable,
        ))
    }

    /// Load stored [`ClaimLifecycleRecord`] entries for `url_or_key` (resolving forward aliases).
    pub fn get_resource_claims(&self, url_or_key: &str) -> Result<Vec<ClaimLifecycleRecord>> {
        let canonical = self.resolve_resource_key(url_or_key)?;
        let key = self.scoped_prefixed(PREFIX_CLAIM_LC, &canonical);
        let Some((_, raw)) = self.store.get(ns::GRAPH_FACT, &key)? else {
            return Ok(Vec::new());
        };
        decode_json(&raw, "claim lifecycle records")
    }

    /// Persist [`ClaimLifecycleRecord`] entries for `url_or_key` (resolving forward aliases).
    pub fn put_resource_claims(
        &self,
        url_or_key: &str,
        records: &[ClaimLifecycleRecord],
    ) -> Result<()> {
        let canonical = self.resolve_resource_key(url_or_key)?;
        let key = self.scoped_prefixed(PREFIX_CLAIM_LC, &canonical);
        let val = encode_json(records, "claim lifecycle records")?;
        let now = to_ms(SystemTime::now());
        let mut tx = self.store.atomic_tx();
        tx.put(ns::GRAPH_FACT, &key, &val, now)?;
        tx.commit()
    }

    /// Reconcile previous stored claim lifecycles for `url_or_key` against a new observation and persist the updated records.
    pub fn reconcile_and_store_resource_claims(
        &self,
        url_or_key: &str,
        outcome: &ObservationOutcome,
        new_claims: &[ClaimRecord],
        observed_at_ms: u64,
    ) -> Result<(Vec<ClaimLifecycleRecord>, Vec<ClaimTransition>)> {
        self.reconcile_and_store_resource_claims_for_contract(
            url_or_key,
            DEFAULT_CONTRACT_ID,
            outcome,
            new_claims,
            observed_at_ms,
        )
    }

    /// 契约级声明协调与持久化：仅更新属于 `contract_id` 的槽位，避免同资源不同提取契约相互误撤回（C24）。
    pub fn reconcile_and_store_resource_claims_for_contract(
        &self,
        url_or_key: &str,
        contract_id: &str,
        outcome: &ObservationOutcome,
        new_claims: &[ClaimRecord],
        observed_at_ms: u64,
    ) -> Result<(Vec<ClaimLifecycleRecord>, Vec<ClaimTransition>)> {
        let prev = self.get_resource_claims(url_or_key)?;
        let had_prev = !prev.is_empty();
        let (updated, transitions) = reconcile_resource_claims_for_contract(
            &prev,
            outcome,
            new_claims,
            observed_at_ms,
            Some(contract_id),
        );
        if !updated.is_empty() || had_prev {
            self.put_resource_claims(url_or_key, &updated)?;
        }
        Ok((updated, transitions))
    }

    /// Query claim lifecycles for a single resource, returning explicit truncation indicator on `as_of_ms`.
    pub fn claims_for_resource_as_of_checked(
        &self,
        url_or_key: &str,
        as_of_ms: Option<u64>,
    ) -> Result<AsOfClaimsResult> {
        let recs = self.get_resource_claims(url_or_key)?;
        match as_of_ms {
            Some(ts) => {
                let (claims, history_unavailable) = claims_as_of_checked(&recs, ts);
                Ok(AsOfClaimsResult {
                    claims,
                    history_unavailable,
                })
            }
            None => Ok(AsOfClaimsResult {
                claims: recs,
                history_unavailable: false,
            }),
        }
    }

    /// Query claim lifecycles for a single resource, optionally reconstructed as of `as_of_ms`.
    pub fn claims_for_resource_as_of(
        &self,
        url_or_key: &str,
        as_of_ms: Option<u64>,
    ) -> Result<Vec<ClaimLifecycleRecord>> {
        Ok(self
            .claims_for_resource_as_of_checked(url_or_key, as_of_ms)?
            .claims)
    }

    /// List claim lifecycle records across the tenant scope with explicit history truncation reporting.
    pub fn list_claims_as_of_checked(
        &self,
        subject_or_url: Option<&str>,
        as_of_ms: Option<u64>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<ClaimLifecycleRecord>, usize, bool, bool)> {
        let filter_norm = subject_or_url.map(|s| {
            if s.contains("://") {
                initial_key(s)
            } else {
                s.to_string()
            }
        });
        let prefix = self.scoped_prefixed(PREFIX_CLAIM_LC, "");
        let keys = self
            .store
            .keys(ns::GRAPH_FACT, |k| k.starts_with(&prefix))?;

        let mut all = Vec::new();
        let mut history_unavailable = false;
        for key in keys {
            let Some((_, raw)) = self.store.get(ns::GRAPH_FACT, &key)? else {
                continue;
            };
            let recs: Vec<ClaimLifecycleRecord> = decode_json(&raw, "claim lifecycle records")?;
            let effective = match as_of_ms {
                Some(ts) => {
                    let (items, unavail) = claims_as_of_checked(&recs, ts);
                    history_unavailable |= unavail;
                    items
                }
                None => recs,
            };
            for rec in effective {
                if let Some(ref f) = filter_norm
                    && rec.claim.subject_key != *f
                    && rec.claim.resource_key != *f
                {
                    continue;
                }
                all.push(rec);
            }
        }
        all.sort_by(|a, b| {
            a.claim
                .subject_key
                .cmp(&b.claim.subject_key)
                .then_with(|| a.claim.normalized_field.cmp(&b.claim.normalized_field))
                .then_with(|| a.claim.resource_key.cmp(&b.claim.resource_key))
        });
        let total = all.len();
        if offset >= total || limit == 0 {
            return Ok((Vec::new(), total, offset < total, history_unavailable));
        }
        let end = total.min(offset.saturating_add(limit));
        let has_more = end < total;
        Ok((
            all[offset..end].to_vec(),
            total,
            has_more,
            history_unavailable,
        ))
    }

    /// List claim lifecycle records across the tenant scope, optionally filtered by `subject_key` or `resource_key` and `as_of_ms`.
    pub fn list_claims_bounded(
        &self,
        subject_or_url: Option<&str>,
        as_of_ms: Option<u64>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<ClaimLifecycleRecord>, usize, bool)> {
        let (items, total, has_more, _) =
            self.list_claims_as_of_checked(subject_or_url, as_of_ms, limit, offset)?;
        Ok((items, total, has_more))
    }
}
