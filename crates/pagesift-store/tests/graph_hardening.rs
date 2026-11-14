use pagesift_graph::{
    ConflictKind, ConflictRecord, ConflictSeverity, DocNode, GraphStats, GraphSummary,
    PageGraphDelta, SourceKind, SubjectKey, SubjectKeySource, VersionChangeKind, VersionEdge,
    VersionRecord, link_version_pair,
};
use pagesift_store::{
    Config, Error, LoadedVersionSnapshot, Store, VersionCommitOutcome, VersionCommitRequest,
};

fn vr(url: &str, doc_id: &str, ts: u64, hash: &str) -> VersionRecord {
    VersionRecord {
        resource_key: url.to_string(),
        doc_id: doc_id.to_string(),
        url: url.to_string(),
        canonical_url: None,
        observed_at_ms: ts,
        content_hash: Some(hash.to_string()),
        text_hash: None,
        status_code: 200,
        etag: None,
        last_modified: None,
    }
}

fn sample_conflict(subject: &str, id: &str) -> ConflictRecord {
    ConflictRecord {
        conflict_id: id.to_string(),
        subject_key: subject.to_string(),
        field_path: "/price".to_string(),
        normalized_field: "price".to_string(),
        kind: ConflictKind::ValueMismatch,
        severity: ConflictSeverity::Medium,
        variants: Vec::new(),
        preferred_hint: None,
    }
}

fn sample_delta(url: &str, doc_id: &str, ts: u64) -> PageGraphDelta {
    PageGraphDelta {
        node: DocNode {
            doc_id: doc_id.to_string(),
            resource_key: url.to_string(),
            requested_url: url.to_string(),
            final_url: url.to_string(),
            canonical_url: None,
            source_kind: SourceKind::Html,
            observed_at_ms: ts,
            status_code: 200,
            engine: "http".into(),
            content_hash: Some(doc_id.to_string()),
            raw_hash: None,
            text_hash: None,
            etag: None,
            last_modified: None,
            title: None,
        },
        subject_key: SubjectKey {
            key: format!("resource:{url}"),
            source: SubjectKeySource::ResourceKey,
            raw: url.to_string(),
        },
        blocks: Vec::new(),
        citations: Vec::new(),
        provenance: Vec::new(),
        version: None,
        evidence: Vec::new(),
        claims: Vec::new(),
    }
}

#[test]
fn graph_store_isolates_tenants_and_never_leaks_legacy_unscoped_data() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let url = "https://shared.example/item";

    // Pre-existing unscoped legacy data
    let legacy = store.graph();
    legacy
        .record_version(&vr(url, "legacy_doc", 10, "lh1"), None)
        .unwrap();
    legacy
        .put_conflicts(&[sample_conflict("sku:LEGACY", "cnf_legacy")])
        .unwrap();
    legacy
        .put_job_summary("job-1", &GraphSummary::default())
        .unwrap();
    legacy
        .append_job_delta("job-1", &sample_delta(url, "legacy_doc", 10))
        .unwrap();

    // Anonymous and authenticated tenants must not see legacy unscoped or each other's data
    let anon = store.graph_for(Some("anonymous"));
    let alice = store.graph_for(Some("alice"));
    let bob = store.graph_for(Some("bob"));

    assert!(anon.history(url).unwrap().1.is_empty());
    assert!(anon.list_conflicts(None).unwrap().is_empty());
    assert!(anon.get_job_summary("job-1").unwrap().is_none());
    assert!(anon.job_deltas("job-1").unwrap().is_empty());

    assert!(alice.history(url).unwrap().1.is_empty());
    assert!(alice.list_conflicts(None).unwrap().is_empty());

    alice
        .record_version(&vr(url, "alice_doc", 20, "ah1"), None)
        .unwrap();
    alice
        .put_conflicts(&[sample_conflict("sku:SHARED", "cnf_alice")])
        .unwrap();
    alice
        .put_job_summary("job-1", &GraphSummary::default())
        .unwrap();
    alice
        .append_job_delta("job-1", &sample_delta(url, "alice_doc", 20))
        .unwrap();

    bob.record_version(&vr(url, "bob_doc", 30, "bh1"), None)
        .unwrap();
    bob.put_conflicts(&[sample_conflict("sku:SHARED", "cnf_bob")])
        .unwrap();

    assert_eq!(alice.history(url).unwrap().1.len(), 1);
    assert_eq!(alice.history(url).unwrap().1[0].doc_id, "alice_doc");
    assert_eq!(bob.history(url).unwrap().1.len(), 1);
    assert_eq!(bob.history(url).unwrap().1[0].doc_id, "bob_doc");
    assert_eq!(anon.history(url).unwrap().1.len(), 0);
    assert_eq!(legacy.history(url).unwrap().1[0].doc_id, "legacy_doc");

    let alice_cnfs = alice.list_conflicts(None).unwrap();
    assert_eq!(alice_cnfs.len(), 1);
    assert_eq!(alice_cnfs[0].conflict_id, "cnf_alice");

    // Purge alice's job without disturbing legacy job-1
    assert_eq!(alice.purge_job("job-1").unwrap(), 2);
    assert!(alice.get_job_summary("job-1").unwrap().is_none());
    assert!(alice.job_deltas("job-1").unwrap().is_empty());
    assert!(legacy.get_job_summary("job-1").unwrap().is_some());
    assert_eq!(legacy.job_deltas("job-1").unwrap().len(), 1);
}

