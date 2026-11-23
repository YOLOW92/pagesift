//! Reverse dependency & scope/negative-dependency index maintenance and transitive impact queries
//! for [`ResolveStore`].

use std::collections::BTreeSet;

use pagesift_graph::resolve::ConclusionRecord;

use crate::Result;
use crate::engine::{AtomicTx, ns};
use crate::resolve::{ResolveStore, decode_json, encode_json, validate_ident};

// [策略] 传递性下游结论影响扩散 BFS 最大跳数上限 32，防止异常深链耗尽 CPU
const MAX_TRANSITIVE_IMPACT_HOPS: usize = 32;

struct DepKeySets {
    units: BTreeSet<String>,
    resources: BTreeSet<String>,
    sources: BTreeSet<String>,
    subjects: BTreeSet<String>,
    parents: BTreeSet<String>,
    scopes: BTreeSet<String>,
}

impl DepKeySets {
    fn from_record(rec: Option<&ConclusionRecord>) -> Self {
        let Some(r) = rec else {
            return Self {
                units: BTreeSet::new(),
                resources: BTreeSet::new(),
                sources: BTreeSet::new(),
                subjects: BTreeSet::new(),
                parents: BTreeSet::new(),
                scopes: BTreeSet::new(),
            };
        };
        Self {
            units: r.dependent_unit_ids.iter().cloned().collect(),
            resources: r.dependent_resource_keys.iter().cloned().collect(),
            sources: r.scope.source_ids.iter().cloned().collect(),
            subjects: r.scope.subject_keys.iter().cloned().collect(),
            parents: r.parent_conclusion_ids.iter().cloned().collect(),
            scopes: r.scope.scope_predicates.iter().cloned().collect(),
        }
    }
}

impl<'a> ResolveStore<'a> {
    /// Load a reverse-index ID set (returns empty set when absent).
    pub(crate) fn load_id_set(&self, key: &[u8]) -> Result<BTreeSet<String>> {
        let Some((_, raw)) = self.store.get(ns::RESOLVE, key)? else {
            return Ok(BTreeSet::new());
        };
        decode_json(&raw, "resolve reverse index set")
    }

    fn load_id_set_in_tx(&self, tx: &AtomicTx<'_>, key: &[u8]) -> Result<BTreeSet<String>> {
        let Some((_, raw)) = tx.get(ns::RESOLVE, key)? else {
            return Ok(BTreeSet::new());
        };
        decode_json(&raw, "resolve reverse index set")
    }

    /// Atomically diff and update all reverse dependency and scope indexes for one conclusion.
    pub(crate) fn update_indexes_in_tx(
        &self,
        tx: &mut AtomicTx<'_>,
        prev: Option<&ConclusionRecord>,
        next: Option<&ConclusionRecord>,
        now: u64,
    ) -> Result<()> {
        let cid = next
            .map(|r| r.conclusion_id.as_str())
            .or_else(|| prev.map(|r| r.conclusion_id.as_str()))
            .unwrap_or_default();
        if cid.is_empty() {
            return Ok(());
        }
        let old_sets = DepKeySets::from_record(prev);
        let new_sets = DepKeySets::from_record(next);

        self.sync_index_dimension(tx, &old_sets.units, &new_sets.units, cid, now, |s, k| {
            s.dep_unit_key(k)
        })?;
        self.sync_index_dimension(
            tx,
            &old_sets.resources,
            &new_sets.resources,
            cid,
            now,
            |s, k| s.dep_resource_key(k),
        )?;
        self.sync_index_dimension(
            tx,
            &old_sets.sources,
            &new_sets.sources,
            cid,
            now,
            |s, k| s.dep_source_key(k),
        )?;
        self.sync_index_dimension(
            tx,
            &old_sets.subjects,
            &new_sets.subjects,
            cid,
            now,
            |s, k| s.dep_subject_key(k),
        )?;
        self.sync_index_dimension(
            tx,
            &old_sets.parents,
            &new_sets.parents,
            cid,
            now,
            |s, k| s.dep_conclusion_key(k),
        )?;
        self.sync_index_dimension(tx, &old_sets.scopes, &new_sets.scopes, cid, now, |s, k| {
            s.scope_key(k)
        })?;
        Ok(())
    }

