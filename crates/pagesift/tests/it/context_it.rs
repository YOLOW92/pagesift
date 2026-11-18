//! 1.6 Context end-to-end integration tests: HTTP `/v2/context` multi-tenant isolation,
//! Loop Sync -> Context reconciliation & restart recovery, Action stopping/permission boundaries,
//! MCP `pagesift_context` (stateless & persistent), and CLI `pagesift context` multi-format ingestion.

use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Html;
use axum::routing::get;
use http_body_util::BodyExt;
use pagesift::{Scraper, ScraperConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn build_http_router(dir: &std::path::Path, api_keys: Vec<String>) -> (Router, Arc<Scraper>) {
    let mut sc_cfg = pagesift_store::Config::new(dir);
    sc_cfg.background = false;
    let store = pagesift_store::Store::open(sc_cfg).unwrap();

    let mut cfg = ScraperConfig::default();
    cfg.fetcher.env_proxy = false;
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    cfg.policy.rate.per_host_concurrency = 4;
    cfg.store = Some(store.clone());

    let scraper = Arc::new(Scraper::new(cfg).unwrap());
    let router = pagesift::api::router(
        scraper.clone(),
        pagesift::api::Config {
            api_keys,
            store: Some(store),
            ..Default::default()
        },
    );
    (router, scraper)
}

async fn send_json(
    app: &Router,
    method: &str,
    uri: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(key) = bearer {
        builder = builder.header("authorization", format!("Bearer {key}"));
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
fn http_v2_context_tenant_isolation_conflict_pack_and_checkpoints() {
    let rt = runtime();
    let dir = tempfile::tempdir().unwrap();

    rt.block_on(async {
        let (app, scraper) = build_http_router(
            dir.path(),
            vec!["secret-alpha".to_owned(), "secret-beta".to_owned()],
        );

        // Tenant alpha ingests two independent sources disagreeing on SKU price + a quota table.
        let (st, ingested) = send_json(
            &app,
            "POST",
            "/v2/context/ingest",
            Some("secret-alpha"),
            Some(json!({
                "sourceId": "pricing-kb",
                "documents": [
                    {
                        "url": "https://vendor-a.example/pro",
                        "originRoot": "vendor-a.example",
                        "subjectKey": "sku:PRO-1",
                        "markdown": "# Pro Plan Specification\n\nprice: $49\n\nQuota limits apply unless Enterprise addendum is signed.\n\n| Plan | RPM | Burst |\n|---|---|---|\n| Starter | 60 | 10 |\n| Pro | 600 | 120 |\n"
                    },
                    {
                        "url": "https://audit-b.org/pro-review",
                        "originRoot": "audit-b.org",
                        "subjectKey": "sku:PRO-1",
                        "markdown": "# Independent Price Audit\n\nprice: $59\n\nVerified public list price is $59 per seat.\n"
                    }
                ]
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "ingest failed: {ingested}");
        assert_eq!(ingested["success"], true);
        let instance_id = ingested["delta"]["instanceId"].as_str().unwrap().to_owned();

        // Tenant beta cannot see Tenant alpha's units, search hits, or deltas.
        let (st_beta, beta_units) = send_json(
            &app,
            "GET",
            "/v2/context/units?sourceId=pricing-kb",
            Some("secret-beta"),
            None,
        )
        .await;
        assert_eq!(st_beta, StatusCode::OK);
        assert_eq!(beta_units["total"], 0);

        let (_, beta_search) = send_json(
            &app,
            "POST",
            "/v2/context/search",
            Some("secret-beta"),
            Some(json!({ "sourceId": "pricing-kb", "query": "Pro RPM price" })),
        )
        .await;
        assert_eq!(beta_search["report"]["hits"].as_array().unwrap().len(), 0);

        // Tenant alpha compiles a ContextPack and preserves table headers, qualifiers, and conflicts.
        let (st_pack, packed) = send_json(
            &app,
            "POST",
            "/v2/context/pack",
            Some("secret-alpha"),
            Some(json!({
                "sourceId": "pricing-kb",
                "query": "Pro Plan price RPM quota limits",
                "subjectKey": "sku:PRO-1",
                "budgetTokens": 512,
                "selector": "submodularClosure",
                "ordering": "edgeSalience"
            })),
        )
        .await;
        assert_eq!(st_pack, StatusCode::OK);
        let pack = &packed["pack"];
        assert_eq!(pack["mustKeepPreserved"], true);
        let text = pack["compiledContext"].as_str().unwrap();
        assert!(text.contains("Plan") && text.contains("RPM"), "missing table header in:\n{text}");
        assert!(text.contains("unless Enterprise addendum"), "missing qualifier in:\n{text}");
        assert!(
            pack["independentSourceCount"].as_u64().unwrap() >= 2,
            "expected >=2 independent origins: {pack}"
        );
        assert!(!pack["provenanceManifest"].as_array().unwrap().is_empty());

        // Consumer checkpoint poll + idempotent ack via GET/POST sub-routes.
        let (st_poll, poll1) = send_json(
            &app,
            "GET",
            "/v2/context/deltas?sourceId=pricing-kb&consumerId=indexer-1",
            Some("secret-alpha"),
            None,
        )
        .await;
        assert_eq!(st_poll, StatusCode::OK);
        assert_eq!(poll1["poll"]["deltas"].as_array().unwrap().len(), 1);

        let (st_ack, ack) = send_json(
            &app,
            "POST",
            "/v2/context/deltas/ack",
            Some("secret-alpha"),
            Some(json!({
                "sourceId": "pricing-kb",
                "consumerId": "indexer-1",
                "instanceId": instance_id,
                "ackedDeltaSeq": 1
            })),
        )
        .await;
        assert_eq!(st_ack, StatusCode::OK);
        assert_eq!(ack["checkpoint"]["ackedDeltaSeq"], 1);

        let (_, poll2) = send_json(
            &app,
            "GET",
            "/v2/context/deltas?sourceId=pricing-kb&consumerId=indexer-1",
            Some("secret-alpha"),
            None,
        )
        .await;
        assert_eq!(poll2["poll"]["deltas"].as_array().map_or(0, Vec::len), 0);

        scraper.shutdown().await;
    });
}

#[test]
fn sync_to_context_restart_recovery_and_action_boundaries() {
    let site_rt = runtime();
    let revision = Arc::new(AtomicUsize::new(1));
    let rev_route = revision.clone();

    let site_app = Router::new().route(
        "/docs/guide",
        get(move || {
            let r = rev_route.load(Ordering::SeqCst);
            async move {
                if r == 1 {
                    Html(
                        "<html><head><title>Guide</title></head><body><article>\
                         <h1>Rate Limiting</h1>\
                         <p>Default burst capacity is 120 requests per second unless throttled.</p>\
                         </article></body></html>"
                            .to_owned(),
                    )
                } else {
                    Html(
                        "<html><head><title>Guide</title></head><body><article>\
                         <p>Announcement: Q4 maintenance window scheduled.</p>\
                         <h1>Rate Limiting</h1>\
                         <p>Default burst capacity is 120 requests per second unless throttled.</p>\
                         </article></body></html>"
                            .to_owned(),
                    )
                }
            }
        }),
    );
    let listener = site_rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    site_rt.spawn(async move { axum::serve(listener, site_app).await.unwrap() });
    let page_url = format!("http://{addr}/docs/guide");

    let dir = tempfile::tempdir().unwrap();

    // Run 1: Create Loop source, run sync tick 1, reconcile Context, record stable unit_id, then drop.
    let initial_unit_id = {
        let rt = runtime();
        rt.block_on(async {
            let (app, scraper) = build_http_router(dir.path(), Vec::new());
            let (st_c, _) = send_json(
                &app,
                "POST",
                "/v2/sources",
                None,
                Some(json!({
                    "id": "guide-src",
                    "seeds": [page_url],
                    "maxDepth": 0,
                    "freshness": {
                        "minIntervalSecs": 1,
                        "defaultIntervalSecs": 1,
                        "maxIntervalSecs": 60
                    },
                    "budget": { "maxRequestsPerTick": 4 },
                    "allowBrowser": false
                })),
            )
            .await;
            assert!(st_c.is_success(), "unexpected status: {st_c}");

            let (st_s, _) = send_json(
                &app,
                "POST",
                "/v2/sources/guide-src/sync",
                None,
                Some(json!({ "forceAll": true })),
            )
            .await;
            assert_eq!(st_s, StatusCode::OK);

            let (_, synced_ctx) = send_json(
                &app,
                "POST",
                "/v2/context",
                None,
                Some(json!({ "action": "sync", "sourceId": "guide-src" })),
            )
            .await;
            assert_eq!(synced_ctx["success"], true);
            assert_eq!(synced_ctx["visibility"]["lagTicks"], 0);

            let (_, units_v1) = send_json(
                &app,
                "GET",
                "/v2/context/units?sourceId=guide-src",
                None,
                None,
            )
            .await;
            let uid = units_v1["units"]
                .as_array()
                .unwrap()
                .iter()
                .find(|u| u["text"].as_str().unwrap_or("").contains("120 requests"))
                .expect("rate limit unit")["unitId"]
                .as_str()
                .unwrap()
                .to_owned();

            scraper.shutdown().await;
            uid
        })
    };

    // Advance upstream page to v2 (inserting a top banner before the section) and reopen store from disk.
    revision.store(2, Ordering::SeqCst);
    {
        let rt = runtime();
        rt.block_on(async {
            let (app, scraper) = build_http_router(dir.path(), Vec::new());

            // Permission boundary: Verify mode requiring 2 independent sources with network disabled stops with PermissionRestricted.
            let (_, verify_restricted) = send_json(
                &app,
                "POST",
                "/v2/context/action",
                None,
                Some(json!({
                    "mode": "verify",
                    "sourceId": "guide-src",
                    "query": "Default burst capacity 120 requests",
                    "minIndependentSources": 2,
                    "allowNetworkAcquisition": false,
                    "candidateUrls": [format!("http://{addr}/docs/guide")]
                })),
            )
            .await;
            assert_eq!(
                verify_restricted["outcome"]["stoppingReason"],
                "permissionRestricted"
            );

            // Watch action with allowNetworkAcquisition refreshes the Loop source and reconciles v2!
            let (_, watched) = send_json(
                &app,
                "POST",
                "/v2/context/action",
                None,
                Some(json!({
                    "mode": "watch",
                    "sourceId": "guide-src",
                    "query": "burst capacity 120 requests",
                    "maxStalenessMs": 0,
                    "allowNetworkAcquisition": true
                })),
            )
            .await;
            assert_eq!(watched["success"], true);

            // Verify unit_id stability across prefix insertion!
            let (_, units_v2) = send_json(
                &app,
                "GET",
                "/v2/context/units?sourceId=guide-src",
                None,
                None,
            )
            .await;
            let matched = units_v2["units"]
                .as_array()
                .unwrap()
                .iter()
                .find(|u| u["text"].as_str().unwrap_or("").contains("120 requests"))
                .expect("rate limit unit after v2 prefix insertion");
            assert_eq!(matched["unitId"].as_str().unwrap(), initial_unit_id);

            scraper.shutdown().await;
        });
    }
}

#[test]
fn mcp_and_cli_context_surfaces_work_with_multiformat_files() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("store");
    let html_file = dir.path().join("policy.html");
    let csv_file = dir.path().join("quotas.csv");

    std::fs::write(
        &html_file,
        "<html><head><title>Refund Policy</title></head><body><article>\
         <h1>Enterprise Refunds</h1>\
         <table><caption>Refund Windows</caption><thead><tr><th>Region</th><th>Days</th></tr></thead>\
         <tbody><tr><td>EU</td><td>14</td></tr><tr><td>US</td><td>30</td></tr></tbody></table>\
         <p>Refunds apply within 30 days unless custom hardware was provisioned.</p>\
         </article></body></html>",
    )
    .unwrap();
    std::fs::write(
        &csv_file,
        "tier,rpm,burst\nfree,30,5\nenterprise,6000,500\n",
    )
    .unwrap();

    // 1. Stateless MCP `pagesift_context` without Store (validating MCP schema & execution)
    let rt = runtime();
    rt.block_on(async {
        let stateless = Arc::new(Scraper::new(ScraperConfig::default()).unwrap());
        let all_tools = pagesift::mcp::tools(stateless.clone());
        pagesift_mcp::McpServer::builder("pagesift", "1.6.0")
            .tools(all_tools.clone())
            .build()
            .expect("pagesift_context MCP tool schema validates cleanly");
        let ctx_tool = all_tools
            .iter()
            .find(|t| t.definition().name == "pagesift_context")
            .expect("pagesift_context registered even without store");
        let res = ctx_tool
            .call(
                serde_json::from_value(json!({
                    "action": "pack",
                    "query": "enterprise rpm",
                    "budget_tokens": 256,
                    "documents": [{
                        "url": "https://example.com/inline",
                        "markdown": "# Quotas\n\nEnterprise tier supports 6000 rpm.\n"
                    }]
                }))
                .unwrap(),
                pagesift_mcp::ToolContext::default(),
            )
            .await
            .unwrap();
        assert!(!res.is_error);
        let payload = res.structured_content.expect("structured JSON output");
        assert_eq!(payload["success"], true);
        assert!(
            payload["pack"]["compiledContext"]
                .as_str()
                .unwrap()
                .contains("6000 rpm")
        );
        stateless.shutdown().await;
    });

    // 2. CLI `pagesift context ingest` + `pack` + `deltas` on real local HTML & CSV files
    let bin = env!("CARGO_BIN_EXE_pagesift");
    let ingest_out = Command::new(bin)
        .env("PAGESIFT_CONFIG", "")
        .args([
            "context",
            "ingest",
            "--store",
            store_dir.to_str().unwrap(),
            "--source-id",
            "local-kb",
            "--file",
            html_file.to_str().unwrap(),
            "--file",
            csv_file.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        ingest_out.status.success(),
        "cli ingest stderr: {}",
        String::from_utf8_lossy(&ingest_out.stderr)
    );
    let ingest_json: Value = serde_json::from_slice(&ingest_out.stdout).unwrap();
    assert_eq!(ingest_json["success"], true);
    assert!(ingest_json["liveUnitCount"].as_u64().unwrap() >= 2);

    let pack_out = Command::new(bin)
        .env("PAGESIFT_CONFIG", "")
        .args([
            "context",
            "pack",
            "--store",
            store_dir.to_str().unwrap(),
            "--source-id",
            "local-kb",
            "--budget-tokens",
            "512",
            "--json",
            "Enterprise Refunds EU Days enterprise 6000",
        ])
        .output()
        .unwrap();
    assert!(
        pack_out.status.success(),
        "cli pack stderr: {}",
        String::from_utf8_lossy(&pack_out.stderr)
    );
    let pack_json: Value = serde_json::from_slice(&pack_out.stdout).unwrap();
    let compiled = pack_json["pack"]["compiledContext"].as_str().unwrap();
    assert!(compiled.contains("Refund Windows") || compiled.contains("EU"));
    assert!(compiled.contains("unless custom hardware"));

    // 3. Stateless MCP strict schema validation for `learn` (with feedback & driftGuard) and `units` (with url)
    rt.block_on(async {
        let mut sc_cfg = pagesift_store::Config::new(&store_dir);
        sc_cfg.background = false;
        let store = pagesift_store::Store::open(sc_cfg).unwrap();
        let cfg = ScraperConfig {
            store: Some(store),
            ..ScraperConfig::default()
        };
        let stateful = Arc::new(Scraper::new(cfg).unwrap());
        let all_tools = pagesift::mcp::tools(stateful.clone());
        pagesift_mcp::McpServer::builder("pagesift", "1.6.0")
            .tools(all_tools.clone())
            .build()
            .unwrap();
        let ctx_tool = all_tools
            .iter()
            .find(|t| t.definition().name == "pagesift_context")
            .unwrap();

        let learn_payload = json!({
            "action": "learn",
            "enabled": true,
            "driftGuard": "cusum",
            "feedback": {
                "rawScore": 0.85,
                "groundedCorrect": true,
                "isHardGroundTruth": true
            }
        });
        pagesift_mcp::schema::validate(&ctx_tool.definition().input_schema, &learn_payload)
            .expect("learn payload with feedback & driftGuard must pass strict MCP inputSchema");
        let learn_res = ctx_tool
            .call(
                serde_json::from_value(learn_payload).unwrap(),
                pagesift_mcp::ToolContext::default(),
            )
            .await
            .unwrap();
        assert!(!learn_res.is_error, "learn failed: {learn_res:?}");

        let units_payload = json!({
            "action": "units",
            "sourceId": "local-kb",
            "url": "https://local.invalid/policy.html",
            "limit": 10
        });
        pagesift_mcp::schema::validate(&ctx_tool.definition().input_schema, &units_payload)
            .expect("units payload with url must pass strict MCP inputSchema");
        let units_res = ctx_tool
            .call(
                serde_json::from_value(units_payload).unwrap(),
                pagesift_mcp::ToolContext::default(),
            )
            .await
            .unwrap();
        assert!(!units_res.is_error, "units failed: {units_res:?}");
        assert!(
            units_res.structured_content.unwrap()["total"]
                .as_u64()
                .unwrap()
                >= 1
        );
        stateful.shutdown().await;
    });

    // 4. CLI local file URL encoding (`#` / `?` / spaces) and duplicate-basename disambiguation
    let dir_a = dir.path().join("sub_a");
    let dir_b = dir.path().join("sub_b");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();
    let f_a = dir_a.join("release #1?.md");
    let f_b = dir_b.join("release #1?.md");
    std::fs::write(&f_a, "# Alpha Service\n\nMax connections: 1024.\n").unwrap();
    std::fs::write(&f_b, "# Beta Service\n\nMax connections: 4096.\n").unwrap();

    let dup_ingest = Command::new(bin)
        .env("PAGESIFT_CONFIG", "")
        .args([
            "context",
            "ingest",
            "--store",
            store_dir.to_str().unwrap(),
            "--source-id",
            "dup-files",
            "--file",
            f_a.to_str().unwrap(),
            "--file",
            f_b.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        dup_ingest.status.success(),
        "dup ingest stderr: {}",
        String::from_utf8_lossy(&dup_ingest.stderr)
    );

    let dup_search = Command::new(bin)
        .env("PAGESIFT_CONFIG", "")
        .args([
            "context",
            "search",
            "--store",
            store_dir.to_str().unwrap(),
            "--source-id",
            "dup-files",
            "--json",
            "Max connections 1024 4096",
        ])
        .output()
        .unwrap();
    assert!(dup_search.status.success());
    let dup_json: Value = serde_json::from_slice(&dup_search.stdout).unwrap();
    let hit_texts: String = dup_json["report"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| h["unit"]["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        hit_texts.contains("1024"),
        "missing sub_a content: {hit_texts}"
    );
    assert!(
        hit_texts.contains("4096"),
        "missing sub_b content: {hit_texts}"
    );
}

#[test]
fn context_hardening_numeric_ids_barriers_same_batch_dedup_and_sync_deletion() {
    let site_rt = runtime();
    let fetch_hits = Arc::new(AtomicUsize::new(0));
    let hits_pub = fetch_hits.clone();
    let hits_priv = fetch_hits.clone();

    let site_app = Router::new()
        .route(
            "/public/spec",
            get(move || {
                hits_pub.fetch_add(1, Ordering::SeqCst);
                async move {
                    Html(
                        "<html><head><title>Public Spec</title></head><body><article>\
                         <h1>Public Quota</h1>\
                         <p>Standard throughput limit is 900 rps.</p>\
                         <a href=\"/internal/secret\">Internal Secret</a>\
                         </article></body></html>"
                            .to_owned(),
                    )
                }
            }),
        )
        .route(
            "/internal/secret",
            get(move || {
                hits_priv.fetch_add(1, Ordering::SeqCst);
                async move {
                    Html(
                        "<html><head><title>Secret</title></head><body><article>\
                         <h1>Internal Token</h1>\
                         <p>Private admin bypass token is ZX-9900.</p>\
                         </article></body></html>"
                            .to_owned(),
                    )
                }
            }),
        );
    let listener = site_rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    site_rt.spawn(async move { axum::serve(listener, site_app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let rt = runtime();
    rt.block_on(async {
        let (app, scraper) = build_http_router(dir.path(), Vec::new());

        // 1. Validation boundaries: oversized budgetTokens, empty query, control-char sourceId, bad timestamp -> 400
        let (st_bad_budget, err_budget) = send_json(
            &app,
            "POST",
            "/v2/context/pack",
            None,
            Some(json!({
                "query": "quota",
                "budgetTokens": 9_999_999,
                "documents": [{"url": "https://example.com/a", "markdown": "quota 10"}]
            })),
        )
        .await;
        assert_eq!(st_bad_budget, StatusCode::BAD_REQUEST, "{err_budget}");
        assert_eq!(err_budget["code"], "INVALID_REQUEST");

        let (st_empty_q, _) = send_json(
            &app,
            "POST",
            "/v2/context/search",
            None,
            Some(json!({
                "query": "   ",
                "documents": [{"url": "https://example.com/a", "markdown": "quota 10"}]
            })),
        )
        .await;
        assert_eq!(st_empty_q, StatusCode::BAD_REQUEST);

        let (st_ctrl_id, _) = send_json(
            &app,
            "POST",
            "/v2/context/ingest",
            None,
            Some(json!({
                "sourceId": "bad\u{001f}id",
                "documents": [{"url": "https://example.com/a", "markdown": "quota 10"}]
            })),
        )
        .await;
        assert_eq!(st_ctrl_id, StatusCode::BAD_REQUEST);

        // 2. Pure-numeric `sourceId=12345` & `consumerId=999`, same-batch duplicate URL evolution,
        // JS safe-integer float timestamp (`1750000000000.0`), and 304/503/unreliable non-retraction!
        let (st_num, ing1) = send_json(
            &app,
            "POST",
            "/v2/context/ingest",
            None,
            Some(json!({
                "sourceId": "12345",
                "documents": [
                    {
                        "url": "https://example.com/rate#frag1",
                        "observedAtMs": 1750000000000.0,
                        "markdown": "# Limits\n\nLegacy limit is 111 rpm.\n"
                    },
                    {
                        "url": "https://example.com/rate#frag2",
                        "observedAtMs": 1750000005000.0,
                        "markdown": "# Limits\n\nUpdated limit is 777 rpm.\n"
                    }
                ]
            })),
        )
        .await;
        assert_eq!(st_num, StatusCode::OK, "{ing1}");

        // GET `/v2/context/units?sourceId=12345` must work and must NOT contain ghost `111 rpm`!
        let (st_get_num, units_num) = send_json(
            &app,
            "GET",
            "/v2/context/units?sourceId=12345",
            None,
            None,
        )
        .await;
        assert_eq!(st_get_num, StatusCode::OK, "{units_num}");
        let all_text: String = units_num["units"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|u| u["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all_text.contains("777 rpm"));
        assert!(!all_text.contains("111 rpm"), "ghost unit from superseded batch entry: {all_text}");

        // Search must also have zero ghost inverted postings for `111`
        let (_, ghost_search) = send_json(
            &app,
            "POST",
            "/v2/context/search",
            None,
            Some(json!({
                "sourceId": "12345",
                "query": "111",
                "asOfMs": 1750000010000.0
            })),
        )
        .await;
        assert_eq!(ghost_search["report"]["hits"].as_array().unwrap().len(), 0);

        // Ingesting 304 Not Modified, 503 Unavailable, and 403/unreliable barrier must preserve active `777 rpm`!
        for bad_doc in [
            json!({"url": "https://example.com/rate", "statusCode": 304, "markdown": ""}),
            json!({"url": "https://example.com/rate", "statusCode": 503, "markdown": "Service Unavailable"}),
            json!({"url": "https://example.com/rate", "statusCode": 200, "unreliable": true, "markdown": "Verify you are human"}),
        ] {
            let (st_b, res_b) = send_json(
                &app,
                "POST",
                "/v2/context/ingest",
                None,
                Some(json!({ "sourceId": "12345", "documents": [bad_doc] })),
            )
            .await;
            assert_eq!(st_b, StatusCode::OK, "{res_b}");
        }
        let (_, units_after_barriers) = send_json(
            &app,
            "GET",
            "/v2/context/units?sourceId=12345",
            None,
            None,
        )
        .await;
        let preserved: String = units_after_barriers["units"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|u| u["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(preserved.contains("777 rpm"), "active unit was retracted by 304/503/barrier: {preserved}");

        // Numeric consumerId in GET `/v2/context/deltas?sourceId=12345&consumerId=999`
        let (st_poll_num, poll_num) = send_json(
            &app,
            "GET",
            "/v2/context/deltas?sourceId=12345&consumerId=999",
            None,
            None,
        )
        .await;
        assert_eq!(st_poll_num, StatusCode::OK, "{poll_num}");
        assert_eq!(poll_num["poll"]["status"]["state"], "incrementalReady");

        // 3. Loop source `excludePaths` respected across `Watch` action AND no ghost exposure after source deletion!
        let pub_url = format!("http://{addr}/public/spec");
        let (st_src, _) = send_json(
            &app,
            "POST",
            "/v2/sources",
            None,
            Some(json!({
                "id": "scoped-src",
                "seeds": [pub_url],
                "excludePaths": ["/internal/"],
                "maxDepth": 2,
                "freshness": { "minIntervalSecs": 1, "defaultIntervalSecs": 1, "maxIntervalSecs": 60 },
                "budget": { "maxRequestsPerTick": 6 },
                "allowBrowser": false
            })),
        )
        .await;
        assert!(st_src.is_success());

        let (_, _) = send_json(
            &app,
            "POST",
            "/v2/sources/scoped-src/sync",
            None,
            Some(json!({ "forceAll": true })),
        )
        .await;
        let (_, synced1) = send_json(
            &app,
            "POST",
            "/v2/context",
            None,
            Some(json!({ "action": "sync", "sourceId": "scoped-src" })),
        )
        .await;
        let scoped_inst = synced1["visibility"]["instanceId"].as_str().unwrap().to_owned();
        let _ = send_json(
            &app,
            "POST",
            "/v2/context/deltas/ack",
            None,
            Some(json!({
                "sourceId": "scoped-src",
                "consumerId": "ext-worker",
                "instanceId": scoped_inst,
                "ackedDeltaSeq": 1
            })),
        )
        .await;

        // `Watch` refresh (even with explicit `candidateUrls`) MUST preserve `excludePaths: ["/internal/"]` and never ingest `ZX-9900`!
        let (st_w, _) = send_json(
            &app,
            "POST",
            "/v2/context/action",
            None,
            Some(json!({
                "mode": "watch",
                "sourceId": "scoped-src",
                "candidateUrls": [format!("http://{addr}/public/spec")],
                "query": "throughput limit 900 rps",
                "maxStalenessMs": 0,
                "allowNetworkAcquisition": true
            })),
        )
        .await;
        assert_eq!(st_w, StatusCode::OK);
        let (_, secret_check) = send_json(
            &app,
            "POST",
            "/v2/context/search",
            None,
            Some(json!({ "sourceId": "scoped-src", "query": "ZX-9900" })),
        )
        .await;
        assert_eq!(
            secret_check["report"]["hits"].as_array().unwrap().len(),
            0,
            "Watch bypassed excludePaths and ingested /internal/secret!"
        );

        // Delete Loop source `scoped-src` -> Context search/units must return 0 ghost hits, and consumer poll must require FullResync!
        let (st_del, _) = send_json(&app, "DELETE", "/v2/sources/scoped-src", None, None).await;
        assert!(st_del.is_success());

        let (_, units_after_del) = send_json(
            &app,
            "GET",
            "/v2/context/units?sourceId=scoped-src",
            None,
            None,
        )
        .await;
        assert_eq!(
            units_after_del["total"], 0,
            "deleted Loop source leaked ghost units: {units_after_del}"
        );

        let (_, search_after_del) = send_json(
            &app,
            "POST",
            "/v2/context/search",
            None,
            Some(json!({ "query": "900 rps" })),
        )
        .await;
        assert_eq!(
            search_after_del["report"]["hits"].as_array().unwrap().len(),
            0,
            "deleted Loop source leaked ghost search hits: {search_after_del}"
        );

        let (_, poll_after_del) = send_json(
            &app,
            "GET",
            "/v2/context/deltas?sourceId=scoped-src&consumerId=ext-worker",
            None,
            None,
        )
        .await;
        assert_eq!(
            poll_after_del["poll"]["status"]["state"],
            "fullResyncRequired"
        );

        scraper.shutdown().await;
    });
}

#[test]
fn re_review_404_caution_loop_ingest_coherence_as_of_replay_and_schema_parity() {
    let site_rt = runtime();
    let flaky_mode = Arc::new(AtomicUsize::new(200));
    let flaky_route = flaky_mode.clone();

    let loop_rev = Arc::new(AtomicUsize::new(1));
    let loop_hits = Arc::new(AtomicUsize::new(0));
    let loop_rev_r = loop_rev.clone();
    let loop_hits_r = loop_hits.clone();

    let ext_hits = Arc::new(AtomicUsize::new(0));
    let ext_hits_r = ext_hits.clone();

    let site_app = Router::new()
        .route(
            "/flaky/spec",
            get(move || {
                let code = flaky_route.load(Ordering::SeqCst);
                async move {
                    match code {
                        200 => (
                            StatusCode::OK,
                            Html(
                                "<html><head><title>Spec</title></head><body><article>\
                                 <h1>Cluster Spec</h1><p>Cluster capacity is 320 nodes.</p>\
                                 </article></body></html>"
                                    .to_owned(),
                            ),
                        ),
                        404 => (
                            StatusCode::NOT_FOUND,
                            Html("<html><body>Not Found</body></html>".to_owned()),
                        ),
                        _ => (
                            StatusCode::GONE,
                            Html("<html><body>Gone</body></html>".to_owned()),
                        ),
                    }
                }
            }),
        )
        .route(
            "/loop/feed",
            get(move || {
                loop_hits_r.fetch_add(1, Ordering::SeqCst);
                let r = loop_rev_r.load(Ordering::SeqCst);
                async move {
                    let v = if r == 1 { 110 } else { 880 };
                    Html(format!(
                        "<html><head><title>Feed</title></head><body><article>\
                         <h1>Throughput</h1><p>Managed feed throughput is {v} req/s.</p>\
                         </article></body></html>"
                    ))
                }
            }),
        )
        .route(
            "/ext/corroborate",
            get(move || {
                ext_hits_r.fetch_add(1, Ordering::SeqCst);
                async move {
                    Html(
                        "<html><head><title>Ext</title></head><body><article>\
                         <h1>Audit</h1><p>External audit confirms 110 req/s baseline.</p>\
                         </article></body></html>"
                            .to_owned(),
                    )
                }
            }),
        );

    let listener = site_rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    site_rt.spawn(async move { axum::serve(listener, site_app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("store");
    let rt = runtime();

    rt.block_on(async {
        let (app, scraper) = build_http_router(&store_dir, Vec::new());

        // 1. Single HTTP 404 via real scrape (`from_api_document`) MUST degrade to `Unverified`
        // (`SuspectedGone`), NOT withdraw! Only HTTP 410 confirms withdrawal.
        let flaky_url = format!("http://{addr}/flaky/spec");
        let (st1, _) = send_json(
            &app,
            "POST",
            "/v2/context/action",
            None,
            Some(json!({
                "mode": "research",
                "sourceId": "flaky-kb",
                "query": "Cluster capacity 320 nodes",
                "candidateUrls": [flaky_url],
                "allowNetworkAcquisition": true
            })),
        )
        .await;
        assert_eq!(st1, StatusCode::OK);

        flaky_mode.store(404, Ordering::SeqCst);
        let (st_404, _) = send_json(
            &app,
            "POST",
            "/v2/context/action",
            None,
            Some(json!({
                "mode": "research",
                "sourceId": "flaky-kb",
                "query": "missing field forcing re-scrape",
                "requiredFields": ["nonexistent_field_xyz"],
                "candidateUrls": [format!("http://{addr}/flaky/spec")],
                "allowNetworkAcquisition": true
            })),
        )
        .await;
        assert_eq!(st_404, StatusCode::OK);

        let (_, units_after_404) = send_json(
            &app,
            "GET",
            "/v2/context/units?sourceId=flaky-kb",
            None,
            None,
        )
        .await;
        assert!(
            units_after_404["total"].as_u64().unwrap() >= 1,
            "single HTTP 404 falsely withdrew active units: {units_after_404}"
        );
        let (_, search_unverified) = send_json(
            &app,
            "POST",
            "/v2/context/search",
            None,
            Some(json!({
                "sourceId": "flaky-kb",
                "query": "320 nodes",
                "includeUnverified": true
            })),
        )
        .await;
        assert!(
            !search_unverified["report"]["hits"]
                .as_array()
                .unwrap()
                .is_empty(),
            "unit should remain retrievable with includeUnverified=true after transient 404"
        );

        // HTTP 410 Gone MUST set `confirmed_gone: true` and withdraw the unit!
        flaky_mode.store(410, Ordering::SeqCst);
        let (st_410, _) = send_json(
            &app,
            "POST",
            "/v2/context/action",
            None,
            Some(json!({
                "mode": "research",
                "sourceId": "flaky-kb",
                "query": "missing field forcing re-scrape",
                "requiredFields": ["nonexistent_field_xyz"],
                "candidateUrls": [format!("http://{addr}/flaky/spec")],
                "allowNetworkAcquisition": true
            })),
        )
        .await;
        assert_eq!(st_410, StatusCode::OK);
        let (_, units_after_410) = send_json(
            &app,
            "GET",
            "/v2/context/units?sourceId=flaky-kb",
            None,
            None,
        )
        .await;
        assert_eq!(units_after_410["total"], 0);

        // 2. Managed Loop source coherence:
        //    (a) Direct `ingest` on freshly created Loop source binds `spec.instance_id` (does NOT evaporate);
        //    (b) `Action` does NOT double-scrape just-synced Loop seeds AND isolates external URLs into
        //        `"context-acquired"` without advancing `managed-loop`'s `indexed_tick_seq` ahead of `total_ticks`!
        let feed_url = format!("http://{addr}/loop/feed");
        let ext_url = format!("http://{addr}/ext/corroborate");
        let (st_loop, _) = send_json(
            &app,
            "POST",
            "/v2/sources",
            None,
            Some(json!({
                "id": "managed-loop",
                "seeds": [feed_url],
                "maxDepth": 0,
                "freshness": { "minIntervalSecs": 1, "defaultIntervalSecs": 1, "maxIntervalSecs": 60 },
                "budget": { "maxRequestsPerTick": 4 },
                "allowBrowser": false
            })),
        )
        .await;
        assert!(st_loop.is_success());

        let (st_pre, _) = send_json(
            &app,
            "POST",
            "/v2/context/ingest",
            None,
            Some(json!({
                "sourceId": "managed-loop",
                "documents": [{
                    "url": format!("http://{addr}/loop/feed"),
                    "markdown": "# Seed Note\n\nPre-sync bootstrap note 42.\n"
                }]
            })),
        )
        .await;
        assert_eq!(st_pre, StatusCode::OK);
        let (_, pre_search) = send_json(
            &app,
            "POST",
            "/v2/context/search",
            None,
            Some(json!({ "sourceId": "managed-loop", "query": "bootstrap note 42" })),
        )
        .await;
        assert!(
            !pre_search["report"]["hits"].as_array().unwrap().is_empty(),
            "ingest into newly created Loop source evaporated due to instance_id mismatch!"
        );

        // Run Action in Verify mode (minIndependentSources=2) passing BOTH the Loop seed and an external URL:
        // 2A runs 1 sync tick on `managed-loop` (fetching `feed_url` once), and 2C must ONLY fetch `ext_url`!
        loop_hits.store(0, Ordering::SeqCst);
        ext_hits.store(0, Ordering::SeqCst);
        let (st_act, act_res) = send_json(
            &app,
            "POST",
            "/v2/context/action",
            None,
            Some(json!({
                "mode": "verify",
                "sourceId": "managed-loop",
                "query": "throughput 110 req/s",
                "minIndependentSources": 2,
                "maxStalenessMs": 0,
                "candidateUrls": [
                    format!("http://{addr}/loop/feed"),
                    ext_url
                ],
                "allowNetworkAcquisition": true
            })),
        )
        .await;
        assert_eq!(st_act, StatusCode::OK, "{act_res}");
        assert_eq!(
            loop_hits.load(Ordering::SeqCst),
            1,
            "Action double-scraped already-synced Loop seed URL in the same call!"
        );
        assert_eq!(ext_hits.load(Ordering::SeqCst), 1);

        // Advance upstream feed to v2 (880 req/s) and run Sync Tick 2: Context MUST NOT skip Tick 2!
        loop_rev.store(2, Ordering::SeqCst);
        let (st_t2, _) = send_json(
            &app,
            "POST",
            "/v2/sources/managed-loop/sync",
            None,
            Some(json!({ "forceAll": true })),
        )
        .await;
        assert_eq!(st_t2, StatusCode::OK);

        let (_, search_t2) = send_json(
            &app,
            "POST",
            "/v2/context/search",
            None,
            Some(json!({ "sourceId": "managed-loop", "query": "880 req/s" })),
        )
        .await;
        assert!(
            !search_t2["report"]["hits"].as_array().unwrap().is_empty(),
            "Loop Tick 2 was silently skipped because indexed_tick_seq jumped ahead!"
        );
        // External URL in `context-acquired` also remains intact (not pruned by `managed-loop` sync)!
        let (_, ext_check) = send_json(
            &app,
            "POST",
            "/v2/context/search",
            None,
            Some(json!({ "sourceId": "context-acquired", "query": "External audit confirms 110" })),
        )
        .await;
        assert!(!ext_check["report"]["hits"].as_array().unwrap().is_empty());

        // 3. Point-in-time `asOfMs` replay across t1 ingest -> t2 304 -> t3 rewrite -> t4 410 withdrawal!
        for doc in [
            json!({
                "url": "https://timeline.example/quota",
                "observedAtMs": 1_700_000_001_000u64,
                "markdown": "# Quota\n\nHistorical quota is 100 rpm.\n"
            }),
            json!({
                "url": "https://timeline.example/quota",
                "observedAtMs": 1_700_000_002_000u64,
                "statusCode": 304,
                "markdown": ""
            }),
            json!({
                "url": "https://timeline.example/quota",
                "observedAtMs": 1_700_000_003_000u64,
                "markdown": "# Quota\n\nRewritten quota is 500 rpm.\n"
            }),
            json!({
                "url": "https://timeline.example/quota",
                "observedAtMs": 1_700_000_004_000u64,
                "statusCode": 410,
                "markdown": ""
            }),
        ] {
            let (st_tl, _) = send_json(
                &app,
                "POST",
                "/v2/context/ingest",
                None,
                Some(json!({ "sourceId": "timeline-kb", "documents": [doc] })),
            )
            .await;
            assert_eq!(st_tl, StatusCode::OK);
        }

        // Between t1 and t2 (before 304 bumped latest observed_at_ms to t2): must STILL find "100 rpm"!
        let (_, asof_1500) = send_json(
            &app,
            "POST",
            "/v2/context/search",
            None,
            Some(json!({
                "sourceId": "timeline-kb",
                "query": "quota rpm",
                "asOfMs": 1_700_000_001_500u64
            })),
        )
        .await;
        let text_1500: String = asof_1500["report"]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|h| h["unit"]["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text_1500.contains("100 rpm"),
            "asOfMs=1500 lost unit across subsequent 304/rewrite/410: {asof_1500}"
        );

        // Between t3 and t4: must find "500 rpm" and NOT "100 rpm"!
        let (_, asof_3500) = send_json(
            &app,
            "POST",
            "/v2/context/search",
            None,
            Some(json!({
                "sourceId": "timeline-kb",
                "query": "quota rpm",
                "asOfMs": 1_700_000_003_500u64
            })),
        )
        .await;
        let text_3500: String = asof_3500["report"]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|h| h["unit"]["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text_3500.contains("500 rpm") && !text_3500.contains("100 rpm"));

        // After t4 (410 Gone): must return 0 hits!
        let (_, asof_4500) = send_json(
            &app,
            "POST",
            "/v2/context/search",
            None,
            Some(json!({
                "sourceId": "timeline-kb",
                "query": "quota rpm",
                "asOfMs": 1_700_000_004_500u64
            })),
        )
        .await;
        assert_eq!(asof_4500["report"]["hits"].as_array().unwrap().len(), 0);

        // 4. `DriftGuardKind::None` + MCP strict inputSchema for `"none"` & nested `search`/`pack`
        let (st_dg_none, dg_none_res) = send_json(
            &app,
            "POST",
            "/v2/context/learn",
            None,
            Some(json!({ "driftGuard": "none" })),
        )
        .await;
        assert_eq!(st_dg_none, StatusCode::OK, "{dg_none_res}");
        assert_eq!(dg_none_res["learn"]["driftGuard"], "none");

        let all_tools = pagesift::mcp::tools(scraper.clone());
        let ctx_tool = all_tools
            .iter()
            .find(|t| t.definition().name == "pagesift_context")
            .unwrap();
        pagesift_mcp::schema::validate(
            &ctx_tool.definition().input_schema,
            &json!({ "action": "learn", "driftGuard": "none" }),
        )
        .expect("MCP schema must accept driftGuard='none'");
        pagesift_mcp::schema::validate(
            &ctx_tool.definition().input_schema,
            &json!({
                "action": "pack",
                "search": { "query": "throughput", "sourceIds": ["managed-loop"] },
                "pack": { "budgetTokens": 256 }
            }),
        )
        .expect("MCP schema must accept nested 'search' and 'pack' objects");

        scraper.shutdown().await;
    });

    // CLI `pagesift context action --mode changes` MUST work without requiring a dummy positional `query`!
    let bin = env!("CARGO_BIN_EXE_pagesift");
    let note_md = dir.path().join("note.md");
    std::fs::write(&note_md, "# Release Note\n\nCLI changes delta check.\n").unwrap();
    let cli_ing = Command::new(bin)
        .env("PAGESIFT_CONFIG", "")
        .args([
            "context",
            "ingest",
            "--store",
            store_dir.to_str().unwrap(),
            "--source-id",
            "cli-changes-kb",
            "--file",
            note_md.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(cli_ing.status.success());

    let changes_cli = Command::new(bin)
        .env("PAGESIFT_CONFIG", "")
        .args([
            "context",
            "action",
            "--store",
            store_dir.to_str().unwrap(),
            "--mode",
            "changes",
            "--source-id",
            "cli-changes-kb",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        changes_cli.status.success(),
        "CLI `--mode changes` without positional query failed: {}",
        String::from_utf8_lossy(&changes_cli.stderr)
    );
    let changes_json: Value = serde_json::from_slice(&changes_cli.stdout).unwrap();
    assert_eq!(changes_json["success"], true);
    assert!(
        !changes_json["outcome"]["deltas"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
