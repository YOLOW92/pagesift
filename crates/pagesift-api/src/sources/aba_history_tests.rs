use std::collections::BTreeSet;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pagesift_graph::{
    ClaimLifecycleRecord, ClaimRecord, ClaimStatus, ClaimTimelineEntry, ExtractionMethod,
    RawFieldTrace, SourceKind, align_traces, build_indexed_blocks, detect_conflicts,
};
use pagesift_store::{
    Config as StoreConfig, Store, SyncDelta, SyncTickStats, TickCommitOutcome, TickCommitRequest,
};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::engine::{BoxFuture, Engine};
use crate::error::{ApiError, ErrorCode};
use crate::server::{Config, router};
use crate::types::{Document, ScrapeOptions};

struct StubEngine;

impl Engine for StubEngine {
    fn scrape<'a>(
        &'a self,
        _url: &'a str,
        _opts: &'a ScrapeOptions,
    ) -> BoxFuture<'a, Result<Document, ApiError>> {
        Box::pin(async { Err(ApiError::new(ErrorCode::Internal, "unused")) })
    }
}

async fn send_json(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let payload = match body {
        Some(v) => {
            builder = builder.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let req = builder.body(payload).expect("build request");
    let resp = app.clone().oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let val = serde_json::from_slice(&bytes).expect("json body");
    (status, val)
}

#[tokio::test]
async fn p0_02_p1_05_p1_06_http_aba_freshness_and_history_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let app = router(
        Arc::new(StubEngine),
        Config {
            store: Some(store.clone()),
            ..Default::default()
        },
    );

    // P1-05: minIntervalSecs > 7 days (864000s) succeeds and persists without error or panic.
    let (st, created_long) = send_json(
        &app,
        "POST",
        "/v2/sources",
        Some(json!({
            "id": "long-freshness",
            "seeds": ["https://example.com/"],
            "freshness": {
                "minIntervalSecs": 864000,
                "defaultIntervalSecs": 1296000,
                "maxIntervalSecs": 2592000
            }
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "long freshness create: {created_long}");
    assert_eq!(
        created_long["source"]["freshness"]["minIntervalSecs"],
        864000
    );

    // P0-02 + P1-06: create A -> delete A -> recreate B with same id -> reject A's tick.
    let (st, created_a) = send_json(
        &app,
        "POST",
        "/v2/sources",
        Some(json!({
            "id": "aba-docs",
            "seeds": ["https://example.com/docs"]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create A: {created_a}");
    let inst_a = created_a["source"]["instanceId"]
        .as_str()
        .expect("instanceId")
        .to_string();
    assert!(!inst_a.is_empty());

    let (st, _) = send_json(&app, "DELETE", "/v2/sources/aba-docs", None).await;
    assert_eq!(st, StatusCode::OK);

    // Missing source delta returns 404 data source not found.
    let (st, miss) = send_json(&app, "GET", "/v2/sources/aba-docs/deltas/t-1", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "missing source: {miss}");

    let (st, created_b) = send_json(
        &app,
        "POST",
        "/v2/sources",
        Some(json!({
            "id": "aba-docs",
            "seeds": ["https://example.com/docs"]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create B: {created_b}");
    let inst_b = created_b["source"]["instanceId"]
        .as_str()
        .expect("instanceId B")
        .to_string();
    assert_ne!(inst_a, inst_b);
    let created_b_ms = created_b["source"]["createdAtMs"]
        .as_u64()
        .expect("createdAtMs B");

    // Stale tick from A cannot commit onto B despite matching source_id + revision=1.
    let ss = store.sync_for(Some(ScrapeOptions::ANONYMOUS_OWNER));
    let mut inv = Vec::new();
    let mut stale_delta = SyncDelta {
        source_id: "aba-docs".into(),
        instance_id: inst_a.clone(),
        tick_id: "tick-stale-a".into(),
        tick_seq: 1,
        started_at_ms: created_b_ms,
        completed_at_ms: created_b_ms + 10,
        coverage_status: "complete_within_scope".into(),
        coverage_reasons: Vec::new(),
        stats: SyncTickStats::default(),
        added_resources: Vec::new(),
        modified_resources: Vec::new(),
        migrated_resources: Vec::new(),
        disappeared_resources: Vec::new(),
        claim_transitions: Vec::new(),
        conflicts: Vec::new(),
        adapt_decisions: Vec::new(),
    };
    let subjects = BTreeSet::new();
    let outcome = ss
        .commit_tick_atomic(TickCommitRequest {
            source_id: "aba-docs",
            expected_instance_id: &inst_a,
            expected_spec_revision: 1,
            spec_revision_used: 1,
            inventory: &mut inv,
            experience_bytes: b"{}",
            delta: &mut stale_delta,
            graph_bundles: Vec::new(),
            conflict_subjects: &subjects,
            conflicts: &[],
        })
        .expect("commit stale check");
    assert_eq!(outcome, TickCommitOutcome::SourceDeleted);

    let (st, state_b) = send_json(&app, "GET", "/v2/sources/aba-docs/state", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(state_b["state"]["totalTicks"], 0);
    assert_eq!(state_b["state"]["spec"]["instanceId"], inst_b);

    // P1-06: querying before B's creation returns not_created_yet + null spec.
    if created_b_ms > 0 {
        let before = created_b_ms - 1;
        let (st, pre) = send_json(
            &app,
            "GET",
            &format!("/v2/sources/aba-docs/state?asOf={before}"),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(pre["state"]["historyStatus"], "not_created_yet");
        assert_eq!(pre["state"]["spec"], Value::Null);
    }
}

#[tokio::test]
async fn p1_04_http_claims_and_conflicts_expose_history_unavailable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let gs = store.graph_for(Some(ScrapeOptions::ANONYMOUS_OWNER));
    let url = "https://example.com/item";
    let key = pagesift_graph::normalize_resource_key(url);

    let trace = RawFieldTrace {
        field_path: "price".into(),
        field_name: "price".into(),
        value: json!("$10"),
        raw_quote: "$10".into(),
        attr_name: None,
        method: ExtractionMethod::JsonLd {
            entity_type: Some("Product".into()),
            entity_id: None,
        },
        source_url: Some(url.into()),
        visible_in_dom: true,
    };
    let blocks = build_indexed_blocks("doc-1", "Price: $10", &[], 64, 512);
    let ev = align_traces("doc-1", url, std::slice::from_ref(&trace), &blocks)
        .into_iter()
        .next()
        .expect("evidence");
    let claim = ClaimRecord::from_trace(
        "sku:item",
        &key,
        "doc-1",
        url,
        SourceKind::Html,
        "http",
        100,
        None,
        &trace,
        ev.clone(),
    );
    // Simulate a truncated/legacy coalesced interval [100, 300] with missing intermediate snapshots.
    let mut lifecycle = ClaimLifecycleRecord::new_active(claim.clone());
    lifecycle.first_observed_at_ms = 100;
    lifecycle.last_confirmed_at_ms = 300;
    lifecycle.observation_count = 3;
    lifecycle.timeline = vec![ClaimTimelineEntry {
        since_ms: 100,
        until_ms: 300,
        status: ClaimStatus::Active,
        claim: claim.clone(),
        last_confirmed_at_ms: 300,
        observation_count: 3,
        confirmations: Vec::new(),
    }];
    gs.put_resource_claims(&key, &[lifecycle])
        .expect("put legacy coalesced claim");
    let url2 = "https://other.example/item";
    let key2 = pagesift_graph::normalize_resource_key(url2);
    let trace2 = RawFieldTrace {
        value: json!("$20"),
        raw_quote: "$20".into(),
        source_url: Some(url2.into()),
        ..trace.clone()
    };
    let blocks2 = build_indexed_blocks("doc-2", "Price: $20", &[], 64, 512);
    let ev2 = align_traces("doc-2", url2, std::slice::from_ref(&trace2), &blocks2)
        .into_iter()
        .next()
        .expect("evidence 2");
    let claim2 = ClaimRecord::from_trace(
        "sku:item",
        &key2,
        "doc-2",
        url2,
        SourceKind::Html,
        "http",
        150,
        None,
        &trace2,
        ev2,
    );
    let conflicts = detect_conflicts(&[claim, claim2]);
    gs.put_conflicts(&conflicts).expect("put conflicts");

    let app = router(
        Arc::new(StubEngine),
        Config {
            store: Some(store),
            ..Default::default()
        },
    );

    let (st, claims_mid) = send_json(
        &app,
        "GET",
        "/v2/graph/claims?subject=sku:item&asOf=200",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "claims mid: {claims_mid}");
    assert_eq!(claims_mid["historyUnavailable"], true);

    let (st, conflicts_mid) = send_json(
        &app,
        "GET",
        "/v2/graph/conflicts?subject=sku:item&asOf=200",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "conflicts mid: {conflicts_mid}");
    assert_eq!(conflicts_mid["historyUnavailable"], true);
}
