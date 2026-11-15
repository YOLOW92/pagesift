use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pagesift_graph::{
    ClaimLifecycleRecord, ClaimRecord, ExtractionMethod, RawFieldTrace, SourceKind, VersionRecord,
    align_traces, build_indexed_blocks,
};
use pagesift_store::{
    Config as StoreConfig, Store, SyncDelta, SyncTickStats, VersionCommitRequest,
};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::engine::{BoxFuture, Engine};
use crate::error::{ApiError, ErrorCode};
use crate::server::{Config, router};
use crate::types::{Document, ScrapeOptions};

struct SyncMockEngine {
    store: Store,
}

impl Engine for SyncMockEngine {
    fn scrape<'a>(
        &'a self,
        _url: &'a str,
        _opts: &'a ScrapeOptions,
    ) -> BoxFuture<'a, Result<Document, ApiError>> {
        Box::pin(async {
            Err(ApiError::unsupported(
                "scrape unused in sources route unit test",
            ))
        })
    }

    fn sync_source<'a>(
        &'a self,
        source_id: &'a str,
        tenant: Option<&'a str>,
        _force_all: bool,
        _max_requests: Option<usize>,
    ) -> BoxFuture<'a, Result<SyncDelta, ApiError>> {
        let store = self.store.clone();
        Box::pin(async move {
            let sync = store.sync_for(tenant);
            let Some(_spec) = sync
                .get_source(source_id)
                .map_err(|e| ApiError::new(ErrorCode::Internal, format!("sync mock error: {e}")))?
            else {
                return Err(ApiError::new(
                    ErrorCode::NotFound,
                    format!("data source '{source_id}' not found"),
                ));
            };
            let delta = SyncDelta {
                source_id: source_id.to_string(),
                tick_id: "tick-1000".to_string(),
                tick_seq: 1,
                started_at_ms: 950,
                completed_at_ms: 1000,
                coverage_status: "complete_within_scope".to_string(),
                coverage_reasons: Vec::new(),
                stats: SyncTickStats {
                    discovered_total: 1,
                    candidates_due: 1,
                    requests_executed: 1,
                    substantive_updates: 1,
                    elapsed_ms: 50,
                    ..Default::default()
                },
                added_resources: Vec::new(),
                modified_resources: Vec::new(),
                migrated_resources: Vec::new(),
                disappeared_resources: Vec::new(),
                claim_transitions: Vec::new(),
                conflicts: Vec::new(),
                adapt_decisions: Vec::new(),
            };
            sync.append_delta(&delta).map_err(|e| {
                ApiError::new(ErrorCode::Internal, format!("append mock delta: {e}"))
            })?;
            Ok(delta)
        })
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
    let resp = app
        .clone()
        .oneshot(builder.body(payload).expect("request"))
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let json: Value = serde_json::from_slice(&bytes).expect("json response");
    (status, json)
}

