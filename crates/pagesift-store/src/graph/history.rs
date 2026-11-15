//! Resource version history, alias traversal, and historical point-in-time (`as_of_ms`) queries.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::SystemTime;

use pagesift_graph::{VersionEdge, VersionRecord};
use serde::de::DeserializeOwned;

use super::{
    DEFAULT_GRAPH_HISTORY_LIMIT, GraphStore, MAX_ALIAS_HOPS, MAX_REV_ALIASES, PREFIX_ALIAS,
    PREFIX_REV, decode_json, encode_json, initial_key, scope_prefixed_key,
};
use crate::engine::{Tx, group_key, ns};
use crate::{Error, Result, to_ms};

/// Paginated/bounded history result for a resource key across its verified aliases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphHistoryResult {
    pub resource_key: String,
    pub versions: Vec<VersionRecord>,
    pub edges: Vec<VersionEdge>,
    pub truncated: bool,
    pub max_buffered_versions: usize,
    pub max_buffered_edges: usize,
}

pub(super) struct BoundedTopK<K, V> {
    pub(super) limit: usize,
    map: BTreeMap<K, V>,
    pub(super) truncated: bool,
    max_len_observed: usize,
}

impl<K: Ord, V> BoundedTopK<K, V> {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            limit,
            map: BTreeMap::new(),
            truncated: false,
            max_len_observed: 0,
        }
    }

    pub(super) fn insert(&mut self, key: K, val: V) {
        if self.map.contains_key(&key) {
            return;
        }
        if self.limit == 0 {
            self.truncated = true;
            return;
        }
        if self.map.len() < self.limit {
            self.map.insert(key, val);
            self.max_len_observed = self.max_len_observed.max(self.map.len());
            return;
        }
        self.truncated = true;
        let replace = self
            .map
            .first_key_value()
            .is_some_and(|(min_k, _)| &key > min_k);
        if replace {
            self.map.pop_first();
            self.map.insert(key, val);
            self.max_len_observed = self.max_len_observed.max(self.map.len());
        }
    }

    pub(super) fn into_sorted_vec(self) -> (Vec<V>, bool, usize) {
        let truncated = self.truncated;
        let max_len = self.max_len_observed;
        let vec = self.map.into_values().collect();
        (vec, truncated, max_len)
    }
}

impl<'a> GraphStore<'a> {
    /// Bind `alias_key -> canonical_key` so URL moves and same-site canonical declarations share history.
    pub fn bind_alias(&self, alias_key: &str, canonical_key: &str) -> Result<()> {
        let now = to_ms(SystemTime::now());
        let mut tx = self.store.tx();
        bind_alias_in_tx(&mut tx, self.scope_str(), alias_key, canonical_key, now)?;
        drop(tx);
        self.store.finish();
        Ok(())
    }

    /// Return the canonical resource key reached by following stored forward aliases.
    pub fn resolve_resource_key(&self, url_or_key: &str) -> Result<String> {
        let initial = initial_key(url_or_key);
        self.resolve_forward(&initial)
    }

    /// Returns true if `url_or_key` has no direct `VersionRecord` entries of its own
    /// and currently points via forward alias to a canonical key other than `expected_target`.
    pub fn is_foreign_alias_pointer(
        &self,
        url_or_key: &str,
        expected_target: &str,
    ) -> Result<bool> {
        let k = initial_key(url_or_key);
        let target = self.resolve_forward(&initial_key(expected_target))?;
        if k == target {
            return Ok(false);
        }
        if self.latest_tail(&k)?.is_some() {
            return Ok(false);
        }
        let resolved = self.resolve_forward(&k)?;
        Ok(resolved != k && resolved != target)
    }

    /// Look up the most recent [`VersionRecord`] for any URL or alias sharing `urls`.
    pub fn latest_version_for_urls(&self, urls: &[&str]) -> Result<Option<VersionRecord>> {
        let mut keys = BTreeSet::new();
        for url in urls {
            let k = initial_key(url);
            for member in self.alias_component(&k)? {
                keys.insert(member);
            }
        }
        let mut best: Option<VersionRecord> = None;
        for key in keys {
            if let Some(rec) = self.latest_tail(&key)? {
                let replace = best
                    .as_ref()
                    .is_none_or(|b| rec.observed_at_ms >= b.observed_at_ms);
                if replace {
                    best = Some(rec);
                }
            }
        }
        Ok(best)
    }

