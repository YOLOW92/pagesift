//! 1.6.1 remediation integration tests covering the review report regression matrix
//! (`P1-01`..`P1-05`, `P2-01`..`P2-07`) and ITNA multi-key account/cookie isolation (`ITNA-02`).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{Html, Redirect};
use axum::routing::get;
use http_body_util::BodyExt;
use pagesift::{Scraper, ScraperConfig};
use pagesift_api::{Engine, KeyLimits};
use pagesift_graph::ObservationOutcome;
use pagesift_graph::context::{AssetSpecStamp, ContextDelta, MaintenanceMode};
use pagesift_store::{ContextCommitRequest, MAX_CONTEXT_DELTAS_PER_SOURCE, Store};
use serde_json::{Value, json};
use tower::ServiceExt;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn build_app(
    dir: &std::path::Path,
    api_keys: Vec<String>,
    key_limits: HashMap<String, KeyLimits>,
    itna: bool,
) -> (Router, Arc<Scraper>, Store) {
    let mut sc_cfg = pagesift_store::Config::new(dir);
    sc_cfg.background = false;
    let store = Store::open(sc_cfg).unwrap();

    let mut cfg = ScraperConfig {
        itna,
        store: Some(store.clone()),
        ..Default::default()
    };
    cfg.fetcher.env_proxy = false;
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    cfg.policy.rate.per_host_concurrency = 8;

    let scraper = Arc::new(Scraper::new(cfg).unwrap());
    let router = pagesift::api::router(
        scraper.clone(),
        pagesift::api::Config {
            api_keys,
            key_limits,
            store: Some(store.clone()),
            ..Default::default()
        },
    );
    (router, scraper, store)
}

async fn send_req(
    app: &Router,
    method: &str,
    uri: &str,
    bearer: Option<&str>,
    extra_headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(key) = bearer {
        builder = builder.header("authorization", format!("Bearer {key}"));
    }
    for (k, v) in extra_headers {
        builder = builder.header(*k, *v);
    }
    let req = if let Some(b) = body {
        builder
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap()
    } else {
        builder.body(Body::empty()).unwrap()
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let val = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({ "raw": String::from_utf8_lossy(&bytes) }));
    (status, val)
}