#[tokio::test]
async fn sources_crud_sync_state_and_delta_endpoints_work() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("store");
    let app = router(
        Arc::new(SyncMockEngine {
            store: store.clone(),
        }),
        Config {
            store: Some(store),
            ..Default::default()
        },
    );

    let (status, created) = send_json(
        &app,
        "POST",
        "/v2/sources",
        Some(json!({
            "id": "docs-site",
            "name": "Docs",
            "seeds": ["https://example.com/docs"],
            "includePaths": ["/docs"],
            "budget": { "maxRequestsPerTick": 25 }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create failed: {created}");
    assert_eq!(created["source"]["id"], "docs-site");

    let (status, list) = send_json(&app, "GET", "/v2/sources", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["total"], 1);

    let (status, synced) = send_json(&app, "POST", "/v2/sources/docs-site/sync", None).await;
    assert_eq!(status, StatusCode::OK, "sync failed: {synced}");
    assert_eq!(synced["delta"]["tickId"], "tick-1000");

    let (status, deltas) = send_json(&app, "GET", "/v2/sources/docs-site/deltas", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deltas["total"], 1);

    let (status, delta) =
        send_json(&app, "GET", "/v2/sources/docs-site/deltas/tick-1000", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(delta["delta"]["coverageStatus"], "complete_within_scope");

    let (status, state) =
        send_json(&app, "GET", "/v2/sources/docs-site/state?asOf=1000", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state["state"]["asOfMs"], 1000);
    assert_eq!(state["state"]["totalTicks"], 1);

    let (status, deleted) = send_json(&app, "DELETE", "/v2/sources/docs-site", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["deleted"], true);
}

#[tokio::test]
async fn graph_claims_and_history_as_of_endpoints_filter_correctly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("store");
    let gs = store.graph_for(Some(ScrapeOptions::ANONYMOUS_OWNER));
    let url = "https://example.com/pricing";
    let key = pagesift_graph::normalize_resource_key(url);

    let ver1 = VersionRecord {
        resource_key: key.clone(),
        doc_id: "doc-1".into(),
        url: url.into(),
        canonical_url: None,
        observed_at_ms: 100,
        content_hash: Some("h1".into()),
        text_hash: Some("t1".into()),
        status_code: 200,
        etag: None,
        last_modified: None,
    };
    gs.commit_version(VersionCommitRequest {
        expected_prev_doc_id: None,
        alias_keys: &[],
        record: &ver1,
        edge: None,
        snapshot_bytes: None,
        snapshot_unavailable_reason: None,
    })
    .expect("commit v1");

    let ver2 = VersionRecord {
        resource_key: key.clone(),
        doc_id: "doc-2".into(),
        url: url.into(),
        canonical_url: None,
        observed_at_ms: 500,
        content_hash: Some("h2".into()),
        text_hash: Some("t2".into()),
        status_code: 200,
        etag: None,
        last_modified: None,
    };
    gs.commit_version(VersionCommitRequest {
        expected_prev_doc_id: Some("doc-1"),
        alias_keys: &[],
        record: &ver2,
        edge: None,
        snapshot_bytes: None,
        snapshot_unavailable_reason: None,
    })
    .expect("commit v2");

    let trace = RawFieldTrace {
        field_path: "price".into(),
        field_name: "price".into(),
        value: json!("$20"),
        raw_quote: "$20".into(),
        attr_name: None,
        method: ExtractionMethod::JsonLd {
            entity_type: Some("Product".into()),
            entity_id: None,
        },
        source_url: Some(url.into()),
        visible_in_dom: true,
    };
    let blocks = build_indexed_blocks("doc-1", "# Pro\n\nPrice: $20", &[], 64, 512);
    let evidence = align_traces("doc-1", url, std::slice::from_ref(&trace), &blocks)
        .into_iter()
        .next()
        .expect("evidence");
    let claim = ClaimRecord::from_trace(
        "sku:pro",
        &key,
        "doc-1",
        url,
        SourceKind::Html,
        "http",
        500,
        None,
        &trace,
        evidence,
    );
    let lifecycle = ClaimLifecycleRecord::new_active(claim);
    gs.put_resource_claims(&key, &[lifecycle])
        .expect("put claims");

    let app = router(
        Arc::new(SyncMockEngine {
            store: store.clone(),
        }),
        Config {
            store: Some(store),
            ..Default::default()
        },
    );

    let (status, hist) = send_json(
        &app,
        "GET",
        "/v2/graph/history?url=https://example.com/pricing&asOf=200",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "history failed: {hist}");
    assert_eq!(hist["versions"].as_array().expect("versions").len(), 1);

    let (status, claims) = send_json(
        &app,
        "GET",
        "/v2/graph/claims?subject=sku:pro&asOf=500",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "claims failed: {claims}");
    assert_eq!(claims["total"], 1);
    assert_eq!(claims["claims"][0]["claim"]["subjectKey"], "sku:pro");
}

#[tokio::test]
async fn max_requests_bounds_and_invalid_seed_parity_t08_c21_r02() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let app_cfg = Config {
        store: Some(store.clone()),
        ..Default::default()
    };
    let engine: Arc<dyn Engine> = Arc::new(SyncMockEngine {
        store: store.clone(),
    });
    let app = router(engine, app_cfg);

    // C21: 全部 Seed 均被 excludePaths 排除时，创建数据源必须明确返回 400 Invalid
    let (status, bad_src) = send_json(
        &app,
        "POST",
        "/v2/sources",
        Some(json!({
            "id": "bad-seed",
            "seeds": ["https://example.com/private/secret"],
            "excludePaths": ["/private/"]
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "excluded seed must reject: {bad_src}"
    );

    let (status, created) = send_json(
        &app,
        "POST",
        "/v2/sources",
        Some(json!({
            "id": "src-t08",
            "seeds": ["https://example.com/"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create failed: {created}");

    // T08: maxRequests 边界统一校验（0 -> 400, 10/11/10000 -> 200, 10001 -> 400）
    for (max_req, expected_status) in [
        (0usize, StatusCode::BAD_REQUEST),
        (10, StatusCode::OK),
        (11, StatusCode::OK),
        (pagesift_store::MAX_REQUESTS_PER_TICK_CAP, StatusCode::OK),
        (
            pagesift_store::MAX_REQUESTS_PER_TICK_CAP + 1,
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (st, body) = send_json(
            &app,
            "POST",
            "/v2/sources/src-t08/sync",
            Some(json!({ "maxRequests": max_req })),
        )
        .await;
        assert_eq!(st, expected_status, "maxRequests={max_req} body={body}");
    }

    // R02: 旧修订号不得覆盖新修订号（CAS 冲突拒绝，upsert 单调递增不回退）
    let ss = store.sync_for(Some(crate::types::ScrapeOptions::ANONYMOUS_OWNER));
    let mut spec = ss.get_source("src-t08").unwrap().unwrap();
    spec.name = Some("Updated".into());
    let rev2 = ss.upsert_source(spec).unwrap();
    assert!(rev2.revision >= 2);
    let mut stale = rev2.clone();
    stale.revision = 1;
    let cas = ss.update_source_cas(stale.clone(), Some(1)).unwrap();
    assert!(
        matches!(cas, pagesift_store::SpecCasOutcome::Conflict { ref current } if current.revision == rev2.revision),
        "expected Conflict at rev {}, got {cas:?}",
        rev2.revision
    );
    let rev3 = ss.upsert_source(stale).unwrap();
    assert!(
        rev3.revision > rev2.revision,
        "revision must stay monotonic: {rev3:?}"
    );
}
