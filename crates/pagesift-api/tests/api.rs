use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pagesift_api::types::{Document, Metadata, ScrapeOptions};
use pagesift_api::{ApiError, BoxFuture, Config, Engine, ErrorCode, router};
use serde_json::{Value, json};
use tower::ServiceExt;

/// 假引擎：URL 里含 `blocked` 时返回 robots 禁止，含 `slow` 时稍微等一下。
struct Fake;

impl Engine for Fake {
    fn scrape<'a>(
        &'a self,
        url: &'a str,
        opts: &'a ScrapeOptions,
    ) -> BoxFuture<'a, Result<Document, ApiError>> {
        Box::pin(async move {
            if url.contains("slow") {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            if url.contains("blocked") {
                return Err(ApiError::new(
                    ErrorCode::RobotsDisallowed,
                    "disallowed by robots.txt",
                ));
            }
            Ok(Document {
                markdown: opts.wants("markdown").then(|| format!("# {url}")),
                metadata: Metadata {
                    source_url: Some(url.to_string()),
                    url: Some(url.to_string()),
                    status_code: 200,
                    proxy_used: "basic".into(),
                    ..Default::default()
                },
                ..Default::default()
            })
        })
    }
}

fn app(keys: &[&str]) -> axum::Router {
    router(
        Arc::new(Fake),
        Config {
            api_keys: keys.iter().map(|k| k.to_string()).collect(),
            public_url: Some("http://pagesift.test".into()),
            max_batch_concurrency: Some(4),
        },
    )
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(k) = key {
        req = req.header("authorization", format!("Bearer {k}"));
    }
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v)
}

#[tokio::test]
async fn scrape_returns_firecrawl_shape() {
    let app = app(&[]);
    let (status, v) = call(
        &app,
        "POST",
        "/v2/scrape",
        Some(json!({"url": "https://example.com/a", "formats": ["markdown"]})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["success"], true);
    assert_eq!(v["data"]["markdown"], "# https://example.com/a");
    assert_eq!(v["data"]["metadata"]["sourceURL"], "https://example.com/a");
    assert_eq!(v["data"]["metadata"]["statusCode"], 200);
}

#[tokio::test]
async fn bypass_options_and_unsupported_features_are_rejected() {
    let app = app(&[]);
    for (body, code) in [
        (
            json!({"url": "https://example.com", "proxy": "stealth"}),
            "UNSUPPORTED_OPTION",
        ),
        (
            json!({"url": "https://example.com", "formats": ["screenshot"]}),
            "UNSUPPORTED_OPTION",
        ),
        (
            json!({"url": "https://example.com", "actions": [{"type": "click", "selector": "a"}]}),
            "UNSUPPORTED_OPTION",
        ),
        (json!({"url": "ftp://example.com"}), "INVALID_REQUEST"),
        (
            json!({"url": "https://example.com", "timeout": 2000, "waitFor": 1500}),
            "INVALID_REQUEST",
        ),
        (json!({"nope": 1}), "INVALID_REQUEST"),
    ] {
        let (status, v) = call(&app, "POST", "/v2/scrape", Some(body.clone()), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(
            (v["success"].clone(), v["code"].as_str()),
            (json!(false), Some(code)),
            "{body}"
        );
    }
    let (status, _) = call(
        &app,
        "POST",
        "/v2/scrape",
        Some(json!({"url": "https://example.com", "proxy": "auto"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn api_keys_are_enforced() {
    let app = app(&["secret"]);
    let body = json!({"url": "https://example.com"});
    let (status, v) = call(&app, "POST", "/v2/scrape", Some(body.clone()), None).await;
    assert_eq!(
        (status, v["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("UNAUTHORIZED"))
    );
    let (status, _) = call(
        &app,
        "POST",
        "/v2/scrape",
        Some(body.clone()),
        Some("wrong"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(&app, "POST", "/v2/scrape", Some(body), Some("secret")).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, "GET", "/healthz", None, None).await;
    assert_eq!(status, StatusCode::OK, "health check stays open");
}

#[tokio::test]
async fn batch_job_runs_to_completion_with_paging_and_errors() {
    let app = app(&[]);
    let mut urls: Vec<String> = (0..150)
        .map(|i| format!("https://example.com/slow/{i}"))
        .collect();
    urls.push("https://example.com/blocked".into());
    urls.push("not a url".into());
    let (status, v) = call(
        &app,
        "POST",
        "/v2/batch/scrape",
        Some(json!({"urls": urls})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = v["id"].as_str().unwrap().to_string();
    assert_eq!(
        v["url"],
        format!("http://pagesift.test/v2/batch/scrape/{id}")
    );
    assert_eq!(v["invalidURLs"], json!(["not a url"]));

    let mut last = Value::Null;
    for _ in 0..200 {
        let (_, s) = call(&app, "GET", &format!("/v2/batch/scrape/{id}"), None, None).await;
        if s["status"] == "completed" {
            last = s;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(last["status"], "completed", "job did not finish");
    assert_eq!(
        (last["total"].clone(), last["completed"].clone()),
        (json!(151), json!(151))
    );
    assert_eq!(last["data"].as_array().unwrap().len(), 100);
    let next = last["next"].as_str().unwrap();
    assert_eq!(
        next,
        format!("http://pagesift.test/v2/batch/scrape/{id}?skip=100")
    );
    let (_, page2) = call(
        &app,
        "GET",
        next.trim_start_matches("http://pagesift.test"),
        None,
        None,
    )
    .await;
    assert_eq!(page2["data"].as_array().unwrap().len(), 50);
    assert!(page2.get("next").is_none());

    let (_, errs) = call(
        &app,
        "GET",
        &format!("/v2/batch/scrape/{id}/errors"),
        None,
        None,
    )
    .await;
    assert_eq!(
        errs["robotsBlocked"],
        json!(["https://example.com/blocked"])
    );
    assert_eq!(errs["errors"][0]["code"], "ROBOTS_DISALLOWED");
}

#[tokio::test]
async fn unknown_routes_jobs_and_metrics() {
    let app = app(&[]);
    let (status, v) = call(&app, "GET", "/v2/nope", None, None).await;
    assert_eq!(
        (status, v["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("NOT_FOUND"))
    );
    let (status, _) = call(&app, "GET", "/v2/batch/scrape/does-not-exist", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, v) = call(
        &app,
        "POST",
        "/v2/map",
        Some(json!({"url": "https://example.com"})),
        None,
    )
    .await;
    assert_eq!(
        (status, v["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("UNSUPPORTED_OPTION"))
    );
    call(
        &app,
        "POST",
        "/v2/scrape",
        Some(json!({"url": "https://example.com"})),
        None,
    )
    .await;
    let (status, m) = call(&app, "GET", "/metrics", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let m = m.as_str().unwrap();
    assert!(
        m.contains("pagesift_scrapes_total{outcome=\"ok\"} 1"),
        "{m}"
    );
    assert!(m.contains("route=\"/v2/scrape\",status=\"200\""), "{m}");
}
