use std::collections::BTreeMap;

use pagesift_graph::ObservationOutcome;
use pagesift_graph::SourceKind;
use pagesift_graph::context::{
    AssetSpecStamp, ConsumerCheckpoint, ContextDelta, KnowledgeUnit, MaintenanceMode, ReplayStatus,
    UnitChangeKind, UnitKind, UnitLineage, UnitLocation, UnitStatus, UnitTransition,
};

use super::{ContextCommitOutcome, ContextCommitRequest};
use crate::{Config, DataSourceSpec, FreshnessPolicy, Store, SyncBudget, SyncPolicy};

fn make_unit(id_seed: &str, text: &str) -> KnowledgeUnit {
    let uid = KnowledgeUnit::mint_unit_id(
        "https://example.com/doc",
        UnitKind::Paragraph,
        "Overview",
        id_seed,
        0,
    );
    let rev = KnowledgeUnit::compute_revision_id(&uid, id_seed, &[], None);
    KnowledgeUnit {
        unit_id: uid,
        revision_id: rev,
        content_hash: id_seed.to_owned(),
        kind: UnitKind::Paragraph,
        text: text.to_owned(),
        tokens: 8,
        subject_key: None,
        entity_anchors: Vec::new(),
        has_negation_or_qualifier: false,
        has_units_or_currency: false,
        location: UnitLocation {
            doc_id: format!("doc_{id_seed}"),
            resource_key: "https://example.com/doc".to_owned(),
            source_url: "https://example.com/doc".to_owned(),
            source_kind: SourceKind::Html,
            block_ids: Vec::new(),
            byte_start: 0,
            byte_end: text.len(),
            page_number: None,
            heading_path: vec!["Overview".to_owned()],
            code_language: None,
            table_meta: None,
        },
        lineage: UnitLineage::created(),
        status: UnitStatus::Active,
        temporal: None,
        origin_root: "https://example.com".to_owned(),
        origin_independent: true,
        claim_ids: Vec::new(),
        evidence_spans: Vec::new(),
        observed_at_ms: 1_700_000_000_000,
    }
}

fn empty_delta(source_id: &str, u: &KnowledgeUnit) -> ContextDelta {
    ContextDelta {
        source_id: source_id.to_owned(),
        instance_id: String::new(),
        delta_seq: 0,
        store_tick_seq: 0,
        created_at_ms: 1_700_000_000_000,
        observation_outcome: ObservationOutcome::VerifiedContent,
        unit_transitions: vec![UnitTransition {
            unit_id: u.unit_id.clone(),
            change_kind: UnitChangeKind::Appeared,
            prev_revision_id: None,
            curr_revision_id: Some(u.revision_id.clone()),
            resource_key: u.location.resource_key.clone(),
            doc_id: Some(u.location.doc_id.clone()),
            parent_unit_ids: Vec::new(),
            child_unit_ids: Vec::new(),
            reason: None,
        }],
        invalidated_assets: Vec::new(),
        reused_asset_count: 0,
        recomputed_asset_count: 4,
        maintenance_mode: MaintenanceMode::Incremental,
        knowledge_delta: None,
    }
}

