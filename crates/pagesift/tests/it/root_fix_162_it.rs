//! 1.6.2 根因修复跨版本升级迁移与端到端回归测试：
//! 覆盖 1.6.0 -> 修复版、1.6.1 -> 修复版、1.6.2 -> 修复版、1.6.0 -> 1.6.1 -> 修复版（双命名空间 fail-closed 与显式 account 解除歧义）。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pagesift::{Scraper, ScraperConfig};
use pagesift_api::{Config as ApiConfig, Engine, KeyLimits, try_router};
use pagesift_store::Store;
use serde_json::{Value, json};
use tower::ServiceExt;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime")
}

fn legacy_fnv1a_160(key: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("key-{h:016x}")
}

async fn send_json(
    app: &axum::Router,
    method: &str,
    uri: &str,
    key: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {key}"));
    let body = match body {
        Some(v) => {
            req = req.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[test]
fn upgrade_matrix_160_161_162_and_dual_namespace_fail_closed() {
    let rt = runtime();
    rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let k_160_agent = "sk-160-agent-only";
        let k_160_attach = "sk-160-attach-only";
        let k_161_fresh = "sk-161-sha-only";
        let k_162_shared_1 = "sk-162-shared-1";
        let k_162_shared_2 = "sk-162-shared-2";
        let k_dual = "sk-160-then-161-dual";

        let fp160_agent = legacy_fnv1a_160(k_160_agent);
        let fp160_attach = legacy_fnv1a_160(k_160_attach);
        let fp_dual_160 = legacy_fnv1a_160(k_dual);

        // Step 1: First write authentic 1.6.1 SHA-256 namespaces via the HTTP router on a fresh store
        // (for `k_161_fresh` and `k_dual`, simulating a 1.6.1 run that wrote into SHA-256 space).
        {
            let mut sc_cfg = pagesift_store::Config::new(dir.path());
            sc_cfg.background = false;
            let store = Store::open(sc_cfg).unwrap();
            let mut cfg = ScraperConfig {
                store: Some(store.clone()),
                ..Default::default()
            };
            cfg.fetcher.env_proxy = false;
            let engine: Arc<dyn Engine> = Arc::new(Scraper::new(cfg).unwrap());
            let app_161 = try_router(
                engine,
                ApiConfig {
                    api_keys: vec![k_161_fresh.into(), k_dual.into()],
                    store: Some(store.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
            let (st1, b1) = send_json(
                &app_161,
                "POST",
                "/v2/context",
                k_161_fresh,
                Some(json!({
                    "action": "ingest",
                    "sourceId": "kb-161",
                    "documents": [{
                        "url": "https://v161.example/doc",
                        "markdown": "# V161Doc\nUnique marker v161_marker_alpha."
                    }]
                })),
            )
            .await;
            assert_eq!(st1, StatusCode::OK, "{b1}");
            let (st2, b2) = send_json(
                &app_161,
                "POST",
                "/v2/context",
                k_dual,
                Some(json!({
                    "action": "ingest",
                    "sourceId": "kb-dual",
                    "documents": [{
                        "url": "https://dual.example/sha161",
                        "markdown": "# Dual\nMarker sha161."
                    }]
                })),
            )
            .await;
            assert_eq!(st2, StatusCode::OK, "{b2}");
            store.flush().unwrap();
        }

        // Step 2: Seed 1.6.0 legacy namespaces (`agent/`, `attachment-access/`, and `k_dual`'s 1.6.0
        // FNV-1a context) plus 1.6.2 shared explicit account `acct-162-team`.
        {
            let mut sc_cfg = pagesift_store::Config::new(dir.path());
            sc_cfg.background = false;
            let store = Store::open(sc_cfg).unwrap();
            let mut cfg = ScraperConfig {
                store: Some(store.clone()),
                ..Default::default()
            };
            cfg.fetcher.env_proxy = false;
            let scraper = Scraper::new(cfg).unwrap();

            // 1.6.0 tenant whose ONLY persisted trace is an `agent/` record.
            store
                .tasks()
                .kv_put(
                    "agent/ag-legacy-160",
                    json!({ "owner": fp160_agent, "status": "completed" })
                        .to_string()
                        .as_bytes(),
                )
                .unwrap();
            // 1.6.0 tenant whose ONLY persisted trace is an `attachment-access/{account}/{id}` record.
            store
                .tasks()
                .kv_put(
                    format!("attachment-access/{fp160_attach}/att-legacy-160"),
                    b"9999999999999",
                )
                .unwrap();
            // 1.6.2 shared explicit account `acct-162-team`.
            scraper
                .context(
                    Some("acct:acct-162-team"),
                    json!({
                        "action": "ingest",
                        "sourceId": "kb-162",
                        "documents": [{
                            "url": "https://v162.example/doc",
                            "markdown": "# V162Shared\nUnique marker v162_shared_beta."
                        }]
                    }),
                )
                .await
                .unwrap();
            // 1.6.0 FNV-1a namespace for `k_dual` alongside the 1.6.1 SHA-256 data created above.
            scraper
                .context(
                    Some(&format!("acct:{fp_dual_160}")),
                    json!({
                        "action": "ingest",
                        "sourceId": "kb-dual",
                        "documents": [{
                            "url": "https://dual.example/legacy160",
                            "markdown": "# Dual\nMarker legacy160."
                        }]
                    }),
                )
                .await
                .unwrap();
            store.flush().unwrap();
        }

        // Boot A: including `k_dual` without explicit `account` MUST fail closed!
        {
            let mut sc_cfg = pagesift_store::Config::new(dir.path());
            sc_cfg.background = false;
            let store = Store::open(sc_cfg).unwrap();
            let mut cfg = ScraperConfig {
                store: Some(store.clone()),
                ..Default::default()
            };
            cfg.fetcher.env_proxy = false;
            let engine: Arc<dyn Engine> = Arc::new(Scraper::new(cfg).unwrap());
            let err = try_router(
                engine,
                ApiConfig {
                    api_keys: vec![k_dual.into()],
                    store: Some(store),
                    ..Default::default()
                },
            )
            .expect_err(
                "1.6.0 -> 1.6.1 dual-namespace key must fail closed without explicit account",
            );
            assert!(
                err.message.contains("both legacy") || err.message.contains("ambiguous"),
                "unexpected error message: {err}"
            );
        }

        // Boot B: 1.6.0 (agent/attachment), 1.6.1 (SHA-256), 1.6.2 (shared account), AND
        // `k_dual` explicitly disambiguated via `account` all boot cleanly and preserve data!
        {
            let mut sc_cfg = pagesift_store::Config::new(dir.path());
            sc_cfg.background = false;
            let store = Store::open(sc_cfg).unwrap();
            let mut cfg = ScraperConfig {
                store: Some(store.clone()),
                ..Default::default()
            };
            cfg.fetcher.env_proxy = false;
            let engine: Arc<dyn Engine> = Arc::new(Scraper::new(cfg).unwrap());

            let mut key_limits = HashMap::new();
            key_limits.insert(k_160_agent.into(), KeyLimits::new().with_pages_per_day(5));
            key_limits.insert(k_160_attach.into(), KeyLimits::new().with_pages_per_day(5));
            key_limits.insert(k_161_fresh.into(), KeyLimits::new().with_pages_per_day(5));
            key_limits.insert(
                k_162_shared_1.into(),
                KeyLimits::new()
                    .with_account("acct-162-team")
                    .with_pages_per_day(5),
            );
            key_limits.insert(
                k_162_shared_2.into(),
                KeyLimits::new()
                    .with_account("acct-162-team")
                    .with_pages_per_day(5),
            );
            key_limits.insert(
                k_dual.into(),
                KeyLimits::new()
                    .with_account("acct-dual-chosen")
                    .with_pages_per_day(5),
            );

            let app = try_router(
                engine,
                ApiConfig {
                    key_limits,
                    store: Some(store.clone()),
                    ..Default::default()
                },
            )
            .expect("clean upgrade + explicit disambiguation must succeed");

            // 1.6.1 SHA-256 tenant reads back its Context document seamlessly.
            let (st161, b161) = send_json(
                &app,
                "POST",
                "/v2/context",
                k_161_fresh,
                Some(json!({ "action": "search", "query": "v161_marker_alpha" })),
            )
            .await;
            assert_eq!(st161, StatusCode::OK, "{b161}");
            assert!(
                b161.to_string().contains("v161_marker_alpha"),
                "1.6.1 SHA-256 data must survive upgrade: {b161}"
            );

            // 1.6.2 shared explicit account: both keys access the shared tenant data.
            for k in [k_162_shared_1, k_162_shared_2] {
                let (st, body) = send_json(
                    &app,
                    "POST",
                    "/v2/context",
                    k,
                    Some(json!({ "action": "search", "query": "v162_shared_beta" })),
                )
                .await;
                assert_eq!(st, StatusCode::OK, "{body}");
                assert!(
                    body.to_string().contains("v162_shared_beta"),
                    "1.6.2 shared account data must be readable by {k}: {body}"
                );
            }

            // 1.6.0 agent-only & attachment-only tenants keep their legacy owner ID when creating monitors.
            for (k, expected_owner) in [
                (k_160_agent, fp160_agent.as_str()),
                (k_160_attach, fp160_attach.as_str()),
            ] {
                let (st, created) = send_json(
                    &app,
                    "POST",
                    "/v2/monitor",
                    k,
                    Some(json!({
                        "targets": [{ "type": "scrape", "urls": ["https://example.com/"] }]
                    })),
                )
                .await;
                assert_eq!(st, StatusCode::OK, "{created}");
                let mid = created["data"]["id"].as_str().unwrap();
                let raw = store
                    .tasks()
                    .kv_get(format!("monitor/{mid}"))
                    .unwrap()
                    .unwrap();
                let persisted: Value = serde_json::from_slice(&raw).unwrap();
                assert_eq!(
                    persisted["owner"].as_str(),
                    Some(expected_owner),
                    "1.6.0 key {k} must retain legacy owner {expected_owner}, got {persisted}"
                );
                let _ = send_json(&app, "DELETE", &format!("/v2/monitor/{mid}"), k, None).await;
            }
        }
    });
}
