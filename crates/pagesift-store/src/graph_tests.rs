use pagesift_graph::{
    ClaimStatus, ClaimTransitionKind, ConflictKind, ConflictRecord, ConflictSeverity,
    DocumentInput, GraphOptions, ObservationOutcome, SourceKind, StreamingGraphBuilder,
    VersionChangeKind, VersionRecord, link_version_pair, normalize_resource_key,
};

use crate::{Config, Store};

fn sample_doc_input(url: &str, price: &str, ts: u64) -> DocumentInput {
    DocumentInput {
        requested_url: url.into(),
        final_url: url.into(),
        canonical_url: None,
        canonical_from_header: false,
        source_kind: SourceKind::Html,
        observed_at_ms: ts,
        status_code: 200,
        engine: "http".into(),
        content_hash: Some(format!("h-{price}")),
        raw_hash: None,
        text_hash: None,
        etag: None,
        last_modified: None,
        title: Some("Widget".into()),
        markdown: format!("# Widget\n\nPrice: {price}"),
        redirect_chain: Vec::new(),
        links: Vec::new(),
        alternates: Vec::new(),
        duplicate_of: None,
        attachments: Vec::new(),
        structured_json_ld: vec![serde_json::json!({
            "@type": "Product",
            "sku": "SKU-1",
            "offers": { "price": price, "priceCurrency": "USD" }
        })],
        extracted_fields: None,
        raw_traces: Vec::new(),
        chunk_spans: Vec::new(),
        previous_version: None,
        version_diff_stats: None,
        changed_block_ids: Vec::new(),
    }
}

#[test]
fn version_chain_alias_bridge_survives_reopen_compact_and_gc() {
    let dir = tempfile::tempdir().unwrap();
    let old_url = "https://example.com/old-price";
    let new_url = "https://example.com/pricing";
    let old_key = normalize_resource_key(old_url);
    let new_key = normalize_resource_key(new_url);

    let v1 = VersionRecord {
        resource_key: old_key.clone(),
        doc_id: "doc-1".into(),
        url: old_url.into(),
        canonical_url: None,
        observed_at_ms: 1_000,
        content_hash: Some("sha1".into()),
        text_hash: None,
        status_code: 200,
        etag: Some("e1".into()),
        last_modified: None,
    };
    {
        let store = Store::open_dir(dir.path()).unwrap();
        store.graph().record_version(&v1, None).unwrap();
        store.flush().unwrap();
    }

    let v2 = VersionRecord {
        resource_key: new_key.clone(),
        doc_id: "doc-2".into(),
        url: new_url.into(),
        canonical_url: Some(new_url.into()),
        observed_at_ms: 2_000,
        content_hash: Some("sha1".into()),
        text_hash: None,
        status_code: 200,
        etag: Some("e2".into()),
        last_modified: None,
    };
    let edge = link_version_pair(&v1, &v2, None, Vec::new());
    assert_eq!(edge.change_kind, VersionChangeKind::UrlMoved);

    {
        let mut cfg = Config::new(dir.path());
        cfg.max_bytes = Some(4_096);
        cfg.low_watermark = 0.25;
        cfg.background = false;
        let store = Store::open(cfg).unwrap();
        store.graph().bind_alias(&old_key, &new_key).unwrap();
        store.graph().record_version(&v2, Some(&edge)).unwrap();

        for i in 0..20 {
            store
                .cache()
                .put(format!("cache-k-{i}"), &[b'x'; 512])
                .unwrap();
        }
        let gc_report = store.gc().unwrap();
        assert!(gc_report.evicted_entries > 0);
        store.compact().unwrap();
        store.flush().unwrap();
    }

    let store = Store::open_dir(dir.path()).unwrap();
    assert_eq!(
        store.graph().resolve_resource_key(old_url).unwrap(),
        new_key
    );
    let latest = store
        .graph()
        .latest_version_for_urls(&[old_url])
        .unwrap()
        .expect("latest version via alias");
    assert_eq!(latest.doc_id, "doc-2");

    for query in [old_url, new_url] {
        let (canon, versions, edges) = store.graph().history(query).unwrap();
        assert_eq!(canon, new_key);
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].doc_id, "doc-1");
        assert_eq!(versions[1].doc_id, "doc-2");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].change_kind, VersionChangeKind::UrlMoved);
    }
}