#[test]
fn idempotent_replay_out_of_order_ack_aba_recreate_and_gc_immunity() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(tmp.path());
    cfg.background = false;
    cfg.sync = SyncPolicy::Never;
    cfg.max_bytes = Some(8 * 1024); // Very tight max_bytes to force GC eviction of cache/snapshots!
    let store = Store::open(cfg).unwrap();

    // 1. Create a Sync source so it has a real `instance_id`
    let spec = store
        .sync()
        .upsert_source(DataSourceSpec {
            id: "src-1".to_owned(),
            name: Some("Docs".to_owned()),
            seeds: vec!["https://example.com/doc".to_owned()],
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            max_depth: 1,
            allow_subdomains: false,
            freshness: FreshnessPolicy::default(),
            budget: SyncBudget::default(),
            extract_schema: None,
            extract_prompt: None,
            required_fields: Vec::new(),
            allow_browser: false,
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
            revision: 1,
            instance_id: String::new(),
        })
        .unwrap();
    let inst_1 = spec.instance_id.clone();

    // Commit tick 1 to ContextStore
    let u1 = make_unit("h1", "First version paragraph stored in ns::CONTEXT.");
    let mut map1 = BTreeMap::new();
    map1.insert("https://example.com/doc".to_owned(), vec![u1.clone()]);
    let out1 = store
        .context()
        .commit_delta(ContextCommitRequest {
            source_id: "src-1",
            instance_id: &inst_1,
            store_tick_seq: 1,
            spec_stamp: AssetSpecStamp::default(),
            resource_units: &map1,
            retired_units: &[],
            index_blob: Some(b"index-v1"),
            delta: empty_delta("src-1", &u1),
            allow_same_zero_tick: false,
        })
        .unwrap();
    assert_eq!(out1, ContextCommitOutcome::Committed { delta_seq: 1 });

    // Replaying tick 1 again MUST be idempotently skipped without bumping `delta_seq`!
    let replay = store
        .context()
        .commit_delta(ContextCommitRequest {
            source_id: "src-1",
            instance_id: &inst_1,
            store_tick_seq: 1,
            spec_stamp: AssetSpecStamp::default(),
            resource_units: &map1,
            retired_units: &[],
            index_blob: Some(b"index-v1"),
            delta: empty_delta("src-1", &u1),
            allow_same_zero_tick: false,
        })
        .unwrap();
    assert_eq!(
        replay,
        ContextCommitOutcome::IdempotentReplaySkipped {
            existing_delta_seq: 1
        }
    );

    // Commit tick 2
    let u2 = make_unit("h2", "Second version paragraph.");
    let mut map2 = BTreeMap::new();
    map2.insert("https://example.com/doc".to_owned(), vec![u2.clone()]);
    let out2 = store
        .context()
        .commit_delta(ContextCommitRequest {
            source_id: "src-1",
            instance_id: &inst_1,
            store_tick_seq: 2,
            spec_stamp: AssetSpecStamp::default(),
            resource_units: &map2,
            retired_units: &[u1],
            index_blob: Some(b"index-v2"),
            delta: empty_delta("src-1", &u2),
            allow_same_zero_tick: false,
        })
        .unwrap();
    assert_eq!(out2, ContextCommitOutcome::Committed { delta_seq: 2 });

    // Flood `Snapshots` (`SNAP_BLOB`) and force `store.gc()`: `ContextStore` units MUST NOT be evicted!
    for i in 0..40 {
        let big = format!("snapshot-payload-{i}-{}", "x".repeat(1024));
        let _ = store
            .snapshots()
            .put(&format!("https://example.com/p{i}"), big.as_bytes());
    }
    let _gc = store.gc().unwrap();
    let loaded = store.context().load_all_live_units("src-1").unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].text, "Second version paragraph.");

    // Consumer polling + out-of-order / duplicate ACK test
    let fp = AssetSpecStamp::default().fingerprint();
    let poll1 = store
        .context()
        .poll_consumer_deltas("qdrant-worker", "src-1", Some(&fp), 10)
        .unwrap();
    assert_eq!(poll1.status, ReplayStatus::IncrementalReady);
    assert_eq!(poll1.deltas.len(), 2);

    // Ack seq 2, then send out-of-order stale ack seq 1 -> checkpoint MUST stay at 2!
    store
        .context()
        .ack_consumer_checkpoint(ConsumerCheckpoint {
            consumer_id: "qdrant-worker".to_owned(),
            source_id: "src-1".to_owned(),
            instance_id: inst_1.clone(),
            acked_delta_seq: 2,
            spec_fingerprint: fp.clone(),
            updated_at_ms: 3_000,
        })
        .unwrap();
    let after_stale = store
        .context()
        .ack_consumer_checkpoint(ConsumerCheckpoint {
            consumer_id: "qdrant-worker".to_owned(),
            source_id: "src-1".to_owned(),
            instance_id: inst_1.clone(),
            acked_delta_seq: 1,
            spec_fingerprint: fp.clone(),
            updated_at_ms: 3_001,
        })
        .unwrap();
    assert_eq!(after_stale.acked_delta_seq, 2);

    // Out-of-order stale ACK omitting `spec_fingerprint` ("") must also stay at 2 and keep `fp`!
    let after_blank_fp_stale = store
        .context()
        .ack_consumer_checkpoint(ConsumerCheckpoint {
            consumer_id: "qdrant-worker".to_owned(),
            source_id: "src-1".to_owned(),
            instance_id: inst_1.clone(),
            acked_delta_seq: 1,
            spec_fingerprint: String::new(),
            updated_at_ms: 3_002,
        })
        .unwrap();
    assert_eq!(after_blank_fp_stale.acked_delta_seq, 2);
    assert_eq!(after_blank_fp_stale.spec_fingerprint, fp);

    // Model/tokenizer spec upgrade triggers FullResyncRequired!
    let upgraded = AssetSpecStamp {
        model_id: "embed-v2".to_owned(),
        ..AssetSpecStamp::default()
    };
    let poll_spec_change = store
        .context()
        .poll_consumer_deltas("qdrant-worker", "src-1", Some(&upgraded.fingerprint()), 10)
        .unwrap();
    assert!(matches!(
        poll_spec_change.status,
        ReplayStatus::FullResyncRequired { .. }
    ));

    // Write failure injection + reopen recovery: failed commit leaves state 100% intact!
    store.fail_writes(true);
    let u3 = make_unit("h3", "Should roll back on write failure.");
    let mut map3 = BTreeMap::new();
    map3.insert("https://example.com/doc".to_owned(), vec![u3.clone()]);
    assert!(
        store
            .context()
            .commit_delta(ContextCommitRequest {
                source_id: "src-1",
                instance_id: &inst_1,
                store_tick_seq: 3,
                spec_stamp: AssetSpecStamp::default(),
                resource_units: &map3,
                retired_units: &[],
                index_blob: Some(b"index-v3"),
                delta: empty_delta("src-1", &u3),
                allow_same_zero_tick: false,
            })
            .is_err()
    );
    store.fail_writes(false);
    drop(store);

    // Reopen store from disk and verify ABA source deletion + recreation isolation
    let store2 = Store::open_dir(tmp.path()).unwrap();
    assert_eq!(
        store2
            .context()
            .get_meta("src-1")
            .unwrap()
            .unwrap()
            .latest_delta_seq,
        2
    );

    store2.sync().delete_source("src-1").unwrap();
    let recreated = store2.sync().upsert_source(spec).unwrap();
    assert_ne!(recreated.instance_id, inst_1);
    let vis_mismatch = store2.context().visibility("src-1").unwrap();
    assert!(!vis_mismatch.instance_matched);
    assert!(!vis_mismatch.is_caught_up);

    // Committing under new `instance_id` purges stale units and alerts external consumer to FullResync!
    let mut map_new = BTreeMap::new();
    map_new.insert("https://example.com/reborn".to_owned(), vec![u3.clone()]);
    let out_reborn = store2
        .context()
        .commit_delta(ContextCommitRequest {
            source_id: "src-1",
            instance_id: &recreated.instance_id,
            store_tick_seq: 1,
            spec_stamp: AssetSpecStamp::default(),
            resource_units: &map_new,
            retired_units: &[],
            index_blob: Some(b"index-reborn"),
            delta: empty_delta("src-1", &u3),
            allow_same_zero_tick: false,
        })
        .unwrap();
    assert_eq!(
        out_reborn,
        ContextCommitOutcome::InstanceReplacedAndCommitted { delta_seq: 1 }
    );
    assert!(
        store2
            .context()
            .load_resource_units("src-1", "https://example.com/doc")
            .unwrap()
            .is_empty(),
        "stale units from deleted ABA instance must be purged"
    );
    let poll_aba = store2
        .context()
        .poll_consumer_deltas("qdrant-worker", "src-1", Some(&fp), 10)
        .unwrap();
    assert!(matches!(
        poll_aba.status,
        ReplayStatus::FullResyncRequired { .. }
    ));
}