    /// Search stored versions across `urls` (and their alias components) newest-first,
    /// returning the most recent [`VersionRecord`] that satisfies `predicate`.
    pub fn find_version_for_urls(
        &self,
        urls: &[&str],
        mut predicate: impl FnMut(&VersionRecord) -> bool,
    ) -> Result<Option<VersionRecord>> {
        let mut keys = BTreeSet::new();
        for url in urls {
            let k = initial_key(url);
            for member in self.alias_component(&k)? {
                keys.insert(member);
            }
        }
        let mut best: Option<VersionRecord> = None;
        for key in keys {
            let scoped = self.scoped_key(&key);
            let Some(g) = self.store.group(ns::GRAPH_NODE, &scoped) else {
                continue;
            };
            if g.live == 0 || g.next <= g.head {
                continue;
            }
            let mut scanned = 0usize;
            for seq in (g.head..g.next).rev() {
                if scanned >= DEFAULT_GRAPH_HISTORY_LIMIT {
                    break;
                }
                if let Some((_, raw)) = self
                    .store
                    .get(ns::GRAPH_NODE, &group_key(&scoped, &seq.to_be_bytes())?)?
                {
                    scanned += 1;
                    let rec = decode_json::<VersionRecord>(&raw, "version record")?;
                    if predicate(&rec) {
                        let is_newer = best.as_ref().is_none_or(|b| {
                            (rec.observed_at_ms, rec.doc_id.as_str())
                                >= (b.observed_at_ms, b.doc_id.as_str())
                        });
                        if is_newer {
                            best = Some(rec);
                        }
                    }
                }
            }
        }
        Ok(best)
    }

    /// Return `(canonical_resource_key, versions, edges)` bounded to the latest [`DEFAULT_GRAPH_HISTORY_LIMIT`].
    pub fn history(
        &self,
        url_or_key: &str,
    ) -> Result<(String, Vec<VersionRecord>, Vec<VersionEdge>)> {
        let page = self.history_bounded(url_or_key, DEFAULT_GRAPH_HISTORY_LIMIT)?;
        Ok((page.resource_key, page.versions, page.edges))
    }

    /// Return bounded latest history across all linked aliases of `url_or_key`.
    pub fn history_bounded(&self, url_or_key: &str, limit: usize) -> Result<GraphHistoryResult> {
        self.history_as_of(url_or_key, None, limit)
    }

