//! Bounded in-process cache of committed [`ContextIndex`] snapshots keyed by `(tenant, source_id)`.
//!
//! Eliminates repeated full-index zstd decompression, JSON deserialization, and deep `BTreeMap`
//! cloning across Search, Pack, and multi-step Action loops while remaining strictly coherent
//! with `ContextStore` metadata (`instance_id`, `latest_delta_seq`, `indexed_tick_seq`, `spec_stamp`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pagesift_extract::context::ContextIndex;
use pagesift_graph::context::AssetSpecStamp;
use pagesift_store::ContextSourceMeta;

// [策略] 单 Scraper 实例最多缓存 32 个活跃数据源的内存索引，超出按最近最少访问代数淘汰
const MAX_CACHED_SOURCES: usize = 32;

struct Entry {
    instance_id: String,
    delta_seq: u64,
    indexed_tick_seq: u64,
    spec_stamp: AssetSpecStamp,
    index: Arc<ContextIndex>,
    access_gen: u64,
}

#[derive(Default)]
pub(crate) struct ContextIndexCache {
    clock: AtomicU64,
    map: Mutex<HashMap<(Option<String>, String), Entry>>,
}

impl ContextIndexCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Zero-I/O memory probe returning `(instance_id, delta_seq, indexed_tick_seq, spec_stamp, index)`
    /// for subsequent validation inside `Store::blocking` against committed `ContextSourceMeta`.
    pub(crate) fn peek(
        &self,
        tenant: Option<&str>,
        source_id: &str,
    ) -> Option<(String, u64, u64, AssetSpecStamp, Arc<ContextIndex>)> {
        let key = (
            tenant.filter(|t| !t.is_empty()).map(str::to_owned),
            source_id.to_owned(),
        );
        let mut guard = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.get_mut(&key)?;
        entry.access_gen = self.clock.fetch_add(1, Ordering::Relaxed) + 1;
        Some((
            entry.instance_id.clone(),
            entry.delta_seq,
            entry.indexed_tick_seq,
            entry.spec_stamp.clone(),
            entry.index.clone(),
        ))
    }

    /// Return a zero-copy `Arc<ContextIndex>` iff the cached entry matches the exact committed `meta`.
    #[cfg(test)]
    pub(crate) fn get_if_fresh(
        &self,
        tenant: Option<&str>,
        meta: &ContextSourceMeta,
    ) -> Option<Arc<ContextIndex>> {
        let key = (
            tenant.filter(|t| !t.is_empty()).map(str::to_owned),
            meta.source_id.clone(),
        );
        let mut guard = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.get_mut(&key)?;
        if entry.instance_id == meta.instance_id
            && entry.delta_seq == meta.latest_delta_seq
            && entry.indexed_tick_seq == meta.indexed_tick_seq
            && entry.spec_stamp == meta.spec_stamp
        {
            entry.access_gen = self.clock.fetch_add(1, Ordering::Relaxed) + 1;
            Some(entry.index.clone())
        } else {
            guard.remove(&key);
            None
        }
    }

    /// Store or refresh a committed `Arc<ContextIndex>` after `ContextStore::commit_delta` succeeds.
    pub(crate) fn put(
        &self,
        tenant: Option<&str>,
        meta: &ContextSourceMeta,
        index: Arc<ContextIndex>,
    ) {
        let key = (
            tenant.filter(|t| !t.is_empty()).map(str::to_owned),
            meta.source_id.clone(),
        );
        let access_gen = self.clock.fetch_add(1, Ordering::Relaxed) + 1;
        let mut guard = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if !guard.contains_key(&key)
            && guard.len() >= MAX_CACHED_SOURCES
            && let Some(victim_key) = guard
                .iter()
                .min_by_key(|(_, e)| e.access_gen)
                .map(|(k, _)| k.clone())
        {
            guard.remove(&victim_key);
        }
        guard.insert(
            key,
            Entry {
                instance_id: meta.instance_id.clone(),
                delta_seq: meta.latest_delta_seq,
                indexed_tick_seq: meta.indexed_tick_seq,
                spec_stamp: meta.spec_stamp.clone(),
                index,
                access_gen,
            },
        );
    }

    pub(crate) fn invalidate(&self, tenant: Option<&str>, source_id: &str) {
        let key = (
            tenant.filter(|t| !t.is_empty()).map(str::to_owned),
            source_id.to_owned(),
        );
        let mut guard = self.map.lock().unwrap_or_else(|e| e.into_inner());
        guard.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::time::Instant;

    use pagesift_extract::context::{ContextIndex, ContextLearnState, compile_context_pack};
    use pagesift_graph::context::{
        AssetSpecStamp, ContextPackRequest, ContextSearchRequest, UnitMatchMode,
    };
    use pagesift_store::context::ContextSourceMeta;
    use pagesift_store::{Config, Store};

    use super::{ContextIndexCache, MAX_CACHED_SOURCES};
    use crate::context::ingest::{ContextDocumentInput, ingest_documents, load_corpus};
    use crate::{Scraper, ScraperConfig};

    fn dummy_meta(source_id: &str, inst: &str, seq: u64) -> ContextSourceMeta {
        ContextSourceMeta {
            source_id: source_id.to_owned(),
            instance_id: inst.to_owned(),
            indexed_tick_seq: seq,
            latest_delta_seq: seq,
            earliest_retained_delta_seq: 1,
            spec_stamp: AssetSpecStamp::default(),
            updated_at_ms: 1_700_000_000_000,
            retired_seq_start: 0,
            retired_seq_end: 0,
            resource_keys: BTreeSet::new(),
            history_resource_keys: BTreeSet::new(),
        }
    }

    #[test]
    fn cache_lru_eviction_tenant_isolation_and_version_guard() {
        let cache = ContextIndexCache::default();
        let idx = Arc::new(ContextIndex::new(AssetSpecStamp::default()));

        for i in 0..MAX_CACHED_SOURCES {
            let id = format!("src-{i}");
            let m = dummy_meta(&id, "inst-1", 1);
            cache.put(Some("tenant-a"), &m, Arc::clone(&idx));
        }
        // Touch src-0 after filling all 32 slots so src-1 becomes the oldest LRU victim!
        assert!(
            cache
                .get_if_fresh(Some("tenant-a"), &dummy_meta("src-0", "inst-1", 1))
                .is_some()
        );
        let overflow_meta = dummy_meta(&format!("src-{MAX_CACHED_SOURCES}"), "inst-1", 1);
        cache.put(Some("tenant-a"), &overflow_meta, Arc::clone(&idx));
        assert!(
            cache
                .get_if_fresh(Some("tenant-a"), &dummy_meta("src-0", "inst-1", 1))
                .is_some(),
            "recently accessed src-0 should survive LRU eviction"
        );
        assert!(
            cache
                .get_if_fresh(Some("tenant-a"), &dummy_meta("src-1", "inst-1", 1))
                .is_none(),
            "oldest untouched src-1 should be evicted when exceeding MAX_CACHED_SOURCES"
        );
        assert!(
            cache
                .get_if_fresh(Some("tenant-b"), &dummy_meta("src-0", "inst-1", 1))
                .is_none()
        );
        assert!(
            cache
                .get_if_fresh(Some("tenant-a"), &dummy_meta("src-0", "inst-1", 2))
                .is_none()
        );
        assert!(
            cache
                .get_if_fresh(Some("tenant-a"), &dummy_meta("src-0", "inst-2", 1))
                .is_none()
        );
    }

    #[test]
    fn perf_exact_key_lookup_and_arc_index_cache_speedup() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut sc = Config::new(dir.path());
        sc.background = false;
        let store = Store::open(sc).unwrap();

        rt.block_on(async {
            let cfg = ScraperConfig {
                store: Some(store.clone()),
                ..ScraperConfig::default()
            };
            let scraper = Scraper::new(cfg).unwrap();

            for s in 0..25 {
                let sid = format!("other-src-{s}");
                let docs: Vec<ContextDocumentInput> = (0..30)
                    .map(|r| ContextDocumentInput {
                        url: format!("https://corp-{s}.example/doc-{r}"),
                        markdown: "# Overview\n\nService latency target is 25ms p99.\n".to_owned(),
                        title: Some("Doc".to_owned()),
                        source_kind: None,
                        observed_at_ms: Some(1_750_000_000_000),
                        valid_from: None,
                        valid_until: None,
                        subject_key: None,
                        origin_root: Some("corp.example".to_owned()),
                        origin_independent: true,
                        status_code: Some(200),
                        unreliable: false,
                        confirmed_gone: false,
                    })
                    .collect();
                ingest_documents(
                    &scraper,
                    None,
                    &sid,
                    docs,
                    UnitMatchMode::BoundedUot,
                    None,
                    true,
                )
                .await
                .unwrap();
            }

            let bench_docs: Vec<ContextDocumentInput> = (0..120)
                .map(|d| ContextDocumentInput {
                    url: format!("https://kb.example/spec-{d}"),
                    markdown: format!(
                        "# Subsystem {d} Specification\n\n\
                         Max burst throughput for subsystem {d} is {} req/s unless regional quota applies.\n\n\
                         | Tier | Limit | Timeout |\n|---|---|---|\n| Standard | {} | 30s |\n| Enterprise | {} | 120s |\n",
                        100 + d * 10,
                        100 + d,
                        1000 + d * 20
                    ),
                    title: Some("Subsystem Spec".to_owned()),
                    source_kind: None,
                    observed_at_ms: Some(1_750_000_000_000),
                    valid_from: None,
                    valid_until: None,
                    subject_key: None,
                    origin_root: Some("kb.example".to_owned()),
                    origin_independent: true,
                    status_code: Some(200),
                    unreliable: false,
                    confirmed_gone: false,
                })
                .collect();
            ingest_documents(
                &scraper,
                None,
                "bench-kb",
                bench_docs,
                UnitMatchMode::BoundedUot,
                None,
                true,
            )
            .await
            .unwrap();

            let ctx_store = store.context_for(None);
            let blob = ctx_store.load_index_blob("bench-kb").unwrap().unwrap();
            let iters = 25usize;
            let search_req = ContextSearchRequest {
                query: "Subsystem 42 burst throughput Enterprise regional quota".to_owned(),
                limit: 16,
                ..ContextSearchRequest::default()
            };
            let pack_req = ContextPackRequest {
                search: search_req.clone(),
                budget_tokens: 1024,
                ..ContextPackRequest::default()
            };

            // wall-clock: diagnostic eprintln only; no assertion depends on elapsed time
            let t_cold = Instant::now();
            for _ in 0..iters {
                scraper.context_cache().invalidate(None, "bench-kb");
                let corpus = load_corpus(&scraper, None, &["bench-kb".to_owned()], None)
                    .await
                    .unwrap();
                let mut learn = ContextLearnState::default();
                let report = corpus.index.search(
                    &search_req,
                    &corpus.conflicts,
                    &corpus.fact_clusters,
                    corpus.visibilities,
                    &mut learn,
                );
                let pack = compile_context_pack(&pack_req, &report, 1_750_000_000_000);
                assert!(!pack.compiled_context.is_empty());
            }
            let cold_us = t_cold.elapsed().as_micros() as f64 / iters as f64;

            // Prime cache once, verify zero-copy `Arc::ptr_eq`, and log warm hit latency
            let primed = load_corpus(&scraper, None, &["bench-kb".to_owned()], None)
                .await
                .unwrap();
            // wall-clock: diagnostic eprintln only; no assertion depends on elapsed time
            let t_warm = Instant::now();
            for _ in 0..iters {
                let corpus = load_corpus(&scraper, None, &["bench-kb".to_owned()], None)
                    .await
                    .unwrap();
                assert!(
                    Arc::ptr_eq(&primed.index, &corpus.index),
                    "warm load_corpus must reuse exact Arc<ContextIndex> without deserializing"
                );
                let mut learn = ContextLearnState::default();
                let report = corpus.index.search(
                    &search_req,
                    &corpus.conflicts,
                    &corpus.fact_clusters,
                    corpus.visibilities,
                    &mut learn,
                );
                let pack = compile_context_pack(&pack_req, &report, 1_750_000_000_000);
                assert!(!pack.compiled_context.is_empty());
            }
            let warm_us = t_warm.elapsed().as_micros() as f64 / iters as f64;

            eprintln!(
                "[perf] 120-doc load_corpus+Search+Pack cold ({} KiB JSON): {:.1} us/op vs Arc<ContextIndex> cache hit: {:.1} us/op ({:.2}x faster)",
                blob.len() / 1024,
                cold_us,
                warm_us,
                cold_us / warm_us.max(0.1)
            );
            scraper.shutdown().await;
        });
    }
}