    fn sync_index_dimension<F>(
        &self,
        tx: &mut AtomicTx<'_>,
        old_keys: &BTreeSet<String>,
        new_keys: &BTreeSet<String>,
        cid: &str,
        now: u64,
        key_fn: F,
    ) -> Result<()>
    where
        F: Fn(&Self, &str) -> Vec<u8>,
    {
        for removed in old_keys.difference(new_keys) {
            if removed.is_empty() {
                continue;
            }
            let k = key_fn(self, removed);
            let mut set = self.load_id_set_in_tx(tx, &k)?;
            if set.remove(cid) {
                if set.is_empty() {
                    tx.delete(ns::RESOLVE, &k)?;
                } else {
                    tx.put(
                        ns::RESOLVE,
                        &k,
                        &encode_json(&set, "resolve reverse index")?,
                        now,
                    )?;
                }
            }
        }
        for added in new_keys.difference(old_keys) {
            if added.is_empty() {
                continue;
            }
            let k = key_fn(self, added);
            let mut set = self.load_id_set_in_tx(tx, &k)?;
            if set.insert(cid.to_owned()) {
                tx.put(
                    ns::RESOLVE,
                    &k,
                    &encode_json(&set, "resolve reverse index")?,
                    now,
                )?;
            }
        }
        Ok(())
    }

    /// Lookup conclusions directly depending on any of `unit_ids`.
    pub fn find_by_units(&self, unit_ids: &[String]) -> Result<BTreeSet<String>> {
        let mut out = BTreeSet::new();
        for uid in unit_ids {
            if !uid.is_empty() {
                out.extend(self.load_id_set(&self.dep_unit_key(uid))?);
            }
        }
        Ok(out)
    }

    /// Lookup conclusions directly depending on any of `resource_keys`.
    pub fn find_by_resources(&self, resource_keys: &[String]) -> Result<BTreeSet<String>> {
        let mut out = BTreeSet::new();
        for rk in resource_keys {
            if !rk.is_empty() {
                out.extend(self.load_id_set(&self.dep_resource_key(rk))?);
            }
        }
        Ok(out)
    }

    /// Lookup all conclusions anchored to a Context/Loop `source_id`.
    pub fn find_by_source(&self, source_id: &str) -> Result<BTreeSet<String>> {
        validate_ident(source_id, "source_id")?;
        self.load_id_set(&self.dep_source_key(source_id))
    }

    /// Lookup conclusions sensitive to any of `scope_predicates` (negative/universal/superlative dependencies).
    pub fn find_by_scope_predicates(&self, predicates: &[String]) -> Result<BTreeSet<String>> {
        let mut out = BTreeSet::new();
        for pred in predicates {
            if !pred.is_empty() {
                out.extend(self.load_id_set(&self.scope_key(pred))?);
            }
        }
        Ok(out)
    }

    /// Lookup direct child conclusions depending on `parent_id`.
    pub fn find_children(&self, parent_id: &str) -> Result<BTreeSet<String>> {
        validate_ident(parent_id, "conclusion_id")?;
        self.load_id_set(&self.dep_conclusion_key(parent_id))
    }

    /// Compute bounded transitive reverse reachability `Reachable_{G^-1}(seeds)` along conclusion-to-conclusion edges.
    pub fn expand_downstream_conclusions(
        &self,
        seeds: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>> {
        let mut visited = seeds.clone();
        let mut frontier: Vec<String> = seeds.iter().cloned().collect();
        for _ in 0..MAX_TRANSITIVE_IMPACT_HOPS {
            if frontier.is_empty() {
                break;
            }
            let mut next_frontier = Vec::new();
            for curr in frontier {
                for child in self.load_id_set(&self.dep_conclusion_key(&curr))? {
                    if visited.insert(child.clone()) {
                        next_frontier.push(child);
                    }
                }
            }
            frontier = next_frontier;
        }
        Ok(visited)
    }
}