    /// Return bounded history across all linked aliases of `url_or_key`, optionally filtered to `<= as_of_ms`.
    pub fn history_as_of(
        &self,
        url_or_key: &str,
        as_of_ms: Option<u64>,
        limit: usize,
    ) -> Result<GraphHistoryResult> {
        let start = initial_key(url_or_key);
        let canonical = self.resolve_forward(&start)?;
        let component = self.alias_component(&start)?;

        let mut version_top = BoundedTopK::<(u64, String), VersionRecord>::new(limit);
        let mut edge_top = BoundedTopK::<(u64, String), VersionEdge>::new(limit);
        for key in component {
            let scoped = self.scoped_key(&key);
            self.scan_tail_into_top_k_filtered(
                ns::GRAPH_NODE,
                &scoped,
                limit,
                "version record",
                |rec: &VersionRecord| as_of_ms.is_none_or(|cutoff| rec.observed_at_ms <= cutoff),
                |rec: VersionRecord| ((rec.observed_at_ms, rec.doc_id.clone()), rec),
                &mut version_top,
            )?;
            self.scan_tail_into_top_k_filtered(
                ns::GRAPH_EDGE,
                &scoped,
                limit,
                "version edge",
                |edge: &VersionEdge| {
                    as_of_ms.is_none_or(|cutoff| edge.curr_observed_at_ms <= cutoff)
                },
                |edge: VersionEdge| ((edge.curr_observed_at_ms, edge.curr_doc_id.clone()), edge),
                &mut edge_top,
            )?;
        }
        let (versions, ver_trunc, max_buffered_versions) = version_top.into_sorted_vec();
        let (edges, edge_trunc, max_buffered_edges) = edge_top.into_sorted_vec();
        Ok(GraphHistoryResult {
            resource_key: canonical,
            versions,
            edges,
            truncated: ver_trunc || edge_trunc,
            max_buffered_versions,
            max_buffered_edges,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn scan_tail_into_top_k_filtered<K: Ord, V: DeserializeOwned>(
        &self,
        n: u8,
        list_key: &[u8],
        limit: usize,
        label: &str,
        filter: impl Fn(&V) -> bool,
        key_fn: impl Fn(V) -> (K, V),
        top_k: &mut BoundedTopK<K, V>,
    ) -> Result<()> {
        let Some(g) = self.store.group(n, list_key) else {
            return Ok(());
        };
        if g.live == 0 || g.next <= g.head {
            return Ok(());
        }
        if limit == 0 {
            top_k.truncated = true;
            return Ok(());
        }
        let mut matched = 0usize;
        for seq in (g.head..g.next).rev() {
            if let Some((_, raw)) = self
                .store
                .get(n, &group_key(list_key, &seq.to_be_bytes())?)?
            {
                let item = decode_json::<V>(&raw, label)?;
                if !filter(&item) {
                    continue;
                }
                if matched >= limit {
                    top_k.truncated = true;
                    break;
                }
                matched += 1;
                let (k, v) = key_fn(item);
                top_k.insert(k, v);
            }
        }
        Ok(())
    }

    pub(super) fn latest_tail(&self, resource_key: &str) -> Result<Option<VersionRecord>> {
        let list_key = self.scoped_key(resource_key);
        let Some(g) = self.store.group(ns::GRAPH_NODE, &list_key) else {
            return Ok(None);
        };
        if g.live == 0 || g.next <= g.head {
            return Ok(None);
        }
        for seq in (g.head..g.next).rev() {
            let key = group_key(&list_key, &seq.to_be_bytes())?;
            if let Some((_, raw)) = self.store.get(ns::GRAPH_NODE, &key)? {
                return decode_json(&raw, "version record").map(Some);
            }
        }
        Ok(None)
    }

    pub(super) fn resolve_forward(&self, start_key: &str) -> Result<String> {
        let mut curr = start_key.to_string();
        let mut seen = BTreeSet::new();
        seen.insert(curr.clone());
        for _ in 0..MAX_ALIAS_HOPS {
            let Some((_, raw)) = self
                .store
                .get(ns::GRAPH_FACT, &self.scoped_prefixed(PREFIX_ALIAS, &curr))?
            else {
                break;
            };
            let next = String::from_utf8(raw)
                .map_err(|e| Error::Corrupt(format!("graph alias utf8: {e}")))?;
            if !seen.insert(next.clone()) {
                break;
            }
            curr = next;
        }
        Ok(curr)
    }

    pub(super) fn alias_component(&self, start_key: &str) -> Result<Vec<String>> {
        let mut visited = BTreeSet::new();
        let mut queue = VecDeque::new();
        visited.insert(start_key.to_string());
        queue.push_back(start_key.to_string());

        while let Some(key) = queue.pop_front() {
            if visited.len() >= MAX_REV_ALIASES * 2 {
                break;
            }
            if let Some((_, raw)) = self
                .store
                .get(ns::GRAPH_FACT, &self.scoped_prefixed(PREFIX_ALIAS, &key))?
            {
                let next = String::from_utf8(raw)
                    .map_err(|e| Error::Corrupt(format!("graph alias utf8: {e}")))?;
                if visited.insert(next.clone()) {
                    queue.push_back(next);
                }
            }
            if let Some((_, raw)) = self
                .store
                .get(ns::GRAPH_FACT, &self.scoped_prefixed(PREFIX_REV, &key))?
            {
                let revs: Vec<String> = decode_json(&raw, "rev aliases")?;
                for prev in revs {
                    if visited.insert(prev.clone()) {
                        queue.push_back(prev);
                    }
                }
            }
        }
        Ok(visited.into_iter().collect())
    }
}

fn resolve_forward_in_tx(tx: &Tx<'_>, scope: &str, start_key: &str) -> Result<String> {
    let mut curr = start_key.to_string();
    let mut seen = BTreeSet::new();
    seen.insert(curr.clone());
    for _ in 0..MAX_ALIAS_HOPS {
        let Some((_, raw)) = tx.get(
            ns::GRAPH_FACT,
            &scope_prefixed_key(scope, PREFIX_ALIAS, &curr),
        )?
        else {
            break;
        };
        let next =
            String::from_utf8(raw).map_err(|e| Error::Corrupt(format!("graph alias utf8: {e}")))?;
        if !seen.insert(next.clone()) {
            break;
        }
        curr = next;
    }
    Ok(curr)
}

fn forward_reaches_in_tx(tx: &Tx<'_>, scope: &str, start_key: &str, needle: &str) -> Result<bool> {
    let mut curr = start_key.to_string();
    let mut seen = BTreeSet::new();
    seen.insert(curr.clone());
    for _ in 0..MAX_ALIAS_HOPS {
        if curr == needle {
            return Ok(true);
        }
        let Some((_, raw)) = tx.get(
            ns::GRAPH_FACT,
            &scope_prefixed_key(scope, PREFIX_ALIAS, &curr),
        )?
        else {
            break;
        };
        let next =
            String::from_utf8(raw).map_err(|e| Error::Corrupt(format!("graph alias utf8: {e}")))?;
        if !seen.insert(next.clone()) {
            break;
        }
        curr = next;
    }
    Ok(curr == needle)
}

pub(super) fn bind_alias_in_tx(
    tx: &mut Tx<'_>,
    scope: &str,
    alias_key: &str,
    canonical_key: &str,
    now: u64,
) -> Result<()> {
    if alias_key.is_empty() || canonical_key.is_empty() || alias_key == canonical_key {
        return Ok(());
    }
    let target = resolve_forward_in_tx(tx, scope, canonical_key)?;
    if alias_key == target || forward_reaches_in_tx(tx, scope, &target, alias_key)? {
        return Ok(());
    }
    let alias_store_key = scope_prefixed_key(scope, PREFIX_ALIAS, alias_key);
    if let Some((_, old_raw)) = tx.get(ns::GRAPH_FACT, &alias_store_key)? {
        let old_target = String::from_utf8(old_raw)
            .map_err(|e| Error::Corrupt(format!("graph alias utf8: {e}")))?;
        if old_target != target {
            let old_rev_key = scope_prefixed_key(scope, PREFIX_REV, &old_target);
            if let Some((_, raw)) = tx.get(ns::GRAPH_FACT, &old_rev_key)? {
                let mut old_revs: Vec<String> = decode_json(&raw, "rev aliases")?;
                let prev_len = old_revs.len();
                old_revs.retain(|k| k != alias_key);
                if old_revs.len() != prev_len {
                    if old_revs.is_empty() {
                        tx.delete(ns::GRAPH_FACT, &old_rev_key)?;
                    } else {
                        let encoded = encode_json(&old_revs, "rev aliases")?;
                        tx.put(ns::GRAPH_FACT, &old_rev_key, &encoded, now)?;
                    }
                }
            }
        }
    }
    tx.put(ns::GRAPH_FACT, &alias_store_key, target.as_bytes(), now)?;
    let rev_key = scope_prefixed_key(scope, PREFIX_REV, &target);
    let mut revs: Vec<String> = match tx.get(ns::GRAPH_FACT, &rev_key)? {
        Some((_, raw)) => decode_json(&raw, "rev aliases")?,
        None => Vec::new(),
    };
    if !revs.iter().any(|k| k == alias_key) {
        if revs.len() >= MAX_REV_ALIASES {
            revs.remove(0);
        }
        revs.push(alias_key.to_string());
        let raw = encode_json(&revs, "rev aliases")?;
        tx.put(ns::GRAPH_FACT, &rev_key, &raw, now)?;
    }
    Ok(())
}
