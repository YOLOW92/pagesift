use super::*;
use crate::ScraperConfig;
use pagesift_graph::{
    DocumentInput, GraphOptions, ObservationOutcome, SourceKind, StreamingGraphBuilder,
    VersionRecord,
};
use serde_json::json;

#[tokio::test]
async fn sync_and_query_graph_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();

    let doc = DocumentInput {
        requested_url: "https://example.com/pricing".into(),
        final_url: "https://example.com/pricing".into(),
        canonical_url: None,
        canonical_from_header: false,
        source_kind: SourceKind::Html,
        observed_at_ms: 1_700_000_000_000,
        status_code: 200,
        engine: "http".into(),
        content_hash: Some("hash-pricing-v1".into()),
        raw_hash: None,
        text_hash: None,
        etag: None,
        last_modified: None,
        title: Some("Pricing".into()),
        markdown: "# Pricing\n\nPro plan costs $20 per month.".into(),
        redirect_chain: Vec::new(),
        links: Vec::new(),
        alternates: Vec::new(),
        duplicate_of: None,
        attachments: Vec::new(),
        structured_json_ld: vec![json!({
            "@id": "https://example.com/sku/pro",
            "name": "Pro",
            "offers": { "price": "20" }
        })],
        extracted_fields: None,
        raw_traces: Vec::new(),
        chunk_spans: Vec::new(),
        previous_version: None,
        version_diff_stats: None,
        changed_block_ids: Vec::new(),
    };
    let mut builder = StreamingGraphBuilder::new(GraphOptions::enabled());
    let delta = builder.ingest(doc);
    let vr = VersionRecord {
        resource_key: delta.node.resource_key.clone(),
        doc_id: delta.node.doc_id.clone(),
        url: delta.node.final_url.clone(),
        canonical_url: delta.node.canonical_url.clone(),
        observed_at_ms: delta.node.observed_at_ms,
        content_hash: delta.node.content_hash.clone(),
        text_hash: delta.node.text_hash.clone(),
        status_code: delta.node.status_code,
        etag: delta.node.etag.clone(),
        last_modified: delta.node.last_modified.clone(),
    };
    let gs = store.graph();
    gs.record_version(&vr, delta.version.as_ref()).unwrap();
    gs.reconcile_and_store_resource_claims(
        &vr.resource_key,
        &ObservationOutcome::VerifiedContent,
        &delta.claims,
        1_700_000_000_000,
    )
    .unwrap();

    let cfg = ScraperConfig {
        store: Some(store),
        ..ScraperConfig::default()
    };
    let scraper = Scraper::new(cfg).unwrap();

    let created = sync(
        &scraper,
        json!({
            "action": "create",
            "source_id": "docs-src",
            "seeds": ["https://example.com/pricing"],
            "max_requests_per_tick": 12,
            "required_fields": ["price"]
        }),
    )
    .await
    .unwrap();
    assert_eq!(created["success"], true);
    assert_eq!(created["source"]["id"], "docs-src");
    assert_eq!(created["source"]["budget"]["maxRequestsPerTick"], 12);

    let listed = sync(&scraper, json!({ "action": "list" })).await.unwrap();
    assert_eq!(listed["total"], 1);

    let state = sync(
        &scraper,
        json!({ "action": "state", "sourceId": "docs-src" }),
    )
    .await
    .unwrap();
    assert_eq!(state["state"]["spec"]["id"], "docs-src");

    let history = query_graph(
        &scraper,
        json!({ "history_url": "https://example.com/pricing" }),
    )
    .await
    .unwrap();
    assert_eq!(history["success"], true);
    assert_eq!(history["versions"].as_array().unwrap().len(), 1);

    let claims = query_graph(
        &scraper,
        json!({ "claims": true, "url": "https://example.com/pricing" }),
    )
    .await
    .unwrap();
    assert_eq!(claims["success"], true);
    assert!(claims["total"].as_u64().unwrap() >= 1);

    let deleted = sync(&scraper, json!({ "action": "delete", "id": "docs-src" }))
        .await
        .unwrap();
    assert_eq!(deleted["deleted"], true);

    let missing = sync(&scraper, json!({ "action": "get", "id": "docs-src" }))
        .await
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn loop_ops_require_persistent_store() {
    let scraper = Scraper::new(ScraperConfig::default()).unwrap();
    let err = sync(&scraper, json!({ "action": "list" }))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidRequest);
    let gerr = query_graph(&scraper, json!({ "conflicts": true }))
        .await
        .unwrap_err();
    assert_eq!(gerr.code, ErrorCode::InvalidRequest);
}
