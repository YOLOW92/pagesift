//! Persistent conflict records and claim lifecycle (`ClaimLifecycleRecord`) storage and queries.

use std::time::SystemTime;

use pagesift_graph::{
    ClaimLifecycleRecord, ClaimRecord, ClaimTransition, ConflictRecord, ObservationOutcome,
    claims_as_of, reconcile_resource_claims,
};

use super::{
    DEFAULT_GRAPH_CONFLICTS_LIMIT, GraphStore, PREFIX_CLAIM_LC, PREFIX_CONFLICT, decode_json,
    encode_json, initial_key,
};
use crate::engine::ns;
use crate::{Result, to_ms};

pub const DEFAULT_GRAPH_CLAIMS_LIMIT: usize = 4096; // [策略] 单次声明生命周期查询默认返回上限

impl<'a> GraphStore<'a> {
    /// Persist conflict records idempotently into `ns::GRAPH_FACT`.
    pub fn put_conflicts(&self, conflicts: &[ConflictRecord]) -> Result<()> {
        if conflicts.is_empty() {
            return Ok(());
        }
        let now = to_ms(SystemTime::now());
        let mut tx = self.store.tx();
        for conflict in conflicts {
            let key = self.scoped_compound(
                PREFIX_CONFLICT,
                &conflict.subject_key,
                &conflict.conflict_id,
            );
            let val = encode_json(conflict, "conflict record")?;
            tx.put(ns::GRAPH_FACT, &key, &val, now)?;
        }
        drop(tx);
        self.store.finish();
        Ok(())
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

    /// Paginated conflict listing optionally filtered to a historical point in time (`as_of_ms`).
    pub fn list_conflicts_as_of(
        &self,
        subject_key: Option<&str>,
        as_of_ms: Option<u64>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<ConflictRecord>, usize, bool)> {
        let mut prefix = self.scoped_prefixed(PREFIX_CONFLICT, "");
        if let Some(subj) = subject_key {
            prefix.extend_from_slice(subj.as_bytes());
            prefix.push(0);
        }
        let keys = self
            .store
            .keys(ns::GRAPH_FACT, |k| k.starts_with(&prefix))?;

        let Some(cutoff) = as_of_ms else {
            let total = keys.len();
            if offset >= total || limit == 0 {
                return Ok((Vec::new(), total, offset < total));
            }
            let end = total.min(offset.saturating_add(limit));
            let has_more = end < total;
            let mut out = Vec::with_capacity(end - offset);
            for key in &keys[offset..end] {
                if let Some((_, raw)) = self.store.get(ns::GRAPH_FACT, key)? {
                    out.push(decode_json(&raw, "conflict record")?);
                }
            }
            return Ok((out, total, has_more));
        };

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
            return Ok((Vec::new(), total, offset < total));
        }
        let end = total.min(offset.saturating_add(limit));
        let has_more = end < total;
        Ok((filtered[offset..end].to_vec(), total, has_more))
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
        self.store.put(ns::GRAPH_FACT, &key, &val, now)?;
        Ok(())
    }

    /// Reconcile previous stored claim lifecycles for `url_or_key` against a new observation and persist the updated records.
    pub fn reconcile_and_store_resource_claims(
        &self,
        url_or_key: &str,
        outcome: &ObservationOutcome,
        new_claims: &[ClaimRecord],
        observed_at_ms: u64,
    ) -> Result<(Vec<ClaimLifecycleRecord>, Vec<ClaimTransition>)> {
        let prev = self.get_resource_claims(url_or_key)?;
        let had_prev = !prev.is_empty();
        let (updated, transitions) =
            reconcile_resource_claims(&prev, outcome, new_claims, observed_at_ms);
        if !updated.is_empty() || had_prev {
            self.put_resource_claims(url_or_key, &updated)?;
        }
        Ok((updated, transitions))
    }

    /// Query claim lifecycles for a single resource, optionally reconstructed as of `as_of_ms`.
    pub fn claims_for_resource_as_of(
        &self,
        url_or_key: &str,
        as_of_ms: Option<u64>,
    ) -> Result<Vec<ClaimLifecycleRecord>> {
        let recs = self.get_resource_claims(url_or_key)?;
        Ok(match as_of_ms {
            Some(ts) => claims_as_of(&recs, ts),
            None => recs,
        })
    }

    /// List claim lifecycle records across the tenant scope, optionally filtered by `subject_key` or `resource_key` and `as_of_ms`.
    pub fn list_claims_bounded(
        &self,
        subject_or_url: Option<&str>,
        as_of_ms: Option<u64>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<ClaimLifecycleRecord>, usize, bool)> {
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
        for key in keys {
            let Some((_, raw)) = self.store.get(ns::GRAPH_FACT, &key)? else {
                continue;
            };
            let recs: Vec<ClaimLifecycleRecord> = decode_json(&raw, "claim lifecycle records")?;
            let effective = match as_of_ms {
                Some(ts) => claims_as_of(&recs, ts),
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
            return Ok((Vec::new(), total, offset < total));
        }
        let end = total.min(offset.saturating_add(limit));
        let has_more = end < total;
        Ok((all[offset..end].to_vec(), total, has_more))
    }
}
