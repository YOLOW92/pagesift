//! 监控端到端：创建、立即检查（`/run`）、实质变化报 changed 并附 diff 和字段变化、
//! 只改噪声（日期、计数）不报、条件请求 304、删除。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use pagesift::{Scraper, ScraperConfig};
use serde_json::{Value, json};

fn version(v: usize) -> String {
    // v0 → v1：只改日期和计数（噪声）；v1 → v2：价格和正文变了（实质）
    let (date, views, price, body) = match v {
        0 => ("2026-11-01", 120, "$19", "The widget ships in blue."),
        1 => ("2026-11-02", 340, "$19", "The widget ships in blue."),
        _ => (
            "2026-11-02",
            341,
            "$24",
            "The widget now ships in blue and red, with a new handle.",
        ),
    };
    format!(
        "<html><head><title>Widget</title></head><body><article><h1>Widget</h1>\
        <p>Updated {date}. {views} views.</p><p class='price'>{price}</p><p>{body}</p>\
        <p>Long description paragraph that stays the same across versions so the page has content.</p>\
        </article></body></html>"
    )
}

async fn call(api: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let mut req = axum::http::Request::builder().method(method).uri(uri);
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            axum::body::Body::from(b.to_string())
        }
        None => axum::body::Body::empty(),
    };
    let resp = api.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let st = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn monitor_reports_substantive_changes_only() {
    let v = Arc::new(AtomicUsize::new(0));
    let hits304 = Arc::new(AtomicUsize::new(0));
    let (vv, h3) = (v.clone(), hits304.clone());
    let app = Router::new().route(
        "/item",
        get(move |h: HeaderMap| {
            let n = vv.load(Ordering::SeqCst);
            let etag = format!("\"v{n}\"");
            let same = h.get("if-none-match").and_then(|x| x.to_str().ok()) == Some(etag.as_str());
            if same {
                h3.fetch_add(1, Ordering::SeqCst);
            }
            async move {
                if same {
                    return (StatusCode::NOT_MODIFIED, [("etag", etag)]).into_response();
                }
                ([("etag", etag)], Html(version(n))).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/item", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut cfg = ScraperConfig::default();
    cfg.fetcher.env_proxy = false;
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    let api = pagesift_api::router(
        Arc::new(Scraper::new(cfg).unwrap()),
        pagesift_api::Config::default(),
    );

    let (st, created) = call(&api, "POST", "/v2/monitor", Some(json!({
        "name": "widget",
        "targets": [{"type": "scrape", "urls": [url], "scrapeOptions": {
            "engine": "http", "ignoreRobotsTxt": true,
            "formats": ["markdown", {"type": "extract", "schema": {"baseSelector": "article", "fields": [
                {"name": "price", "selector": ".price", "type": "text"}]}}]}}]
    }))).await;
    assert_eq!(st, StatusCode::OK, "{created}");
    let id = created["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["data"]["schedule"]["adaptive"], true);

    let run = |api: Router, id: String| async move {
        call(&api, "POST", &format!("/v2/monitor/{id}/run"), None)
            .await
            .1
    };
    let first = run(api.clone(), id.clone()).await;
    assert_eq!(first["data"]["pages"][0]["status"], "new", "{first}");

    // 没变：条件请求 304
    let same = run(api.clone(), id.clone()).await;
    assert_eq!(same["data"]["pages"][0]["status"], "same", "{same}");
    assert_eq!(same["data"]["pages"][0]["notModified"], true, "{same}");
    assert_eq!(hits304.load(Ordering::SeqCst), 1);

    // 只改日期和计数：不算 changed
    v.store(1, Ordering::SeqCst);
    let noise = run(api.clone(), id.clone()).await;
    assert_eq!(noise["data"]["pages"][0]["status"], "same", "{noise}");
    assert_eq!(noise["data"]["pages"][0]["noiseOnly"], true, "{noise}");

    // 实质变化：changed，附统一 diff 和字段级变化
    v.store(2, Ordering::SeqCst);
    let changed = run(api.clone(), id.clone()).await;
    let p = &changed["data"]["pages"][0];
    assert_eq!(p["status"], "changed", "{changed}");
    assert!(
        p["diff"]["text"]
            .as_str()
            .unwrap()
            .contains("+The widget now ships"),
        "{p}"
    );
    assert_eq!(
        p["fieldChanges"][0],
        json!({"item": 0, "field": "price", "before": "$19", "after": "$24"})
    );

    let (_, checks) = call(&api, "GET", &format!("/v2/monitor/{id}/checks"), None).await;
    // 后台调度器创建后也会立即检查：至少是这里手动触发的 4 次
    assert!(checks["data"].as_array().unwrap().len() >= 4, "{checks}");
    let (_, list) = call(&api, "GET", "/v2/monitor", None).await;
    assert_eq!(list["data"].as_array().unwrap().len(), 1);
    let (st, _) = call(&api, "DELETE", &format!("/v2/monitor/{id}"), None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(&api, "GET", &format!("/v2/monitor/{id}"), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // 做不到的：邮件通知、LLM 判断、搜索目标
    for bad in [
        json!({"targets": [{"type": "scrape", "urls": ["https://e.com/"]}], "notification": {"email": {"enabled": true}}}),
        json!({"targets": [{"type": "scrape", "urls": ["https://e.com/"]}], "judgeEnabled": true}),
        json!({"targets": [{"type": "search", "query": "x"}]}),
        json!({"targets": [{"type": "scrape", "urls": ["https://e.com/"]}], "schedule": {"cron": "* * * * *"}}),
    ] {
        let (st, v) = call(&api, "POST", "/v2/monitor", Some(bad)).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    }
}