#[test]
fn colon_ids_deleted_sync_ghosts_late_cross_instance_acks_and_o1_ring_trim() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(tmp.path());
    cfg.background = false;
    cfg.sync = SyncPolicy::Never;
    let store = Store::open(cfg).unwrap();

    // 1. Key separator isolation when `source_id` or `consumer_id` contains ':'
    let u_root = make_unit("root", "Root project unit.");
    let u_sub = make_unit("sub", "Subproject unit.");
    let mut m_root = BTreeMap::new();
    m_root.insert("sub:https://example.com/a".to_owned(), vec![u_root.clone()]);
    let mut m_sub = BTreeMap::new();
    m_sub.insert("https://example.com/a".to_owned(), vec![u_sub.clone()]);

    store
        .context()
        .commit_delta(ContextCommitRequest {
            source_id: "org",
            instance_id: "manual-org",
            store_tick_seq: 1,
            spec_stamp: AssetSpecStamp::default(),
            resource_units: &m_root,
            retired_units: &[],
            index_blob: Some(b"idx-org"),
            delta: empty_delta("org", &u_root),
            allow_same_zero_tick: false,
        })
        .unwrap();
    store
        .context()
        .commit_delta(ContextCommitRequest {
            source_id: "org:sub",
            instance_id: "manual-sub",
            store_tick_seq: 1,
            spec_stamp: AssetSpecStamp::default(),
            resource_units: &m_sub,
            retired_units: &[],
            index_blob: Some(b"idx-sub"),
            delta: empty_delta("org:sub", &u_sub),
            allow_same_zero_tick: false,
        })
        .unwrap();

    assert_eq!(store.context().load_all_live_units("org").unwrap().len(), 1);
    assert_eq!(
        store.context().load_all_live_units("org").unwrap()[0].text,
        "Root project unit."
    );
    assert_eq!(
        store
            .context()
            .load_all_live_units("org:sub")
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store.context().load_all_live_units("org:sub").unwrap()[0].text,
        "Subproject unit."
    );

    // Purging "org" MUST NOT touch "org:sub"!
    store.context().purge_source("org").unwrap();
    assert!(!store.context().has_meta("org").unwrap());
    assert!(store.context().has_meta("org:sub").unwrap());
    assert_eq!(
        store
            .context()
            .load_all_live_units("org:sub")
            .unwrap()
            .len(),
        1
    );

    // Checkpoints with ':' in `consumer_id` vs `source_id` MUST NOT collide!
    let fp = AssetSpecStamp::default().fingerprint();
    store
        .context()
        .ack_consumer_checkpoint(ConsumerCheckpoint {
            consumer_id: "worker:org".to_owned(),
            source_id: "sub".to_owned(),
            instance_id: "inst-x".to_owned(),
            acked_delta_seq: 7,
            spec_fingerprint: fp.clone(),
            updated_at_ms: 100,
        })
        .unwrap();
    assert!(
        store
            .context()
            .get_consumer_checkpoint("worker", "org:sub")
            .unwrap()
            .is_none()
    );

    // 2. Deleted SyncStore source (without recreation) MUST NOT leak ghost units or claim `is_caught_up`!
    let sync_spec = store
        .sync()
        .upsert_source(DataSourceSpec {
            id: "ephemeral-sync".to_owned(),
            name: None,
            seeds: vec!["https://example.com/doc".to_owned()],
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            max_depth: 0,
            allow_subdomains: false,
            freshness: FreshnessPolicy::default(),
            budget: SyncBudget::default(),
            extract_schema: None,
            extract_prompt: None,
            required_fields: Vec::new(),
            allow_browser: false,
            created_at_ms: 2_000,
            updated_at_ms: 2_000,
            revision: 1,
            instance_id: String::new(),
        })
        .unwrap();

    store
        .context()
        .commit_delta(ContextCommitRequest {
            source_id: "ephemeral-sync",
            instance_id: &sync_spec.instance_id,
            store_tick_seq: 1,
            spec_stamp: AssetSpecStamp::default(),
            resource_units: &m_sub,
            retired_units: &[],
            index_blob: Some(b"idx-sync"),
            delta: empty_delta("ephemeral-sync", &u_sub),
            allow_same_zero_tick: false,
        })
        .unwrap();

    // Same-tick spec_stamp upgrade MUST NOT be skipped by idempotent replay check!
    let upgraded_spec = AssetSpecStamp {
        tokenizer_version: 2,
        ..AssetSpecStamp::default()
    };
    let out_spec_up = store
        .context()
        .commit_delta(ContextCommitRequest {
            source_id: "ephemeral-sync",
            instance_id: &sync_spec.instance_id,
            store_tick_seq: 1,
            spec_stamp: upgraded_spec.clone(),
            resource_units: &m_sub,
            retired_units: &[],
            index_blob: Some(b"idx-sync-v2"),
            delta: empty_delta("ephemeral-sync", &u_sub),
            allow_same_zero_tick: false,
        })
        .unwrap();
    assert_eq!(
        out_spec_up,
        ContextCommitOutcome::Committed { delta_seq: 2 }
    );

    // Delete SyncStore source without recreating it -> zero ghost reads!
    store.sync().delete_source("ephemeral-sync").unwrap();
    let vis_del = store.context().visibility("ephemeral-sync").unwrap();
    assert!(!vis_del.instance_matched);
    assert!(!vis_del.is_caught_up);
    assert!(
        store
            .context()
            .load_all_live_units("ephemeral-sync")
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .context()
            .load_index_blob("ephemeral-sync")
            .unwrap()
            .is_none()
    );
    assert!(
        !store
            .context()
            .list_indexed_sources()
            .unwrap()
            .iter()
            .any(|m| m.source_id == "ephemeral-sync")
    );
    let poll_del = store
        .context()
        .poll_consumer_deltas("c1", "ephemeral-sync", None, 10)
        .unwrap();
    assert!(matches!(
        poll_del.status,
        ReplayStatus::FullResyncRequired { .. }
    ));

    // 3. Cross-instance late stale ACK MUST NOT overwrite a checkpoint already on the active instance,
    // and future `acked_delta_seq` > `latest_delta_seq` is clamped safely!
    let ack_clamped = store
        .context()
        .ack_consumer_checkpoint(ConsumerCheckpoint {
            consumer_id: "c-aba".to_owned(),
            source_id: "org:sub".to_owned(),
            instance_id: "manual-sub".to_owned(),
            acked_delta_seq: 999,
            spec_fingerprint: fp.clone(),
            updated_at_ms: 500,
        })
        .unwrap();
    assert_eq!(ack_clamped.acked_delta_seq, 1);

    let late_old_inst = store
        .context()
        .ack_consumer_checkpoint(ConsumerCheckpoint {
            consumer_id: "c-aba".to_owned(),
            source_id: "org:sub".to_owned(),
            instance_id: "stale-old-instance".to_owned(),
            acked_delta_seq: 50,
            spec_fingerprint: fp.clone(),
            updated_at_ms: 501,
        })
        .unwrap();
    assert_eq!(late_old_inst.instance_id, "manual-sub");
    assert_eq!(late_old_inst.acked_delta_seq, 1);

    // Purging a source that a consumer already synced alerts FullResyncRequired on next poll!
    store.context().purge_source("org:sub").unwrap();
    let poll_purged = store
        .context()
        .poll_consumer_deltas("c-aba", "org:sub", Some(&fp), 10)
        .unwrap();
    assert!(matches!(
        poll_purged.status,
        ReplayStatus::FullResyncRequired { .. }
    ));

    // 4. Ring window trimming (> 128 deltas & > 256 retired units) via O(1) sequence addressing
    for tick in 1..=140u64 {
        let ret_batch = vec![
            make_unit(&format!("r_{tick}_a"), "retired a"),
            make_unit(&format!("r_{tick}_b"), "retired b"),
        ];
        store
            .context()
            .commit_delta(ContextCommitRequest {
                source_id: "ring-src",
                instance_id: "manual-ring",
                store_tick_seq: tick,
                spec_stamp: AssetSpecStamp::default(),
                resource_units: &m_sub,
                retired_units: &ret_batch,
                index_blob: None,
                delta: empty_delta("ring-src", &u_sub),
                allow_same_zero_tick: false,
            })
            .unwrap();
    }
    let rmeta = store.context().get_meta("ring-src").unwrap().unwrap();
    assert_eq!(rmeta.latest_delta_seq, 140);
    assert_eq!(
        rmeta.earliest_retained_delta_seq,
        140 - super::MAX_CONTEXT_DELTAS_PER_SOURCE as u64 + 1
    );
    assert_eq!(
        rmeta.retired_seq_end - rmeta.retired_seq_start,
        super::MAX_RETIRED_UNITS_PER_SOURCE as u64
    );
    let listed = store.context().list_deltas("ring-src", None, 200).unwrap();
    assert_eq!(listed.len(), super::MAX_CONTEXT_DELTAS_PER_SOURCE);
    assert_eq!(listed.first().unwrap().delta_seq, 13);
    assert_eq!(listed.last().unwrap().delta_seq, 140);

    // Diagnostic measurement: full `ns::CONTEXT` pread key scan vs O(1)/O(limit) direct sequence lookup
    let iters = 50usize;
    // wall-clock: diagnostic eprintln only; no assertion depends on elapsed time
    let t_scan = std::time::Instant::now();
    let mut scanned = 0usize;
    for _ in 0..iters {
        scanned += store
            .keys(crate::engine::ns::CONTEXT, |_| true)
            .unwrap()
            .len();
    }
    let scan_us = t_scan.elapsed().as_nanos() as f64 / (iters as f64 * 1_000.0);

    // wall-clock: diagnostic eprintln only; no assertion depends on elapsed time
    let t_exact = std::time::Instant::now();
    let mut fetched = 0usize;
    for _ in 0..iters {
        fetched += store
            .context()
            .list_deltas("ring-src", Some(135), 10)
            .unwrap()
            .len();
    }
    let exact_us = t_exact.elapsed().as_nanos() as f64 / (iters as f64 * 1_000.0);
    assert_eq!(fetched, 5 * iters);
    eprintln!(
        "[perf-store] ns::CONTEXT full key scan ({} keys): {:.2} us/op vs O(limit) list_deltas(after=135): {:.2} us/op ({:.1}x faster)",
        scanned / iters,
        scan_us,
        exact_us,
        scan_us / exact_us.max(0.05)
    );
}