#[test]
fn bind_alias_cleans_stale_reverse_links_when_rebound() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let gs = store.graph();
    let alias = "https://example.com/alias";
    let t1 = "https://example.com/t1";
    let t2 = "https://example.com/t2";

    gs.record_version(&vr(t1, "d1", 10, "h1"), None).unwrap();
    gs.record_version(&vr(t2, "d2", 20, "h2"), None).unwrap();
    gs.bind_alias(alias, t1).unwrap();
    assert_eq!(gs.history(t1).unwrap().1.len(), 1);

    gs.bind_alias(alias, t2).unwrap();
    assert_eq!(gs.resolve_resource_key(alias).unwrap(), t2);
    let (_, t1_vers, _) = gs.history(t1).unwrap();
    assert_eq!(t1_vers.len(), 1);
    assert_eq!(t1_vers[0].doc_id, "d1");
}

#[test]
fn commit_version_binds_exact_snapshots_enforces_cas_and_handles_oversize_and_eviction() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(dir.path());
    cfg.snapshot_max_bytes = 64;
    cfg.max_bytes = Some(1);
    cfg.background = false;
    let store = Store::open(cfg).unwrap();
    let gs = store.graph_for(Some("tenant-x"));
    let url = "https://example.com/doc";

    // V1 normal small snapshot
    let v1 = vr(url, "doc_v1", 100, "h1");
    let out1 = gs
        .commit_version(VersionCommitRequest {
            expected_prev_doc_id: None,
            alias_keys: &[],
            record: &v1,
            edge: None,
            snapshot_bytes: Some(b"hello v1"),
            snapshot_unavailable_reason: None,
        })
        .unwrap();
    assert_eq!(
        out1,
        VersionCommitOutcome::Committed {
            snapshot_stored: true,
            snapshot_warning: None,
        }
    );
    assert_eq!(
        gs.load_version_snapshot(&v1).unwrap(),
        LoadedVersionSnapshot::Markdown("hello v1".into())
    );

    // Concurrent writer racing with wrong expected_prev_doc_id must get StaleParent
    let stale_v2 = vr(url, "doc_stale", 150, "hs");
    let stale_out = gs
        .commit_version(VersionCommitRequest {
            expected_prev_doc_id: Some("doc_other"),
            alias_keys: &[],
            record: &stale_v2,
            edge: None,
            snapshot_bytes: Some(b"stale"),
            snapshot_unavailable_reason: None,
        })
        .unwrap();
    assert!(matches!(
        stale_out,
        VersionCommitOutcome::StaleParent { ref current_latest } if current_latest.doc_id == "doc_v1"
    ));

    // V2 oversized snapshot (>64 bytes) commits version record + unavailable snapshot flag + warning
    let v2 = vr(url, "doc_v2", 200, "h2");
    let e2 = link_version_pair(&v1, &v2, None, Vec::new());
    let big = vec![b'x'; 128];
    let out2 = gs
        .commit_version(VersionCommitRequest {
            expected_prev_doc_id: Some("doc_v1"),
            alias_keys: &[],
            record: &v2,
            edge: Some(&e2),
            snapshot_bytes: Some(&big),
            snapshot_unavailable_reason: None,
        })
        .unwrap();
    match out2 {
        VersionCommitOutcome::Committed {
            snapshot_stored,
            snapshot_warning,
        } => {
            assert!(!snapshot_stored);
            assert!(snapshot_warning.unwrap().contains("exceeds limit"));
        }
        other => panic!("unexpected commit outcome: {other:?}"),
    }

    // Loading V2's snapshot must report Unavailable (NOT fall back to V1's "hello v1"!)
    match gs.load_version_snapshot(&v2).unwrap() {
        LoadedVersionSnapshot::Unavailable { reason } => {
            assert!(reason.contains("snapshot_too_large"), "{reason}");
        }
        other => panic!("expected unavailable snapshot for V2, got {other:?}"),
    }

    // V3 small snapshot commits cleanly after V2
    let v3 = vr(url, "doc_v3", 300, "h3");
    let e3 = VersionEdge {
        diff_unavailable_reason: Some("previous_snapshot_too_large".into()),
        ..link_version_pair(&v2, &v3, None, Vec::new())
    };
    let out3 = gs
        .commit_version(VersionCommitRequest {
            expected_prev_doc_id: Some("doc_v2"),
            alias_keys: &[],
            record: &v3,
            edge: Some(&e3),
            snapshot_bytes: Some(b"hello v3"),
            snapshot_unavailable_reason: None,
        })
        .unwrap();
    assert_eq!(
        out3,
        VersionCommitOutcome::Committed {
            snapshot_stored: true,
            snapshot_warning: None,
        }
    );

    // Force-evict snapshot blob via Store::gc to verify evicted snapshot reports previous_snapshot_evicted
    let report = store.gc().unwrap();
    assert!(report.evicted_entries >= 1);
    assert_eq!(
        gs.load_version_snapshot(&v3).unwrap(),
        LoadedVersionSnapshot::Unavailable {
            reason: "previous_snapshot_evicted".into(),
        }
    );
    let (_, vers, edges) = gs.history(url).unwrap();
    assert_eq!(vers.len(), 3);
    assert_eq!(edges.len(), 2);
    assert_eq!(edges[1].change_kind, VersionChangeKind::Substantive);
    assert_eq!(
        edges[1].diff_unavailable_reason.as_deref(),
        Some("previous_snapshot_too_large")
    );
}