#[test]
fn conflicts_and_zstd_job_summary_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let mut builder = StreamingGraphBuilder::new(GraphOptions::enabled());
    let d1 = builder.ingest(sample_doc_input("https://example.com/a", "99", 100));
    let d2 = builder.ingest(sample_doc_input("https://example.com/b", "149", 200));
    let summary = builder.finalize();
    assert_eq!(summary.conflicts.len(), 1);

    let extra_conflict = ConflictRecord {
        conflict_id: "c-other".into(),
        subject_key: "org:other.test".into(),
        field_path: "founded".into(),
        normalized_field: "founded".into(),
        kind: ConflictKind::ValueMismatch,
        severity: ConflictSeverity::Low,
        variants: Vec::new(),
        preferred_hint: None,
    };

    {
        let store = Store::open_dir(dir.path()).unwrap();
        let g = store.graph();
        g.put_conflicts(&summary.conflicts).unwrap();
        g.put_conflicts(&[extra_conflict]).unwrap();
        g.append_job_delta("job-1", &d2).unwrap();
        g.append_job_delta("job-1", &d1).unwrap();
        g.put_job_summary("job-1", &summary).unwrap();
        store.flush().unwrap();
    }

    let store = Store::open_dir(dir.path()).unwrap();
    let g = store.graph();
    assert_eq!(g.list_conflicts(None).unwrap().len(), 2);
    let filtered = g
        .list_conflicts(Some(&summary.conflicts[0].subject_key))
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].conflict_id, summary.conflicts[0].conflict_id);

    let loaded = g.get_job_summary("job-1").unwrap().unwrap();
    assert_eq!(loaded, summary);

    let deltas = g.job_deltas("job-1").unwrap();
    assert_eq!(deltas.len(), 2);
    assert_eq!(deltas[0].node.observed_at_ms, 100);
    assert_eq!(deltas[1].node.observed_at_ms, 200);
}

#[test]
fn history_as_of_and_claim_lifecycle_persistence_work() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let g = store.graph();
    let url = "https://example.com/pricing";
    let key = normalize_resource_key(url);

    let mut builder = StreamingGraphBuilder::new(GraphOptions::enabled());
    let d1 = builder.ingest(sample_doc_input(url, "99", 1_000));
    let d2 = builder.ingest(sample_doc_input(url, "89", 2_000));

    let v1 = VersionRecord {
        resource_key: key.clone(),
        doc_id: d1.node.doc_id.clone(),
        url: url.into(),
        canonical_url: None,
        observed_at_ms: 1_000,
        content_hash: d1.node.content_hash.clone(),
        text_hash: None,
        status_code: 200,
        etag: None,
        last_modified: None,
    };
    let v2 = VersionRecord {
        resource_key: key.clone(),
        doc_id: d2.node.doc_id.clone(),
        url: url.into(),
        canonical_url: None,
        observed_at_ms: 2_000,
        content_hash: d2.node.content_hash.clone(),
        text_hash: None,
        status_code: 200,
        etag: None,
        last_modified: None,
    };
    let edge = link_version_pair(&v1, &v2, None, Vec::new());
    g.record_version(&v1, None).unwrap();
    g.record_version(&v2, Some(&edge)).unwrap();

    let h_1500 = g.history_as_of(url, Some(1_500), 10).unwrap();
    assert_eq!(h_1500.versions.len(), 1);
    assert_eq!(h_1500.versions[0].doc_id, v1.doc_id);
    assert!(h_1500.edges.is_empty());

    let (_, trans1) = g
        .reconcile_and_store_resource_claims(
            url,
            &ObservationOutcome::VerifiedContent,
            &d1.claims,
            1_000,
        )
        .unwrap();
    assert!(
        trans1
            .iter()
            .any(|t| t.kind == ClaimTransitionKind::Appeared)
    );

    let (_, trans2) = g
        .reconcile_and_store_resource_claims(
            url,
            &ObservationOutcome::VerifiedContent,
            &d2.claims,
            2_000,
        )
        .unwrap();
    assert!(
        trans2
            .iter()
            .any(|t| t.kind == ClaimTransitionKind::Updated)
    );

    let (_, trans3) = g
        .reconcile_and_store_resource_claims(
            url,
            &ObservationOutcome::TransientFetchError {
                error: "timeout".into(),
            },
            &[],
            3_000,
        )
        .unwrap();
    assert!(
        trans3
            .iter()
            .any(|t| t.kind == ClaimTransitionKind::Unverified)
    );

    let curr_claims = g.claims_for_resource_as_of(url, None).unwrap();
    assert!(
        curr_claims
            .iter()
            .all(|c| matches!(c.status, ClaimStatus::Unverified { .. }))
    );

    let past_claims = g.claims_for_resource_as_of(url, Some(1_500)).unwrap();
    assert!(past_claims.iter().all(|c| c.status == ClaimStatus::Active));
    let price_1500 = past_claims
        .iter()
        .find(|c| c.claim.normalized_field == "price")
        .unwrap();
    assert_eq!(price_1500.claim.normalized_value.raw(), "99");
}