#[test]
fn resource_history_replays_as_of_across_304_edits_410_and_aba_purge() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open_dir(tmp.path()).unwrap();
    let ctx = store.context();
    let rk = "https://example.com/pricing".to_owned();

    // t1 = 1000: ingest "100 rpm"
    let mut u_t1 = make_unit("u1", "Tier quota: 100 rpm.");
    u_t1.location.resource_key = rk.clone();
    u_t1.observed_at_ms = 1_000;
    let mut m1 = BTreeMap::new();
    m1.insert(rk.clone(), vec![u_t1.clone()]);
    ctx.commit_delta(ContextCommitRequest {
        source_id: "hist-src",
        instance_id: "inst-a",
        store_tick_seq: 1,
        spec_stamp: AssetSpecStamp::default(),
        resource_units: &m1,
        retired_units: &[],
        index_blob: None,
        delta: empty_delta("hist-src", &u_t1),
        allow_same_zero_tick: false,
    })
    .unwrap();

    // t2 = 2000 & 2500: repeated 304 refreshes advancing `observed_at_ms`
    for (tick, ts) in [(2u64, 2_000u64), (3u64, 2_500u64), (4u64, 2_800u64)] {
        let mut u_304 = u_t1.clone();
        u_304.observed_at_ms = ts;
        let mut m_304 = BTreeMap::new();
        m_304.insert(rk.clone(), vec![u_304]);
        ctx.commit_delta(ContextCommitRequest {
            source_id: "hist-src",
            instance_id: "inst-a",
            store_tick_seq: tick,
            spec_stamp: AssetSpecStamp::default(),
            resource_units: &m_304,
            retired_units: &[],
            index_blob: None,
            delta: empty_delta("hist-src", &u_t1),
            allow_same_zero_tick: false,
        })
        .unwrap();
    }

    // t3 = 3000: rewrite to "500 rpm"
    let mut u_t3 = make_unit("u2", "Tier quota: 500 rpm.");
    u_t3.location.resource_key = rk.clone();
    u_t3.observed_at_ms = 3_000;
    let mut m3 = BTreeMap::new();
    m3.insert(rk.clone(), vec![u_t3.clone()]);
    ctx.commit_delta(ContextCommitRequest {
        source_id: "hist-src",
        instance_id: "inst-a",
        store_tick_seq: 5,
        spec_stamp: AssetSpecStamp::default(),
        resource_units: &m3,
        retired_units: &[],
        index_blob: None,
        delta: empty_delta("hist-src", &u_t3),
        allow_same_zero_tick: false,
    })
    .unwrap();

    // t4 = 4000: HTTP 410 confirmed withdrawal (empty live Vec, Withdrawn at 4000)
    let mut dead = u_t3.clone();
    dead.status = pagesift_graph::context::UnitStatus::Withdrawn {
        at_ms: 4_000,
        reason: "http_410_gone".to_owned(),
    };
    let mut m4 = BTreeMap::new();
    m4.insert(rk.clone(), Vec::new());
    ctx.commit_delta(ContextCommitRequest {
        source_id: "hist-src",
        instance_id: "inst-a",
        store_tick_seq: 6,
        spec_stamp: AssetSpecStamp::default(),
        resource_units: &m4,
        retired_units: &[dead],
        index_blob: None,
        delta: empty_delta("hist-src", &u_t3),
        allow_same_zero_tick: false,
    })
    .unwrap();

    assert!(ctx.load_units_as_of("hist-src", 500).unwrap().1.is_empty());
    let at_1500 = ctx.load_units_as_of("hist-src", 1_500).unwrap().1;
    assert_eq!(at_1500.len(), 1);
    assert_eq!(at_1500[0].text, "Tier quota: 100 rpm.");
    assert_eq!(at_1500[0].observed_at_ms, 1_000);

    let at_3500 = ctx.load_units_as_of("hist-src", 3_500).unwrap().1;
    assert_eq!(at_3500.len(), 1);
    assert_eq!(at_3500[0].text, "Tier quota: 500 rpm.");

    assert!(
        ctx.load_units_as_of("hist-src", 4_500)
            .unwrap()
            .1
            .is_empty()
    );

    // ABA instance replacement purges old `chis` history cleanly!
    let mut u_new = make_unit("u_new", "Reborn instance.");
    u_new.location.resource_key = rk.clone();
    u_new.observed_at_ms = 5_000;
    let mut m_new = BTreeMap::new();
    m_new.insert(rk, vec![u_new.clone()]);
    ctx.commit_delta(ContextCommitRequest {
        source_id: "hist-src",
        instance_id: "inst-b",
        store_tick_seq: 1,
        spec_stamp: AssetSpecStamp::default(),
        resource_units: &m_new,
        retired_units: &[],
        index_blob: None,
        delta: empty_delta("hist-src", &u_new),
        allow_same_zero_tick: false,
    })
    .unwrap();
    assert!(
        ctx.load_units_as_of("hist-src", 1_500)
            .unwrap()
            .1
            .is_empty()
    );
    assert_eq!(ctx.load_units_as_of("hist-src", 5_500).unwrap().1.len(), 1);
}