#[test]
fn bounded_queries_and_write_read_capacity_symmetry_hold() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let gs = store.graph();
    let url = "https://example.com/hist";

    for i in 0..10u64 {
        let rec = vr(url, &format!("d{i}"), 100 + i, &format!("h{i}"));
        gs.record_version(&rec, None).unwrap();
    }
    let page = gs.history_bounded(url, 3).unwrap();
    assert!(page.truncated);
    assert_eq!(page.versions.len(), 3);
    assert_eq!(
        page.versions
            .iter()
            .map(|v| v.observed_at_ms)
            .collect::<Vec<_>>(),
        vec![107, 108, 109]
    );

    let cnfs: Vec<_> = (0..5)
        .map(|i| sample_conflict("sku:X", &format!("c{i}")))
        .collect();
    gs.put_conflicts(&cnfs).unwrap();
    let (p1, total, more) = gs.list_conflicts_bounded(None, 2, 1).unwrap();
    assert_eq!(total, 5);
    assert!(more);
    assert_eq!(p1.len(), 2);

    // Oversized JSON > 64 MiB must be rejected symmetrically at write time before compression
    let huge = GraphSummary {
        stats: GraphStats::default(),
        nodes: vec![DocNode {
            doc_id: "big".into(),
            resource_key: url.into(),
            requested_url: url.into(),
            final_url: url.into(),
            canonical_url: None,
            source_kind: SourceKind::Html,
            observed_at_ms: 1,
            status_code: 200,
            engine: "http".into(),
            content_hash: None,
            raw_hash: None,
            text_hash: None,
            etag: None,
            last_modified: None,
            title: Some("A".repeat((64 << 20) + 1024)),
        }],
        ..GraphSummary::default()
    };
    assert!(matches!(
        gs.put_job_summary("big-job", &huge),
        Err(Error::ValueTooLarge { .. })
    ));
}

#[test]
fn record_version_is_idempotent_after_commit_version_and_replays() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let gs = store.graph_for(Some("acct:idemp"));
    let url = "https://example.com/page";

    let v1 = vr(url, "doc_c1", 100, "h1");
    gs.commit_version(VersionCommitRequest {
        expected_prev_doc_id: None,
        alias_keys: &[],
        record: &v1,
        edge: None,
        snapshot_bytes: Some(b"hello"),
        snapshot_unavailable_reason: None,
    })
    .unwrap();
    // Replay via record_version (e.g. job sink persisting a locally committed delta) must be a no-op
    gs.record_version(&v1, None).unwrap();

    // Remote worker delta committed solely via record_version (e.g. from gateway replay) must deduplicate too
    let v2 = vr(url, "doc_r2", 200, "h2");
    let e2 = link_version_pair(&v1, &v2, None, Vec::new());
    gs.record_version(&v2, Some(&e2)).unwrap();
    gs.record_version(&v2, Some(&e2)).unwrap();

    let (_, versions, edges) = gs.history(url).unwrap();
    assert_eq!(
        versions.len(),
        2,
        "versions must not duplicate: {versions:?}"
    );
    assert_eq!(edges.len(), 1, "edges must not duplicate: {edges:?}");
}
