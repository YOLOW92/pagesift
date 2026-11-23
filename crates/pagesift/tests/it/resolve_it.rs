//! 1.7 Resolve end-to-end integration tests:
//! 1. HTTP `/v2/resolve*` tenant isolation, cycle rejection, point-in-time history, read-only GET `/learn`,
//!    fixed-route conflict rejection, and per-key rate limiting on GET endpoints.
//! 2. Live delta reconciliation: Salsa Early Cutoff (`RelocatedOnly`), `ObservationLost` non-refutation,
//!    ScopeDependency negative/superlative invalidation on newly appearing records, Incremental vs Full
//!    agreement, and ABA source-instance replacement fallback.
//! 3. Bounded Gap acquisition against a local HTTP server honoring `allowNetworkAcquisition`,
//!    `allowedHosts`, and `maxAcquisitionRequests`.
//! 4. MCP `pagesift_resolve` and CLI `pagesift resolve` end-to-end workflows.

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
use pagesift::api::{Engine, KeyLimits};
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

fn build_http_router(
    dir: &std::path::Path,
    api_keys: Vec<String>,
    key_limits: std::collections::HashMap<String, KeyLimits>,
) -> (Router, Arc<Scraper>) {
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
            key_limits,
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
fn http_v2_resolve_tenant_isolation_cycles_history_readonly_learn_and_rate_limit() {
    let rt = runtime();
    let dir = tempfile::tempdir().unwrap();

    rt.block_on(async {
        let mut limits = std::collections::HashMap::new();
        limits.insert(
            "key-rate-limited".to_owned(),
            KeyLimits {
                requests_per_minute: Some(2),
                ..Default::default()
            },
        );
        let (app, scraper) = build_http_router(
            dir.path(),
            vec!["alpha-key".to_owned(), "beta-key".to_owned()],
            limits,
        );

        // Tenant alpha ingests pricing corpus
        let (st, _) = send_json(
            &app,
            "POST",
            "/v2/context/ingest",
            Some("alpha-key"),
            Some(json!({
                "sourceId": "pricing-src",
                "documents": [
                    {
                        "url": "https://vendor-a.example/pricing",
                        "originRoot": "vendor-a.example",
                        "markdown": "# Pricing\n\n| Plan | Annual Price |\n|---|---|\n| Plan Pro | $99 |\n"
                    }
                ]
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        // Fixed route must reject conflicting action
        let (st_conflict, conflict_body) = send_json(
            &app,
            "POST",
            "/v2/resolve/check",
            Some("alpha-key"),
            Some(json!({
                "action": "delete",
                "answer": "Plan Pro costs $99/yr."
            })),
        )
        .await;
        assert_eq!(st_conflict, StatusCode::BAD_REQUEST, "{conflict_body}");
        assert_eq!(conflict_body["code"], "INVALID_REQUEST");

        // Alpha persists parent conclusion C1
        let (st_c1, c1_body) = send_json(
            &app,
            "POST",
            "/v2/resolve/check",
            Some("alpha-key"),
            Some(json!({
                "sourceId": "pricing-src",
                "conclusionId": "c_parent",
                "answer": "Plan Pro costs $99/yr.",
                "persist": true
            })),
        )
        .await;
        assert_eq!(st_c1, StatusCode::OK, "{c1_body}");
        assert_eq!(c1_body["outcome"]["persisted"], true);
        assert_eq!(c1_body["outcome"]["conclusion"]["state"], "supported");
        let v1_ts = c1_body["outcome"]["conclusion"]["checkedAtMs"]
            .as_u64()
            .unwrap();

        // Alpha persists child conclusion C2 depending on C1
        let (st_c2, c2_body) = send_json(
            &app,
            "POST",
            "/v2/resolve/check",
            Some("alpha-key"),
            Some(json!({
                "sourceId": "pricing-src",
                "conclusionId": "c_child",
                "parentConclusionIds": ["c_parent"],
                "answer": "Plan Pro costs $99/yr.",
                "persist": true
            })),
        )
        .await;
        assert_eq!(st_c2, StatusCode::OK, "{c2_body}");

        // Transitive cycle c_parent -> c_child -> c_parent MUST be rejected!
        let (st_cyc, cyc_body) = send_json(
            &app,
            "POST",
            "/v2/resolve/check",
            Some("alpha-key"),
            Some(json!({
                "sourceId": "pricing-src",
                "conclusionId": "c_parent",
                "parentConclusionIds": ["c_child"],
                "answer": "Plan Pro costs $99/yr.",
                "persist": true
            })),
        )
        .await;
        assert_eq!(st_cyc, StatusCode::BAD_REQUEST, "{cyc_body}");
        assert!(
            cyc_body["error"]
                .as_str()
                .unwrap_or("")
                .contains("resolve_cycle_detected"),
            "unexpected cycle error: {cyc_body}"
        );

        // Tenant beta MUST NOT see alpha's conclusions, graph, or events, nor delete them
        let (st_b_list, b_list) = send_json(
            &app,
            "GET",
            "/v2/resolve/conclusions",
            Some("beta-key"),
            None,
        )
        .await;
        assert_eq!(st_b_list, StatusCode::OK);
        assert_eq!(b_list["total"], 0);

        let (st_b_get, _) = send_json(
            &app,
            "GET",
            "/v2/resolve/conclusions/c_parent",
            Some("beta-key"),
            None,
        )
        .await;
        assert_eq!(st_b_get, StatusCode::NOT_FOUND);

        let (st_b_del, _) = send_json(
            &app,
            "DELETE",
            "/v2/resolve/conclusions/c_parent",
            Some("beta-key"),
            None,
        )
        .await;
        assert_eq!(st_b_del, StatusCode::NOT_FOUND);

        // Update c_parent to v2 with refuting monthly claim, then verify immutable point-in-time replay
        let (st_v2, v2_body) = send_json(
            &app,
            "POST",
            "/v2/resolve/check",
            Some("alpha-key"),
            Some(json!({
                "sourceId": "pricing-src",
                "conclusionId": "c_parent",
                "answer": "Plan Pro costs $99/mo.",
                "persist": true
            })),
        )
        .await;
        assert_eq!(st_v2, StatusCode::OK, "{v2_body}");
        assert_eq!(v2_body["outcome"]["conclusion"]["version"], 2);
        assert_eq!(v2_body["outcome"]["conclusion"]["state"], "refuted");

        let (st_hist, hist_body) = send_json(
            &app,
            "GET",
            "/v2/resolve/conclusions/c_parent/history",
            Some("alpha-key"),
            None,
        )
        .await;
        assert_eq!(st_hist, StatusCode::OK, "{hist_body}");
        assert_eq!(hist_body["revisions"].as_array().unwrap().len(), 2);

        let (st_asof, asof_body) = send_json(
            &app,
            "GET",
            &format!("/v2/resolve/conclusions/c_parent?asOf={v1_ts}"),
            Some("alpha-key"),
            None,
        )
        .await;
        assert_eq!(st_asof, StatusCode::OK, "{asof_body}");
        assert_eq!(asof_body["revision"]["version"], 1);
        assert_eq!(asof_body["revision"]["state"], "supported");

        // GET /v2/resolve/learn is strictly read-only; POST /v2/resolve/learn ignores weak signals
        let (st_ro, ro_body) = send_json(
            &app,
            "GET",
            "/v2/resolve/learn?enabled=false",
            Some("alpha-key"),
            None,
        )
        .await;
        assert_eq!(st_ro, StatusCode::BAD_REQUEST, "{ro_body}");

        let (st_lrn, lrn_body) = send_json(
            &app,
            "POST",
            "/v2/resolve/learn",
            Some("alpha-key"),
            Some(json!({
                "feedback": [
                    { "domain": "pricing", "rawScore": 0.9, "groundedSupported": true, "isHardGroundTruth": false },
                    { "domain": "pricing", "rawScore": 0.9, "groundedSupported": true, "isHardGroundTruth": true }
                ]
            })),
        )
        .await;
        assert_eq!(st_lrn, StatusCode::OK, "{lrn_body}");
        assert_eq!(lrn_body["acceptedFeedback"], 1);
        assert_eq!(lrn_body["ignoredWeakOrFrozenFeedback"], 1);

        // Per-key requests_per_minute rate limit MUST apply to GET /v2/resolve/*
        let (r1, _) = send_json(
            &app,
            "GET",
            "/v2/resolve/conclusions",
            Some("key-rate-limited"),
            None,
        )
        .await;
        let (r2, _) = send_json(
            &app,
            "GET",
            "/v2/resolve/conclusions",
            Some("key-rate-limited"),
            None,
        )
        .await;
        let (r3, r3_body) = send_json(
            &app,
            "GET",
            "/v2/resolve/conclusions",
            Some("key-rate-limited"),
            None,
        )
        .await;
        assert_eq!(r1, StatusCode::OK);
        assert_eq!(r2, StatusCode::OK);
        assert_eq!(r3, StatusCode::TOO_MANY_REQUESTS, "{r3_body}");

        scraper.shutdown().await;
    });
}

#[test]
fn live_reconcile_early_cutoff_observation_lost_scope_expansion_and_aba_reset() {
    let rt = runtime();
    let dir = tempfile::tempdir().unwrap();

    rt.block_on(async {
        let mut sc_cfg = pagesift_store::Config::new(dir.path());
        sc_cfg.background = false;
        let store = pagesift_store::Store::open(sc_cfg).unwrap();

        let mut cfg = ScraperConfig::default();
        cfg.fetcher.env_proxy = false;
        cfg.fetcher.ssrf.allow_private_network = true;
        cfg.store = Some(store.clone());
        let scraper = Scraper::new(cfg).unwrap();

        // Seed initial corpus: Plan Pro is $99/yr (cheapest in initial closed set)
        let _ = Engine::context(
            &scraper,
            None,
            json!({
                "action": "ingest",
                "sourceId": "market",
                "documents": [{
                    "url": "https://vendor-a.example/pricing",
                    "originRoot": "vendor-a.example",
                    "markdown": "# Pricing\n\nPlan Pro costs $99/yr for teams.\n"
                }]
            }),
        )
        .await
        .unwrap();

        // Persist C1 (exact price), C2 (downstream of C1), C3 (superlative), C4 (negative existential)
        let c1 = Engine::resolve(
            &scraper,
            None,
            json!({
                "action": "check",
                "sourceId": "market",
                "conclusionId": "c1_price",
                "answer": "Plan Pro costs $99/yr.",
                "persist": true
            }),
        )
        .await
        .unwrap();
        assert_eq!(c1["outcome"]["conclusion"]["state"], "supported");

        let c2 = Engine::resolve(
            &scraper,
            None,
            json!({
                "action": "check",
                "sourceId": "market",
                "conclusionId": "c2_downstream",
                "parentConclusionIds": ["c1_price"],
                "answer": "Plan Pro costs $99/yr.",
                "persist": true
            }),
        )
        .await
        .unwrap();
        assert_eq!(c2["outcome"]["conclusion"]["state"], "supported");

        let c3 = Engine::resolve(
            &scraper,
            None,
            json!({
                "action": "check",
                "sourceId": "market",
                "conclusionId": "c3_cheapest",
                "domainKind": "closed_observed_set",
                "answer": "Plan Pro is the cheapest plan at $99/yr.",
                "persist": true
            }),
        )
        .await
        .unwrap();
        assert_eq!(c3["outcome"]["conclusion"]["state"], "supported");

        let c4 = Engine::resolve(
            &scraper,
            None,
            json!({
                "action": "check",
                "sourceId": "market",
                "conclusionId": "c4_no_incidents",
                "domainKind": "closed_observed_set",
                "answer": "There is no public record of Acme security incidents.",
                "persist": true
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            c4["outcome"]["conclusion"]["state"],
            "qualifiedInObservedScope"
        );

        // Initialize baseline checkpoint
        let _ = Engine::resolve(
            &scraper,
            None,
            json!({ "action": "reconcile", "sourceIds": ["market"] }),
        )
        .await
        .unwrap();

        // A. Salsa Early Cutoff: prepend unrelated banner so Plan Pro paragraph only relocates!
        let _ = Engine::context(
            &scraper,
            None,
            json!({
                "action": "ingest",
                "sourceId": "market",
                "documents": [{
                    "url": "https://vendor-a.example/pricing",
                    "originRoot": "vendor-a.example",
                    "markdown": "# Announcement\n\nWelcome to our refreshed developer portal.\n\n# Pricing\n\nPlan Pro costs $99/yr for teams.\n"
                }]
            }),
        )
        .await
        .unwrap();

        let rec_cut = Engine::resolve(
            &scraper,
            None,
            json!({ "action": "reconcile", "sourceIds": ["market"] }),
        )
        .await
        .unwrap();
        let impacted_cut = rec_cut["report"]["impactedConclusionIds"]
            .as_array()
            .unwrap();
        assert!(
            !impacted_cut.iter().any(|v| v == "c1_price"),
            "RelocatedOnly must NOT invalidate c1_price: {rec_cut}"
        );
        let c1_after_cut = Engine::resolve(
            &scraper,
            None,
            json!({ "action": "get", "conclusionId": "c1_price" }),
        )
        .await
        .unwrap();
        assert_eq!(c1_after_cut["conclusion"]["state"], "supported");

        // B. Scope Expansion (negative/superlative dependency): new competitor arrives without editing old pricing page!
        let _ = Engine::context(
            &scraper,
            None,
            json!({
                "action": "ingest",
                "sourceId": "market",
                "documents": [
                    {
                        "url": "https://vendor-b.example/pricing",
                        "originRoot": "vendor-b.example",
                        "markdown": "# Competitor Pricing\n\nPlan Nano costs $19/yr for teams.\n"
                    },
                    {
                        "url": "https://status.acme.example/incident-2026",
                        "originRoot": "status.acme.example",
                        "markdown": "# Security Incident Report\n\nPublic record of Acme security incidents disclosed on 2026-03-01.\n"
                    }
                ]
            }),
        )
        .await
        .unwrap();

        let rec_scope = Engine::resolve(
            &scraper,
            None,
            json!({ "action": "reconcile", "sourceIds": ["market"] }),
        )
        .await
        .unwrap();
        let scope_ids: Vec<&str> = rec_scope["report"]["scopeImpactedConclusionIds"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            scope_ids.contains(&"c3_cheapest"),
            "new cheaper plan must invalidate superlative c3_cheapest: {rec_scope}"
        );
        assert!(
            scope_ids.contains(&"c4_no_incidents"),
            "new incident report must invalidate negative existential c4_no_incidents: {rec_scope}"
        );

        // Incremental revalidation must flip both C3 and C4 to Refuted and agree 100% with Full!
        let rev_inc = Engine::resolve(
            &scraper,
            None,
            json!({ "action": "revalidate", "sourceIds": ["market"], "mode": "incremental" }),
        )
        .await
        .unwrap();
        assert!(rev_inc["outcome"]["checkedCount"].as_u64().unwrap() >= 2);
        let c3_inc = Engine::resolve(
            &scraper,
            None,
            json!({ "action": "get", "conclusionId": "c3_cheapest" }),
        )
        .await
        .unwrap();
        let c4_inc = Engine::resolve(
            &scraper,
            None,
            json!({ "action": "get", "conclusionId": "c4_no_incidents" }),
        )
        .await
        .unwrap();
        assert_eq!(c3_inc["conclusion"]["state"], "refuted");
        assert_eq!(c4_inc["conclusion"]["state"], "refuted");

        // C. ObservationLost (unreliable capture) => StalePendingRevalidation, NEVER directly Refuted!
        let _ = Engine::context(
            &scraper,
            None,
            json!({
                "action": "ingest",
                "sourceId": "market",
                "documents": [{
                    "url": "https://vendor-a.example/pricing",
                    "originRoot": "vendor-a.example",
                    "markdown": "",
                    "unreliable": true
                }]
            }),
        )
        .await
        .unwrap();

        let rec_lost = Engine::resolve(
            &scraper,
            None,
            json!({ "action": "reconcile", "sourceIds": ["market"] }),
        )
        .await
        .unwrap();
        let obs_ids: Vec<&str> = rec_lost["report"]["observationLostConclusionIds"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(obs_ids.contains(&"c1_price"), "{rec_lost}");
        assert!(
            obs_ids.contains(&"c2_downstream"),
            "downstream child must propagate staleness: {rec_lost}"
        );
        let c1_lost = Engine::resolve(
            &scraper,
            None,
            json!({ "action": "get", "conclusionId": "c1_price" }),
        )
        .await
        .unwrap();
        assert_eq!(
            c1_lost["conclusion"]["state"],
            "stalePendingRevalidation",
            "ObservationLost must NEVER directly mark Refuted!"
        );
        assert_eq!(
            c1_lost["conclusion"]["staleReason"],
            "observation_lost_pending_confirmation"
        );

        // D. ABA instance replacement: create Loop source `aba-src`, persist conclusion, delete + recreate `aba-src`
        let _ = pagesift::loop_ops::sync(
            &scraper,
            json!({
                "action": "create",
                "id": "aba-src",
                "seeds": ["https://aba.example/doc"]
            }),
        )
        .await
        .unwrap();
        let _ = Engine::context(
            &scraper,
            None,
            json!({
                "action": "ingest",
                "sourceId": "aba-src",
                "documents": [{
                    "url": "https://aba.example/doc",
                    "originRoot": "aba.example",
                    "markdown": "Nova Engine latency is 5ms."
                }]
            }),
        )
        .await
        .unwrap();
        let _ = Engine::resolve(
            &scraper,
            None,
            json!({
                "action": "check",
                "sourceId": "aba-src",
                "conclusionId": "c_aba",
                "answer": "Nova Engine latency is 5ms.",
                "persist": true
            }),
        )
        .await
        .unwrap();
        let _ = Engine::resolve(
            &scraper,
            None,
            json!({ "action": "reconcile", "sourceIds": ["aba-src"] }),
        )
        .await
        .unwrap();

        // Delete and recreate `aba-src` -> fresh instance_id!
        let _ = pagesift::loop_ops::sync(&scraper, json!({ "action": "delete", "id": "aba-src" }))
            .await
            .unwrap();
        let _ = pagesift::loop_ops::sync(
            &scraper,
            json!({
                "action": "create",
                "id": "aba-src",
                "seeds": ["https://aba.example/doc"]
            }),
        )
        .await
        .unwrap();
        let rec_aba = Engine::resolve(
            &scraper,
            None,
            json!({ "action": "reconcile", "sourceIds": ["aba-src"] }),
        )
        .await
        .unwrap();
        assert!(
            rec_aba["report"]["abaSupersededSources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "aba-src"),
            "ABA deletion+recreation must trigger conservative fallback: {rec_aba}"
        );
        let c_aba = Engine::resolve(
            &scraper,
            None,
            json!({ "action": "get", "conclusionId": "c_aba" }),
        )
        .await
        .unwrap();
        assert_eq!(c_aba["conclusion"]["state"], "stalePendingRevalidation");

        scraper.shutdown().await;
    });
}

#[test]
fn gap_resolution_enforces_permission_allowed_hosts_and_resolves_against_local_http() {
    let rt = runtime();
    let dir = tempfile::tempdir().unwrap();

    rt.block_on(async {
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = hits.clone();
        let app = Router::new().route(
            "/pricing",
            get(move || {
                let c = hits_clone.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Html(
                        "<html><body><h1>Official Pricing</h1><p>Atlas Pro costs $79/mo for standard accounts.</p></body></html>",
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/pricing");
        let server_task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let mut sc_cfg = pagesift_store::Config::new(dir.path());
        sc_cfg.background = false;
        let store = pagesift_store::Store::open(sc_cfg).unwrap();
        let mut cfg = ScraperConfig::default();
        cfg.fetcher.env_proxy = false;
        cfg.fetcher.ssrf.allow_private_network = true;
        cfg.policy.rate.default_interval = Duration::from_millis(1);
        cfg.store = Some(store);
        let scraper = Scraper::new(cfg).unwrap();

        // 1. Permission off => 0 HTTP hits
        let r_denied = Engine::resolve(
            &scraper,
            None,
            json!({
                "action": "check",
                "sourceId": "live-gap",
                "answer": "Atlas Pro costs $79/mo.",
                "candidateUrls": [url],
                "allowNetworkAcquisition": false
            }),
        )
        .await
        .unwrap();
        assert_eq!(r_denied["outcome"]["conclusion"]["state"], "insufficient");
        assert_eq!(
            r_denied["outcome"]["stoppingReason"],
            "gapsRemainPermissionRestricted"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0);

        // 2. Host outside allowedHosts => 0 HTTP hits
        let r_host_blocked = Engine::resolve(
            &scraper,
            None,
            json!({
                "action": "check",
                "sourceId": "live-gap",
                "answer": "Atlas Pro costs $79/mo.",
                "candidateUrls": [url],
                "allowedHosts": ["other.example"],
                "allowNetworkAcquisition": true,
                "maxAcquisitionRequests": 2
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            r_host_blocked["outcome"]["stoppingReason"],
            "gapsRemainPermissionRestricted"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0);

        // 3. Authorized host + budget => fetches local HTTP page, ingests into Context, resolves to Supported!
        let r_ok = Engine::resolve(
            &scraper,
            None,
            json!({
                "action": "check",
                "sourceId": "live-gap",
                "answer": "Atlas Pro costs $79/mo.",
                "candidateUrls": [url],
                "allowedHosts": ["127.0.0.1"],
                "allowNetworkAcquisition": true,
                "maxAcquisitionRequests": 2,
                "persist": true
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            r_ok["outcome"]["conclusion"]["state"], "supported",
            "expected gap resolution to verify claim: {r_ok}"
        );
        assert!(hits.load(Ordering::SeqCst) >= 1);
        assert!(r_ok["outcome"]["networkRequests"].as_u64().unwrap() >= 1);

        scraper.shutdown().await;
        server_task.abort();
    });
}

#[test]
fn mcp_pagesift_resolve_and_cli_resolve_round_trip() {
    let rt = runtime();
    let dir = tempfile::tempdir().unwrap();

    // 1. Verify MCP `pagesift_resolve` tool registration and execution
    rt.block_on(async {
        let mut sc_cfg = pagesift_store::Config::new(dir.path().join("mcp-store"));
        sc_cfg.background = false;
        let store = pagesift_store::Store::open(sc_cfg).unwrap();
        let mut cfg = ScraperConfig::default();
        cfg.fetcher.env_proxy = false;
        cfg.store = Some(store);
        let scraper = Arc::new(Scraper::new(cfg).unwrap());
        let tools = pagesift::mcp::tools(scraper.clone());
        let resolve_tool = tools
            .iter()
            .find(|t| t.definition().name == pagesift_mcp::tools::RESOLVE)
            .expect("pagesift_resolve tool must be registered in MCP toolset");

        let args = json!({
            "action": "check",
            "answer": "Plan Pro costs $99/mo.",
            "documents": [{
                "url": "https://vendor.example/pricing",
                "markdown": "| Plan | Annual Price |\n|---|---|\n| Plan Pro | $99 |\n"
            }]
        })
        .as_object()
        .unwrap()
        .clone();
        let out = resolve_tool
            .call(args, pagesift_mcp::ToolContext::default())
            .await
            .unwrap();
        let val = out
            .structured_content
            .expect("MCP resolve must return structured JSON");
        assert_eq!(val["success"], true);
        assert_eq!(val["outcome"]["conclusion"]["state"], "refuted");

        scraper.shutdown().await;
    });

    // 2. Verify CLI `pagesift resolve check` + `list` + `graph`
    let doc_path = dir.path().join("spec.md");
    std::fs::write(
        &doc_path,
        "# Spec\n\nPulse DB supports ACID transactions.\n",
    )
    .unwrap();
    let cli_store = dir.path().join("cli-store");
    let bin = env!("CARGO_BIN_EXE_pagesift");

    let check_out = Command::new(bin)
        .args([
            "resolve",
            "check",
            "--store",
            cli_store.to_str().unwrap(),
            "--file",
            doc_path.to_str().unwrap(),
            "--answer",
            "Pulse DB supports ACID transactions.",
            "--persist",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        check_out.status.success(),
        "cli check failed: {}",
        String::from_utf8_lossy(&check_out.stderr)
    );
    let check_json: Value = serde_json::from_slice(&check_out.stdout).unwrap();
    assert_eq!(check_json["success"], true);
    assert_eq!(check_json["outcome"]["conclusion"]["state"], "supported");

    let list_out = Command::new(bin)
        .args([
            "resolve",
            "list",
            "--store",
            cli_store.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(list_out.status.success());
    let list_json: Value = serde_json::from_slice(&list_out.stdout).unwrap();
    assert_eq!(list_json["total"], 1);
}