#[test]
fn p1_01_context_action_and_source_sync_enforce_pages_per_day_and_refund() {
    let rt = runtime();
    rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let mut limits = HashMap::new();
        limits.insert(
            "k-seq".into(),
            KeyLimits {
                name: Some("quota-seq".into()),
                account: None,
                requests_per_minute: None,
                max_concurrent_jobs: None,
                pages_per_day: Some(1),
            },
        );
        limits.insert(
            "k-conc".into(),
            KeyLimits {
                name: Some("quota-conc".into()),
                account: None,
                requests_per_minute: None,
                max_concurrent_jobs: None,
                pages_per_day: Some(1),
            },
        );
        limits.insert(
            "k-sync".into(),
            KeyLimits {
                name: Some("quota-sync".into()),
                account: None,
                requests_per_minute: None,
                max_concurrent_jobs: None,
                pages_per_day: Some(1),
            },
        );
        let (app, _scraper, _store) = build_app(dir.path(), vec![], limits, false);

        let seq_hits = Arc::new(AtomicUsize::new(0));
        let conc_hits = Arc::new(AtomicUsize::new(0));
        let sync_hits = Arc::new(AtomicUsize::new(0));

        let s1 = seq_hits.clone();
        let c1 = conc_hits.clone();
        let y1 = sync_hits.clone();
        let srv = Router::new()
            .route(
                "/seq/{id}",
                get(move || {
                    let s1 = s1.clone();
                    async move {
                        s1.fetch_add(1, Ordering::SeqCst);
                        Html(
                            "<html><body><article><h1>Sequential Page</h1><p>Alpha baseline paragraph without matching target term.</p></article></body></html>",
                        )
                    }
                }),
            )
            .route(
                "/conc/{id}",
                get(move || {
                    let c1 = c1.clone();
                    async move {
                        tokio::time::sleep(Duration::from_millis(60)).await;
                        c1.fetch_add(1, Ordering::SeqCst);
                        Html(
                            "<html><body><article><h1>Concurrent Page</h1><p>Delayed page content for atomic quota race verification.</p></article></body></html>",
                        )
                    }
                }),
            )
            .route(
                "/sync/{id}",
                get(move || {
                    let y1 = y1.clone();
                    async move {
                        y1.fetch_add(1, Ordering::SeqCst);
                        Html(
                            "<html><body><article><h1>Loop Sync Page</h1><p>Loop sync body for pages_per_day verification.</p></article></body></html>",
                        )
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, srv).await.unwrap() });

        // Reserve an ephemeral closed port so the scrape fails and must refund the held page.
        let closed_port = {
            let tmp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tmp.local_addr().unwrap().port()
        };
        let (st_fail, _) = send_req(
            &app,
            "POST",
            "/v2/context/action",
            Some("k-seq"),
            &[],
            Some(json!({
                "mode": "research",
                "query": "unreachable probe term",
                "sourceIds": ["quota-src"],
                "candidateUrls": [format!("http://127.0.0.1:{closed_port}/dead")],
                "allowNetworkAcquisition": true,
                "maxAcquisitionRequests": 2
            })),
        )
        .await;
        assert_eq!(st_fail, StatusCode::OK);

        // Failed scrape above must have refunded quota; now 1 of 2 candidates succeeds and 2nd stops on quota.
        let (st_seq, body_seq) = send_req(
            &app,
            "POST",
            "/v2/context/action",
            Some("k-seq"),
            &[],
            Some(json!({
                "mode": "verify",
                "query": "nonexistent_term_forcing_two_sources",
                "minIndependentSources": 2,
                "sourceIds": ["quota-src"],
                "candidateUrls": [
                    format!("http://{addr}/seq/1"),
                    format!("http://{addr}/seq/2")
                ],
                "allowNetworkAcquisition": true,
                "maxAcquisitionRequests": 5
            })),
        )
        .await;
        assert_eq!(st_seq, StatusCode::OK, "seq action response: {body_seq}");
        assert_eq!(seq_hits.load(Ordering::SeqCst), 1);
        assert_eq!(body_seq["outcome"]["stoppingReason"], "budgetExhausted");
        assert!(
            body_seq["outcome"]["gaps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|g| g["kind"] == "daily_page_quota_exceeded"),
            "expected daily_page_quota_exceeded gap, got: {body_seq}"
        );

        // Subsequent action targeting existing context source returns the reused pack with
        // BudgetExhausted + daily_page_quota_exceeded and issues 0 additional HTTP requests.
        let (st_seq2, body_seq2) = send_req(
            &app,
            "POST",
            "/v2/context/action",
            Some("k-seq"),
            &[],
            Some(json!({
                "mode": "research",
                "query": "another_unmatched_term",
                "sourceIds": ["quota-src"],
                "candidateUrls": [format!("http://{addr}/seq/3")],
                "allowNetworkAcquisition": true,
                "maxAcquisitionRequests": 2
            })),
        )
        .await;
        assert_eq!(st_seq2, StatusCode::OK, "seq2 response: {body_seq2}");
        assert_eq!(body_seq2["outcome"]["reusedExistingContext"], true);
        assert_eq!(body_seq2["outcome"]["stoppingReason"], "budgetExhausted");
        assert!(
            body_seq2["outcome"]["gaps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|g| g["kind"] == "daily_page_quota_exceeded"),
            "expected daily_page_quota_exceeded gap in seq2: {body_seq2}"
        );
        assert_eq!(seq_hits.load(Ordering::SeqCst), 1);

        // Subsequent action with no reusable local context fails immediately with 429 QUOTA_EXCEEDED.
        let (st_seq3, body_seq3) = send_req(
            &app,
            "POST",
            "/v2/context/action",
            Some("k-seq"),
            &[],
            Some(json!({
                "mode": "research",
                "query": "another_unmatched_term",
                "sourceIds": ["empty-quota-src"],
                "candidateUrls": [format!("http://{addr}/seq/4")],
                "allowNetworkAcquisition": true,
                "maxAcquisitionRequests": 2
            })),
        )
        .await;
        assert_eq!(st_seq3, StatusCode::TOO_MANY_REQUESTS, "seq3 response: {body_seq3}");
        assert_eq!(seq_hits.load(Ordering::SeqCst), 1);

        // Concurrent Context Actions for k-conc (pages_per_day = 1) cannot exceed 1 real HTTP fetch.
        let futs = (0..4).map(|i| {
            let app = app.clone();
            let u = format!("http://{addr}/conc/{i}");
            async move {
                send_req(
                    &app,
                    "POST",
                    "/v2/context/action",
                    Some("k-conc"),
                    &[],
                    Some(json!({
                        "mode": "research",
                        "query": format!("unique_conc_term_{i}"),
                        "sourceIds": [format!("conc-src-{i}")],
                        "candidateUrls": [u],
                        "allowNetworkAcquisition": true,
                        "maxAcquisitionRequests": 1
                    })),
                )
                .await
            }
        });
        let results = futures_util::future::join_all(futs).await;
        let ok_count = results.iter().filter(|(st, _)| *st == StatusCode::OK).count();
        let quota_count = results
            .iter()
            .filter(|(st, _)| *st == StatusCode::TOO_MANY_REQUESTS)
            .count();
        assert_eq!(ok_count, 1, "exactly one concurrent action should succeed: {results:?}");
        assert_eq!(quota_count, 3, "other three concurrent actions should get 429: {results:?}");
        assert_eq!(
            conc_hits.load(Ordering::SeqCst),
            1,
            "concurrent actions must reserve pages atomically and issue at most 1 HTTP fetch"
        );

        // /v2/sources/{id}/sync must also honor pages_per_day = 1.
        let (st_create, create_body) = send_req(
            &app,
            "POST",
            "/v2/sources",
            Some("k-sync"),
            &[],
            Some(json!({
                "id": "sync-quota",
                "seeds": [
                    format!("http://{addr}/sync/1"),
                    format!("http://{addr}/sync/2")
                ]
            })),
        )
        .await;
        assert_eq!(st_create, StatusCode::OK, "create source: {create_body}");

        let (st_sync1, sync_body1) = send_req(
            &app,
            "POST",
            "/v2/sources/sync-quota/sync",
            Some("k-sync"),
            &[],
            Some(json!({ "forceAll": true })),
        )
        .await;
        assert_eq!(st_sync1, StatusCode::OK, "sync1: {sync_body1}");
        assert_eq!(sync_hits.load(Ordering::SeqCst), 1);
        assert!(
            sync_body1["delta"]["coverageReasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "daily_page_quota_exceeded"),
            "sync1 delta should record daily_page_quota_exceeded: {sync_body1}"
        );

        // Second sync when daily quota is already 0 returns 429 QUOTA_EXCEEDED and fetches nothing.
        let (st_sync2, sync_body2) = send_req(
            &app,
            "POST",
            "/v2/sources/sync-quota/sync",
            Some("k-sync"),
            &[],
            Some(json!({ "forceAll": true })),
        )
        .await;
        assert_eq!(st_sync2, StatusCode::TOO_MANY_REQUESTS, "sync2: {sync_body2}");
        assert_eq!(sync_hits.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn p1_02_action_direct_scrape_writes_tenant_graph_only() {
    let rt = runtime();
    rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let (app, _scraper, store) =
            build_app(dir.path(), vec!["k-alpha".into(), "k-beta".into()], HashMap::new(), false);

        let rev = Arc::new(AtomicUsize::new(1));
        let rev_c = rev.clone();
        let srv = Router::new().route(
            "/article",
            get(move || {
                let rev_c = rev_c.clone();
                async move {
                    let v = rev_c.load(Ordering::SeqCst);
                    Html(format!(
                        "<html><body><article><h1>Quantum Widget Rev {v}</h1><p>Quantum widget specification release {v} has verified throughput of 420 units.</p></article></body></html>"
                    ))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, srv).await.unwrap() });
        let url = format!("http://{addr}/article");

        let (st, body) = send_req(
            &app,
            "POST",
            "/v2/context/action",
            Some("k-alpha"),
            &[],
            Some(json!({
                "mode": "research",
                "query": "quantum widget throughput",
                "sourceIds": ["alpha-src"],
                "candidateUrls": [url],
                "allowNetworkAcquisition": true,
                "maxAcquisitionRequests": 1
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "action response: {body}");

        // Local/unauthenticated Graph namespace (None) MUST NOT be polluted.
        let local_hist = store.graph_for(None).history_bounded(&url, 10).unwrap();
        assert!(
            local_hist.versions.is_empty(),
            "authenticated Action direct scrape leaked into global None Graph: {:?}",
            local_hist.versions
        );

        let encoded: String = url::form_urlencoded::byte_serialize(url.as_bytes()).collect();
        let hist_uri = format!("/v2/graph/history?url={encoded}");
        let (st_a, hist_a) = send_req(&app, "GET", &hist_uri, Some("k-alpha"), &[], None).await;
        assert_eq!(st_a, StatusCode::OK, "alpha graph history: {hist_a}");
        assert!(
            hist_a
                .get("versions")
                .and_then(Value::as_array)
                .is_some_and(|v| !v.is_empty()),
            "alpha should see its own Graph version: {hist_a}"
        );

        let (st_b, hist_b) = send_req(&app, "GET", &hist_uri, Some("k-beta"), &[], None).await;
        assert_eq!(st_b, StatusCode::OK, "beta graph history: {hist_b}");
        assert!(
            hist_b
                .get("versions")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty),
            "beta must not see alpha's Graph history: {hist_b}"
        );

        // Cache isolation: rev bumps to 2, beta with large maxAge must NOT receive alpha's Rev 1 cache.
        rev.store(2, Ordering::SeqCst);
        let (st_s, scrape_b) = send_req(
            &app,
            "POST",
            "/v2/scrape",
            Some("k-beta"),
            &[],
            Some(json!({ "url": url, "maxAge": 600000 })),
        )
        .await;
        assert_eq!(st_s, StatusCode::OK, "beta scrape: {scrape_b}");
        let md = scrape_b["data"]["markdown"].as_str().unwrap_or("");
        assert!(
            md.contains("Rev 2"),
            "beta received stale/leaked cached page from alpha: {md}"
        );
    });
}

#[test]
fn p1_03_action_allowed_hosts_blocks_loop_inventory_and_redirects() {
    let rt = runtime();
    rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let (app, _scraper, _store) =
            build_app(dir.path(), vec!["k-hosts".into()], HashMap::new(), false);

        let disallowed_hits = Arc::new(AtomicUsize::new(0));
        let redir_hits = Arc::new(AtomicUsize::new(0));
        let d_c = disallowed_hits.clone();
        let r_c = redir_hits.clone();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let port = addr.port();
        let srv = Router::new()
            .route(
                "/secret",
                get(move || {
                    let d_c = d_c.clone();
                    async move {
                        d_c.fetch_add(1, Ordering::SeqCst);
                        Html("<html><body><article><h1>Disallowed Host</h1><p>Secret payload.</p></article></body></html>")
                    }
                }),
            )
            .route(
                "/redir",
                get(move || {
                    let r_c = r_c.clone();
                    async move {
                        r_c.fetch_add(1, Ordering::SeqCst);
                        Redirect::temporary(&format!("http://127.0.0.1:{port}/secret"))
                    }
                }),
            );
        tokio::spawn(async move { axum::serve(listener, srv).await.unwrap() });

        // Case A: Existing Loop source has inventory on 127.0.0.1, Action restricts allowedHosts to ["localhost"].
        let (st_src, _) = send_req(
            &app,
            "POST",
            "/v2/sources",
            Some("k-hosts"),
            &[],
            Some(json!({
                "id": "loop-disallowed",
                "seeds": [format!("http://127.0.0.1:{port}/secret")]
            })),
        )
        .await;
        assert_eq!(st_src, StatusCode::OK);

        let (st_a1, body_a1) = send_req(
            &app,
            "POST",
            "/v2/context/action",
            Some("k-hosts"),
            &[],
            Some(json!({
                "mode": "research",
                "query": "secret payload",
                "sourceIds": ["loop-disallowed"],
                "allowedHosts": ["localhost"],
                "allowNetworkAcquisition": true,
                "maxAcquisitionRequests": 3
            })),
        )
        .await;
        assert_eq!(st_a1, StatusCode::OK, "loop allowedHosts action: {body_a1}");
        assert_eq!(
            disallowed_hits.load(Ordering::SeqCst),
            0,
            "Loop source sync under Action must not fetch hosts outside allowedHosts"
        );

        // Case B: Direct candidate URL on allowed host `localhost` redirects 307 -> disallowed `127.0.0.1`.
        let (st_a2, body_a2) = send_req(
            &app,
            "POST",
            "/v2/context/action",
            Some("k-hosts"),
            &[],
            Some(json!({
                "mode": "research",
                "query": "secret payload",
                "sourceIds": ["redir-src"],
                "candidateUrls": [format!("http://localhost:{port}/redir")],
                "allowedHosts": ["localhost"],
                "allowNetworkAcquisition": true,
                "maxAcquisitionRequests": 2
            })),
        )
        .await;
        assert_eq!(st_a2, StatusCode::OK, "redirect action: {body_a2}");
        assert!(redir_hits.load(Ordering::SeqCst) >= 1);
        assert_eq!(
            disallowed_hits.load(Ordering::SeqCst),
            0,
            "HTTP redirect from allowed host to disallowed host must be blocked"
        );
        assert_eq!(body_a2["outcome"]["stoppingReason"], "permissionRestricted");
    });
}

#[test]
fn p1_04_and_itna_02_key_name_is_display_only_explicit_account_shares_all() {
    let rt = runtime();
    rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let mut limits = HashMap::new();
        limits.insert(
            "k-same-name-1".into(),
            KeyLimits {
                name: Some("team-display".into()),
                account: None,
                requests_per_minute: None,
                max_concurrent_jobs: None,
                pages_per_day: Some(3),
            },
        );
        limits.insert(
            "k-same-name-2".into(),
            KeyLimits {
                name: Some("team-display".into()),
                account: None,
                requests_per_minute: None,
                max_concurrent_jobs: None,
                pages_per_day: Some(3),
            },
        );
        limits.insert(
            "k-shared-acct-1".into(),
            KeyLimits {
                name: Some("svc-east".into()),
                account: Some("acct-team-shared".into()),
                requests_per_minute: None,
                max_concurrent_jobs: None,
                pages_per_day: Some(3),
            },
        );
        limits.insert(
            "k-shared-acct-2".into(),
            KeyLimits {
                name: Some("svc-west".into()),
                account: Some("acct-team-shared".into()),
                requests_per_minute: None,
                max_concurrent_jobs: None,
                pages_per_day: Some(3),
            },
        );
        let (app, _scraper, _store) = build_app(dir.path(), vec![], limits, true);

        // 1. Context isolation: same display `name` stays isolated; same explicit `account` shares.
        let (st, _) = send_req(
            &app,
            "POST",
            "/v2/context/ingest",
            Some("k-same-name-1"),
            &[],
            Some(json!({
                "sourceId": "shared-source",
                "documents": [{
                    "url": "https://example.test/a1",
                    "markdown": "# Secret A1\nKey one private memorandum."
                }]
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        let (st, sr_a2) = send_req(
            &app,
            "POST",
            "/v2/context/search",
            Some("k-same-name-2"),
            &[],
            Some(json!({ "sourceIds": ["shared-source"], "query": "private memorandum" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert!(
            sr_a2["report"]["hits"].as_array().unwrap().is_empty(),
            "different keys sharing only display name must NOT share Context: {sr_a2}"
        );

        let (st, _) = send_req(
            &app,
            "POST",
            "/v2/context/ingest",
            Some("k-shared-acct-1"),
            &[],
            Some(json!({
                "sourceId": "shared-source",
                "documents": [{
                    "url": "https://example.test/b1",
                    "markdown": "# Shared B1\nExplicit account shared specification."
                }]
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        let (st, sr_b2) = send_req(
            &app,
            "POST",
            "/v2/context/search",
            Some("k-shared-acct-2"),
            &[],
            Some(json!({ "sourceIds": ["shared-source"], "query": "Explicit account shared" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert!(
            !sr_b2["report"]["hits"].as_array().unwrap().is_empty(),
            "keys with the same explicit account must share Context: {sr_b2}"
        );

        // 2. ITNA HTTP cookie isolation & sharing + quota isolation/sharing.
        let srv = Router::new()
            .route(
                "/set/{token}",
                get(|axum::extract::Path(token): axum::extract::Path<String>| async move {
                    (
                        StatusCode::OK,
                        [("set-cookie", format!("sid={token}; Path=/"))],
                        Html(format!("<html><body><article><h1>Set {token}</h1><p>Cookie issued for {token}.</p></article></body></html>")),
                    )
                }),
            )
            .route(
                "/echo",
                get(|headers: HeaderMap| async move {
                    let cookie = headers
                        .get("cookie")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("NONE");
                    Html(format!("<html><body><article><h1>Echo</h1><p>COOKIE={cookie}</p></article></body></html>"))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, srv).await.unwrap() });

        let (st, _) = send_req(
            &app,
            "POST",
            "/v2/scrape",
            Some("k-same-name-1"),
            &[],
            Some(json!({ "url": format!("http://{addr}/set/A1_SECRET"), "engine": "http" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        let (st, echo_a2) = send_req(
            &app,
            "POST",
            "/v2/scrape",
            Some("k-same-name-2"),
            &[],
            Some(json!({ "url": format!("http://{addr}/echo"), "engine": "http" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let md_a2 = echo_a2["data"]["markdown"].as_str().unwrap_or("");
        assert!(
            !md_a2.contains("A1_SECRET"),
            "ITNA cookie leaked between keys sharing only display name: {md_a2}"
        );

        let (st, echo_a1) = send_req(
            &app,
            "POST",
            "/v2/scrape",
            Some("k-same-name-1"),
            &[],
            Some(json!({ "url": format!("http://{addr}/echo"), "engine": "http" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert!(
            echo_a1["data"]["markdown"]
                .as_str()
                .unwrap_or("")
                .contains("A1_SECRET"),
            "key must retain its own ITNA session cookie: {echo_a1}"
        );

        // Explicit shared account shares both ITNA cookies AND pages_per_day=3 quota.
        let (st, _) = send_req(
            &app,
            "POST",
            "/v2/scrape",
            Some("k-shared-acct-1"),
            &[],
            Some(json!({ "url": format!("http://{addr}/set/B_SHARED"), "engine": "http" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        let (st, echo_b2) = send_req(
            &app,
            "POST",
            "/v2/scrape",
            Some("k-shared-acct-2"),
            &[],
            Some(json!({ "url": format!("http://{addr}/echo"), "engine": "http" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert!(
            echo_b2["data"]["markdown"]
                .as_str()
                .unwrap_or("")
                .contains("B_SHARED"),
            "keys sharing explicit account must share ITNA session cookie: {echo_b2}"
        );

        // 3rd scrape uses the last page of acct-team-shared (pages_per_day=3); 4th returns 429.
        let (st3, _) = send_req(
            &app,
            "POST",
            "/v2/scrape",
            Some("k-shared-acct-1"),
            &[],
            Some(json!({ "url": format!("http://{addr}/echo"), "engine": "http" })),
        )
        .await;
        assert_eq!(st3, StatusCode::OK);
        let (st4, _) = send_req(
            &app,
            "POST",
            "/v2/scrape",
            Some("k-shared-acct-2"),
            &[],
            Some(json!({ "url": format!("http://{addr}/echo"), "engine": "http" })),
        )
        .await;
        assert_eq!(st4, StatusCode::TOO_MANY_REQUESTS);

        // Meanwhile k-same-name-2 only spent 1 of its own 3 pages (isolated from k-same-name-1's 2 pages).
        let (st_a2_2, _) = send_req(
            &app,
            "POST",
            "/v2/scrape",
            Some("k-same-name-2"),
            &[],
            Some(json!({ "url": format!("http://{addr}/echo"), "engine": "http" })),
        )
        .await;
        assert_eq!(st_a2_2, StatusCode::OK);
    });
}

#[test]
fn p1_05_anonymous_http_session_isolation_boundary() {
    let rt = runtime();
    rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let (app, scraper, _store) = build_app(dir.path(), vec![], HashMap::new(), false);

        // Ingest under X-Session-Id: sess-1.
        let (st, _) = send_req(
            &app,
            "POST",
            "/v2/context/ingest",
            None,
            &[("x-session-id", "sess-1")],
            Some(json!({
                "sourceId": "anon-src",
                "documents": [{
                    "url": "https://example.test/s1",
                    "markdown": "# Session One\nUnique session one content."
                }]
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        // Different X-Session-Id cannot read sess-1.
        let (st, r_s2) = send_req(
            &app,
            "POST",
            "/v2/context/search",
            None,
            &[("x-session-id", "sess-2")],
            Some(json!({ "sourceIds": ["anon-src"], "query": "Unique session one" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert!(r_s2["report"]["hits"].as_array().unwrap().is_empty());

        // Unheadered anonymous client cannot read sess-1.
        let (st, r_plain) = send_req(
            &app,
            "POST",
            "/v2/context/search",
            None,
            &[],
            Some(json!({ "sourceIds": ["anon-src"], "query": "Unique session one" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert!(r_plain["report"]["hits"].as_array().unwrap().is_empty());

        // Local CLI/MCP tenant (None) cannot read HTTP anonymous namespaces.
        let local_res = scraper
            .context(
                None,
                json!({ "action": "search", "sourceIds": ["anon-src"], "query": "Unique session one" }),
            )
            .await
            .unwrap();
        assert!(local_res["report"]["hits"].as_array().unwrap().is_empty());
    });
}

#[test]
fn p2_01_p2_02_p2_03_p2_04_routes_changes_learn_get_and_rate_limit() {
    let rt = runtime();
    rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let mut limits = HashMap::new();
        limits.insert(
            "k-rl".into(),
            KeyLimits {
                name: Some("rate-limited".into()),
                account: None,
                requests_per_minute: Some(2),
                max_concurrent_jobs: None,
                pages_per_day: None,
            },
        );
        let (app, _scraper, _store) =
            build_app(dir.path(), vec!["k-main".into()], limits, false);

        // P2-01: Fixed routes reject conflicting `action` / `op` fields with 400 and no side effects.
        let (st_c1, _) = send_req(
            &app,
            "POST",
            "/v2/context/ingest",
            Some("k-main"),
            &[],
            Some(json!({ "action": "search", "query": "hijack" })),
        )
        .await;
        assert_eq!(st_c1, StatusCode::BAD_REQUEST);

        let (st_c2, _) = send_req(
            &app,
            "POST",
            "/v2/context/ingest",
            Some("k-main"),
            &[],
            Some(json!({ "op": "learn", "enabled": false })),
        )
        .await;
        assert_eq!(st_c2, StatusCode::BAD_REQUEST);

        let (st_c3, _) = send_req(
            &app,
            "POST",
            "/v2/context",
            Some("k-main"),
            &[],
            Some(json!({ "action": "search", "op": "learn" })),
        )
        .await;
        assert_eq!(st_c3, StatusCode::BAD_REQUEST);

        // P2-03: GET /v2/context/learn rejects mutating query params and preserves state.
        let (st_get_mut, _) = send_req(
            &app,
            "GET",
            "/v2/context/learn?enabled=false&frozen=true",
            Some("k-main"),
            &[],
            None,
        )
        .await;
        assert_eq!(st_get_mut, StatusCode::BAD_REQUEST);

        let (st_get_ok, learn_status) =
            send_req(&app, "GET", "/v2/context/learn", Some("k-main"), &[], None).await;
        assert_eq!(st_get_ok, StatusCode::OK, "GET learn: {learn_status}");
        assert_eq!(learn_status["learn"]["enabled"], true);
        assert_eq!(learn_status["learn"]["frozen"], false);

        // P2-02: >128 deltas push seq 1 out of the ring; ActionMode::Changes reports resyncRequired.
        let total_revs = MAX_CONTEXT_DELTAS_PER_SOURCE + 4;
        for rev in 1..=total_revs {
            let (st, body) = send_req(
                &app,
                "POST",
                "/v2/context/ingest",
                Some("k-main"),
                &[],
                Some(json!({
                    "sourceId": "delta-ring",
                    "documents": [{
                        "url": "https://example.test/doc",
                        "markdown": format!("# Doc\nRevision number {rev} with updated metrics.")
                    }]
                })),
            )
            .await;
            assert_eq!(st, StatusCode::OK, "ingest rev {rev}: {body}");
        }

        let (st_gap, gap_body) = send_req(
            &app,
            "POST",
            "/v2/context/action",
            Some("k-main"),
            &[],
            Some(json!({
                "mode": "changes",
                "sourceIds": ["delta-ring"],
                "sinceDeltaSeq": 1
            })),
        )
        .await;
        assert_eq!(st_gap, StatusCode::OK, "changes gap: {gap_body}");
        assert_eq!(gap_body["outcome"]["stoppingReason"], "resyncRequired");
        assert!(
            gap_body["outcome"]["gaps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|g| g["kind"] == "delta_retention_gap"),
            "expected delta_retention_gap: {gap_body}"
        );

        let (st_cont, cont_body) = send_req(
            &app,
            "POST",
            "/v2/context/action",
            Some("k-main"),
            &[],
            Some(json!({
                "mode": "changes",
                "sourceIds": ["delta-ring"],
                "sinceDeltaSeq": total_revs - 1
            })),
        )
        .await;
        assert_eq!(st_cont, StatusCode::OK, "changes cont: {cont_body}");
        assert_eq!(cont_body["outcome"]["stoppingReason"], "sufficient");
        assert_eq!(cont_body["outcome"]["deltas"].as_array().unwrap().len(), 1);

        // P2-04: GET /v2/context/units and /deltas respect requests_per_minute and limit cap.
        let (st_u_ing, _) = send_req(
            &app,
            "POST",
            "/v2/context/ingest",
            Some("k-main"),
            &[],
            Some(json!({
                "sourceId": "multi-u",
                "documents": [
                    { "url": "https://example.test/u1", "markdown": "# U1\nParagraph one.\n\n## U1b\nParagraph two." },
                    { "url": "https://example.test/u2", "markdown": "# U2\nParagraph three.\n\n## U2b\nParagraph four." }
                ]
            })),
        )
        .await;
        assert_eq!(st_u_ing, StatusCode::OK);

        let (st_u_list, u_body) = send_req(
            &app,
            "GET",
            "/v2/context/units?sourceId=multi-u&limit=2",
            Some("k-main"),
            &[],
            None,
        )
        .await;
        assert_eq!(st_u_list, StatusCode::OK, "units list: {u_body}");
        assert_eq!(u_body["units"].as_array().unwrap().len(), 2);
        assert!(u_body["total"].as_u64().unwrap() > 2);

        // Rate-limited key (requests_per_minute = 2): 2 GETs pass, 3rd returns 429.
        let (r1, _) = send_req(
            &app,
            "GET",
            "/v2/context/units?sourceId=missing&limit=1",
            Some("k-rl"),
            &[],
            None,
        )
        .await;
        let (r2, _) = send_req(
            &app,
            "GET",
            "/v2/context/deltas?sourceId=missing",
            Some("k-rl"),
            &[],
            None,
        )
        .await;
        let (r3, _) = send_req(
            &app,
            "GET",
            "/v2/context/learn",
            Some("k-rl"),
            &[],
            None,
        )
        .await;
        assert_eq!(r1, StatusCode::OK);
        assert_eq!(r2, StatusCode::OK);
        assert_eq!(r3, StatusCode::TOO_MANY_REQUESTS);
    });
}

#[test]
fn p2_06_p2_07_corruption_fault_injection_blocks_overwrite_and_rebuilds() {
    let rt = runtime();
    rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let mut limits = HashMap::new();
        limits.insert(
            "k-fault".into(),
            KeyLimits {
                name: Some("fault-runner".into()),
                account: Some("acct-fault".into()),
                requests_per_minute: None,
                max_concurrent_jobs: None,
                pages_per_day: None,
            },
        );
        let (app, _scraper, store) = build_app(dir.path(), vec![], limits, false);

        // Seed two documents into tenant context.
        let (st, ing_body) = send_req(
            &app,
            "POST",
            "/v2/context/ingest",
            Some("k-fault"),
            &[],
            Some(json!({
                "sourceId": "fault-src",
                "documents": [
                    { "url": "https://example.test/doc-a", "markdown": "# Doc A\nAlpha turbine calibration baseline." },
                    { "url": "https://example.test/doc-b", "markdown": "# Doc B\nBeta compressor telemetry notes." }
                ]
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "seed ingest: {ing_body}");

        let (st_l, _) = send_req(
            &app,
            "POST",
            "/v2/context/learn",
            Some("k-fault"),
            &[],
            Some(json!({ "enabled": true })),
        )
        .await;
        assert_eq!(st_l, StatusCode::OK);

        let tenant = "acct:acct-fault";

        // 1. Corrupt `clrn` persisted learn state.
        let corrupt_learn = b"{corrupt_learn_blob";
        store
            .context_for(Some(tenant))
            .put_learn_bytes("default", corrupt_learn)
            .unwrap();

        // POST /v2/context/learn must fail instead of silently overwriting corrupt state with defaults.
        let (st_mut, _) = send_req(
            &app,
            "POST",
            "/v2/context/learn",
            Some("k-fault"),
            &[],
            Some(json!({ "frozen": true })),
        )
        .await;
        assert_eq!(st_mut, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            store
                .context_for(Some(tenant))
                .load_learn_bytes("default")
                .unwrap()
                .as_deref(),
            Some(corrupt_learn.as_slice()),
            "corrupt learn blob must not be overwritten"
        );

        // Search and Pack degrade gracefully and emit a learn_state_corrupt warning.
        let (st_s, s_body) = send_req(
            &app,
            "POST",
            "/v2/context/search",
            Some("k-fault"),
            &[],
            Some(json!({ "sourceIds": ["fault-src"], "query": "Alpha turbine" })),
        )
        .await;
        assert_eq!(st_s, StatusCode::OK, "search with corrupt learn: {s_body}");
        assert!(
            s_body["report"]["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w.as_str().unwrap_or("").contains("learn_state_corrupt")),
            "expected learn_state_corrupt warning in search report: {s_body}"
        );

        // 2. Corrupt `cidx` index blob via commit_delta with invalid JSON index bytes.
        let ctx_st = store.context_for(Some(tenant));
        let meta = ctx_st.get_meta("fault-src").unwrap().unwrap();
        let corrupt_delta = ContextDelta {
            source_id: "fault-src".into(),
            instance_id: meta.instance_id.clone(),
            delta_seq: 0,
            store_tick_seq: 0,
            created_at_ms: 0,
            observation_outcome: ObservationOutcome::VerifiedContent,
            unit_transitions: Vec::new(),
            invalidated_assets: Vec::new(),
            reused_asset_count: 0,
            recomputed_asset_count: 0,
            maintenance_mode: MaintenanceMode::Incremental,
            knowledge_delta: None,
        };
        let empty_res = BTreeMap::new();
        ctx_st
            .commit_delta(ContextCommitRequest {
                source_id: "fault-src",
                instance_id: &meta.instance_id,
                store_tick_seq: 0,
                spec_stamp: AssetSpecStamp::default(),
                resource_units: &empty_res,
                retired_units: &[],
                index_blob: Some(b"{corrupt_index_json"),
                delta: corrupt_delta.clone(),
                allow_same_zero_tick: true,
            })
            .unwrap();

        // Search rebuilds cleanly from live units and emits context_index_corrupt_rebuilt warning.
        let (st_s2, s2_body) = send_req(
            &app,
            "POST",
            "/v2/context/search",
            Some("k-fault"),
            &[],
            Some(json!({ "sourceIds": ["fault-src"], "query": "Beta compressor" })),
        )
        .await;
        assert_eq!(st_s2, StatusCode::OK, "search after corrupt cidx: {s2_body}");
        assert!(!s2_body["report"]["hits"].as_array().unwrap().is_empty());
        assert!(
            s2_body["report"]["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w.as_str().unwrap_or("").contains("context_index_corrupt_rebuilt")),
            "expected context_index_corrupt_rebuilt warning: {s2_body}"
        );

        // Re-corrupt cidx, then ingest ONLY doc-c: ingest must rebuild baseline from live units so doc-a & doc-b survive!
        ctx_st
            .commit_delta(ContextCommitRequest {
                source_id: "fault-src",
                instance_id: &meta.instance_id,
                store_tick_seq: 0,
                spec_stamp: AssetSpecStamp::default(),
                resource_units: &empty_res,
                retired_units: &[],
                index_blob: Some(b"{corrupt_index_json_again"),
                delta: corrupt_delta,
                allow_same_zero_tick: true,
            })
            .unwrap();

        let (st_ing3, ing3_body) = send_req(
            &app,
            "POST",
            "/v2/context/ingest",
            Some("k-fault"),
            &[],
            Some(json!({
                "sourceId": "fault-src",
                "documents": [
                    { "url": "https://example.test/doc-c", "markdown": "# Doc C\nGamma reactor thermal shield." }
                ]
            })),
        )
        .await;
        assert_eq!(st_ing3, StatusCode::OK, "ingest over corrupt cidx: {ing3_body}");

        // All three documents (A, B, C) must remain searchable after self-healing ingest.
        for q in ["Alpha turbine", "Beta compressor", "Gamma reactor"] {
            let (st_q, q_body) = send_req(
                &app,
                "POST",
                "/v2/context/search",
                Some("k-fault"),
                &[],
                Some(json!({ "sourceIds": ["fault-src"], "query": q })),
            )
            .await;
            assert_eq!(st_q, StatusCode::OK);
            assert!(
                !q_body["report"]["hits"].as_array().unwrap().is_empty(),
                "expected surviving hit for {q:?}, got: {q_body}"
            );
        }
    });
}
