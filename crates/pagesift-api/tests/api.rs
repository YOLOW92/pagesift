use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pagesift_api::types::{CrawlRequest, CrawlSummary, Document, Metadata, ScrapeOptions};
use pagesift_api::{ApiError, BoxFuture, Config, CrawlSink, Engine, ErrorCode, router};
use serde_json::{Value, json};
use tower::ServiceExt;

/// 假引擎：URL 里含 `blocked` 时返回 robots 禁止，含 `slow` 时稍微等一下。
struct Fake;

impl Engine for Fake {
    fn search_budgeted<'a>(
        &'a self,
        _req: &'a pagesift_api::types::SearchRequest,
        budget: Arc<dyn pagesift_api::PageBudget>,
    ) -> BoxFuture<'a, Result<Value, ApiError>> {
        Box::pin(async move {
            budget.acquire().await?;
            Ok(json!({"web": []}))
        })
    }

    fn extract_budgeted<'a>(
        &'a self,
        _req: &'a pagesift_api::types::ExtractRequest,
        budget: Arc<dyn pagesift_api::PageBudget>,
    ) -> BoxFuture<'a, Result<Value, ApiError>> {
        Box::pin(async move {
            budget.acquire().await?;
            Ok(json!({"result": "ok"}))
        })
    }
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
            let mut attachments = Vec::new();
            if opts.image_assets.is_some() {
                let mut a = pagesift_api::types::Attachment::new(
                    b"\x89PNG\r\n\x1a\nimage".to_vec(),
                    "png",
                    Some(url.into()),
                );
                a.inline_data = true;
                attachments.push(a);
            }
            Ok(Document {
                markdown: opts.wants("markdown").then(|| format!("# {url}")),
                attachments,
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

    /// 只抓起始页；带查询时报告一个停止原因。
    fn crawl<'a>(
        &'a self,
        req: &'a CrawlRequest,
        sink: Arc<dyn CrawlSink>,
    ) -> BoxFuture<'a, Result<(), ApiError>> {
        Box::pin(async move {
            let doc = self.scrape(&req.url, &req.scrape_options).await?;
            sink.page(doc).await;
            if req.query.is_some() {
                sink.summary(CrawlSummary {
                    stop_reason: "coverage_reached".into(),
                    coverage: Some(1.0),
                    term_evidence: [("refund".to_string(), 3)].into(),
                    pages: 1,
                })
                .await;
            }
            Ok(())
        })
    }
}

#[tokio::test]
async fn batch_image_assets_carry_duplicate_content_only_once() {
    let app = app(&[]);
    let (status, started) = call(
        &app,
        "POST",
        "/v2/batch/scrape",
        Some(json!({
            "urls": ["https://example.com/a", "https://example.com/b", "https://example.com/c"],
            "imageAssets": "attach"
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let path = format!("/v2/batch/scrape/{}", started["id"].as_str().unwrap());
    for _ in 0..100 {
        let (status, body) = call(&app, "GET", &path, None, None).await;
        assert_eq!(status, StatusCode::OK);
        if body["status"] == "completed" {
            let pages = body["data"].as_array().unwrap();
            assert_eq!(pages.len(), 3);
            let attachments: Vec<_> = pages.iter().map(|p| &p["attachments"][0]).collect();
            assert!(attachments.iter().all(|a| a["id"] == attachments[0]["id"]));
            assert_eq!(
                attachments
                    .iter()
                    .filter(|a| a.get("data").is_some())
                    .count(),
                1,
                "{body}"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("batch did not complete");
}

#[tokio::test]
async fn scrape_and_crawl_carry_image_assets() {
    let app = app(&[]);
    let (status, body) = call(
        &app,
        "POST",
        "/v2/scrape",
        Some(json!({
            "url": "https://example.com/a", "imageAssets": "attach"
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["data"]["attachments"][0]["data"].is_string());
    let (status, body) = call(
        &app,
        "POST",
        "/v2/crawl",
        Some(json!({
            "url": "https://example.com/a", "limit": 1, "scrapeOptions": {"imageAssets": "rewrite"}
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let path = format!("/v2/crawl/{}", body["id"].as_str().unwrap());
    for _ in 0..100 {
        let (_, body) = call(&app, "GET", &path, None, None).await;
        if body["status"] == "completed" {
            assert!(
                body["data"][0]["attachments"][0]["data"].is_string(),
                "{body}"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("crawl did not complete");
}

fn app(keys: &[&str]) -> axum::Router {
    router(
        Arc::new(Fake),
        Config {
            api_keys: keys.iter().map(|k| k.to_string()).collect(),
            public_url: Some("http://pagesift.test".into()),
            max_batch_concurrency: Some(4),
            ..Default::default()
        },
    )
}

async fn call_bytes(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, Vec<u8>) {
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
    (status, bytes.to_vec())
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let (status, bytes) = call_bytes(app, method, uri, body, key).await;
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
async fn jobs_are_private_to_their_owner() {
    let app = app(&["key-a", "key-b"]);
    let (status, started) = call(
        &app,
        "POST",
        "/v2/crawl",
        Some(json!({"url":"https://example.com/slow"})),
        Some("key-a"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = started["id"].as_str().unwrap();
    let (_, active) = call(&app, "GET", "/v2/crawl/active", None, Some("key-b")).await;
    assert!(
        !active["crawls"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == id)
    );
    for (method, suffix) in [
        ("GET", ""),
        ("GET", "/errors"),
        ("GET", "/stream"),
        ("DELETE", ""),
    ] {
        let (status, _) = call(
            &app,
            method,
            &format!("/v2/crawl/{id}{suffix}"),
            None,
            Some("key-b"),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {suffix}");
    }
    let (status, _) = call(&app, "GET", &format!("/v2/crawl/{id}"), None, Some("key-a")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn attachments_are_private_to_their_owner() {
    let app = app(&["key-a", "key-b"]);
    // A 用 imageAssets=attach 抓取含图片的 fixture 页，拿到附件 id
    let (status, v) = call(
        &app,
        "POST",
        "/v2/scrape",
        Some(json!({
            "url": "https://example.com/has-image",
            "imageAssets": "attach"
        })),
        Some("key-a"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let attachments = v["data"]["attachments"]
        .as_array()
        .expect("attachments array");
    assert!(!attachments.is_empty(), "expected at least 1 attachment");
    let id = attachments[0]["id"].as_str().expect("attachment id");

    // A 下载该 id → 200，字节与源图一致
    let (status, bytes_a) = call_bytes(
        &app,
        "GET",
        &format!("/v2/attachments/{id}"),
        None,
        Some("key-a"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes_a.as_slice(), b"\x89PNG\r\n\x1a\nimage");

    // B 下载该 id → 404（注意：断言 404，不是 403）
    let (status, err_b) = call(
        &app,
        "GET",
        &format!("/v2/attachments/{id}"),
        None,
        Some("key-b"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{err_b}");
    assert_eq!(
        err_b["error"],
        format!("attachment {id} not found or expired")
    );

    // B 下载伪造 id → 404
    let (status, err_fake) = call(
        &app,
        "GET",
        "/v2/attachments/nonexistent.png",
        None,
        Some("key-b"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{err_fake}");

    // 无 Key → 401
    let (status, _) = call(&app, "GET", &format!("/v2/attachments/{id}"), None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // B 自己抓取同一张图（同内容同 id）后，B 下载 → 200（去重保留、授权独立）
    let (status, v_b) = call(
        &app,
        "POST",
        "/v2/scrape",
        Some(json!({
            "url": "https://example.com/has-image",
            "imageAssets": "attach"
        })),
        Some("key-b"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v_b}");
    let b_attachments = v_b["data"]["attachments"]
        .as_array()
        .expect("attachments array");
    let b_id = b_attachments[0]["id"].as_str().expect("attachment id");
    assert_eq!(b_id, id);

    let (status, bytes_b) = call_bytes(
        &app,
        "GET",
        &format!("/v2/attachments/{id}"),
        None,
        Some("key-b"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes_b.as_slice(), b"\x89PNG\r\n\x1a\nimage");

    // A 依然可以下载
    let (status, bytes_a_again) = call_bytes(
        &app,
        "GET",
        &format!("/v2/attachments/{id}"),
        None,
        Some("key-a"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes_a_again.as_slice(), b"\x89PNG\r\n\x1a\nimage");
}

/// 只做 scrape、带延迟的引擎：用来观察并发请求里实际发生了多少次外部抓取。
struct Counting {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    delay: Duration,
}

impl Engine for Counting {
    fn scrape<'a>(
        &'a self,
        url: &'a str,
        _opts: &'a ScrapeOptions,
    ) -> BoxFuture<'a, Result<Document, ApiError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            if url.contains("blocked") {
                return Err(ApiError::new(ErrorCode::RobotsDisallowed, "disallowed"));
            }
            Ok(Document {
                markdown: Some(format!("# {url}")),
                metadata: Metadata {
                    source_url: Some(url.to_string()),
                    url: Some(url.to_string()),
                    status_code: 200,
                    ..Default::default()
                },
                ..Default::default()
            })
        })
    }
}

#[tokio::test]
async fn concurrent_scrapes_never_exceed_the_page_quota() {
    use pagesift_api::KeyLimits;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let app = router(
        Arc::new(Counting {
            calls: calls.clone(),
            delay: Duration::from_millis(40),
        }),
        Config {
            key_limits: [(
                "limited".to_string(),
                KeyLimits {
                    pages_per_day: Some(1),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        },
    );
    let mut tasks = Vec::new();
    for i in 0..20 {
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            call(
                &app,
                "POST",
                "/v2/scrape",
                Some(json!({"url": format!("https://example.com/{i}")})),
                Some("limited"),
            )
            .await
            .0
        }));
    }
    let mut ok = 0;
    for task in tasks {
        let status = task.await.unwrap();
        if status == StatusCode::OK {
            ok += 1;
        } else {
            assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        }
    }
    assert_eq!(ok, 1, "only one request may spend the single page");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "rejected requests must not reach the engine"
    );
}

#[tokio::test]
async fn batch_never_fetches_more_pages_than_the_quota() {
    use pagesift_api::KeyLimits;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let app = router(
        Arc::new(Counting {
            calls: calls.clone(),
            delay: Duration::ZERO,
        }),
        Config {
            key_limits: [(
                "limited".to_string(),
                KeyLimits {
                    pages_per_day: Some(1),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        },
    );
    let (status, started) = call(
        &app,
        "POST",
        "/v2/batch/scrape",
        Some(json!({"urls": ["https://example.com/a", "https://example.com/b"]})),
        Some("limited"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = started["id"].as_str().unwrap().to_string();
    let mut last = Value::Null;
    for _ in 0..200 {
        let (_, s) = call(
            &app,
            "GET",
            &format!("/v2/batch/scrape/{id}"),
            None,
            Some("limited"),
        )
        .await;
        if s["status"] == "completed" {
            last = s;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(last["status"], "completed");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "only the single paid page may be fetched"
    );
    let (_, errs) = call(
        &app,
        "GET",
        &format!("/v2/batch/scrape/{id}/errors"),
        None,
        Some("limited"),
    )
    .await;
    assert_eq!(errs["errors"][0]["code"], "QUOTA_EXCEEDED", "{errs}");
}

#[tokio::test]
async fn zero_quota_never_reaches_the_engine() {
    use pagesift_api::KeyLimits;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let app = router(
        Arc::new(Counting {
            calls: calls.clone(),
            delay: Duration::ZERO,
        }),
        Config {
            key_limits: [(
                "limited".to_string(),
                KeyLimits {
                    pages_per_day: Some(0),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        },
    );
    let (status, _) = call(
        &app,
        "POST",
        "/v2/scrape",
        Some(json!({"url": "https://example.com/a"})),
        Some("limited"),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn failed_scrapes_refund_the_reservation() {
    use pagesift_api::KeyLimits;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let app = router(
        Arc::new(Counting {
            calls: calls.clone(),
            delay: Duration::ZERO,
        }),
        Config {
            key_limits: [(
                "limited".to_string(),
                KeyLimits {
                    pages_per_day: Some(1),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        },
    );
    let (status, _) = call(
        &app,
        "POST",
        "/v2/scrape",
        Some(json!({"url": "https://example.com/blocked"})),
        Some("limited"),
    )
    .await;
    assert_ne!(status, StatusCode::OK);
    // 失败不计费：下一次仍然有额度
    let (status, _) = call(
        &app,
        "POST",
        "/v2/scrape",
        Some(json!({"url": "https://example.com/ok"})),
        Some("limited"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn search_and_extract_consume_page_quota() {
    use pagesift_api::KeyLimits;
    let app = router(
        Arc::new(Fake),
        Config {
            key_limits: [(
                "limited".to_string(),
                KeyLimits {
                    pages_per_day: Some(1),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        },
    );
    let (status, _) = call(
        &app,
        "POST",
        "/v2/search",
        Some(json!({"query":"hello", "scrapeOptions":{"formats":["markdown"]}})),
        Some("limited"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        "POST",
        "/v2/extract",
        Some(json!({"urls":["https://example.com"],"prompt":"test"})),
        Some("limited"),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn extract_results_are_private_and_spend_quota() {
    use pagesift_api::KeyLimits;
    let app = router(
        Arc::new(Fake),
        Config {
            api_keys: vec!["other".into()],
            key_limits: [(
                "limited".to_string(),
                KeyLimits {
                    pages_per_day: Some(1),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        },
    );
    let (status, started) = call(
        &app,
        "POST",
        "/v2/extract",
        Some(json!({"urls":["https://example.com"],"prompt":"test"})),
        Some("limited"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let path = format!("/v2/extract/{}", started["id"].as_str().unwrap());
    let (status, _) = call(&app, "GET", &path, None, Some("other")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let mut finished = false;
    for _ in 0..100 {
        let (_, value) = call(&app, "GET", &path, None, Some("limited")).await;
        if value["status"] == "completed" {
            finished = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(finished);
    let (status, _) = call(
        &app,
        "POST",
        "/v2/scrape",
        Some(json!({"url":"https://example.com"})),
        Some("limited"),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

/// 兼容接口照常接收未知字段，但在 warnings 里如实列出；客户端自报身份的字段不算。
#[tokio::test]
async fn ignored_fields_are_reported_as_warnings() {
    let app = app(&[]);
    let (status, v) = call(
        &app,
        "POST",
        "/v2/scrape",
        Some(
            json!({"url": "https://example.com/a", "origin": "python-sdk",
                    "zeroDataRetention": true, "lockdown": false}),
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        v["data"]["metadata"]["pagesift"]["warnings"],
        json!(["ignored_fields: lockdown, zeroDataRetention"])
    );
    let (_, v) = call(
        &app,
        "POST",
        "/v1/crawl",
        Some(json!({"url": "https://example.com", "prompt": "x",
                    "scrapeOptions": {"formats": ["markdown"], "redactPII": true}})),
        None,
    )
    .await;
    assert_eq!(
        v["warnings"],
        json!(["ignored_fields: prompt, scrapeOptions.redactPII"])
    );
    let (_, v) = call(
        &app,
        "POST",
        "/v2/scrape",
        Some(json!({"url": "https://example.com/a", "integration": "x"})),
        None,
    )
    .await;
    assert!(v["data"]["metadata"]["pagesift"].is_null(), "{v}");
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

async fn finished_crawl(app: &axum::Router, body: Value) -> Value {
    let (status, v) = call(app, "POST", "/v2/crawl", Some(body), None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    for _ in 0..200 {
        let (_, s) = call(app, "GET", &format!("/v2/crawl/{id}"), None, None).await;
        if s["status"] == "completed" {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("crawl did not finish");
}

#[tokio::test]
async fn job_status_reports_the_stop_reason_of_focused_crawls() {
    let app = app(&[]);
    let plain = finished_crawl(&app, json!({"url": "https://example.com/"})).await;
    assert!(plain.get("pagesift").is_none(), "{plain}");
    let focused = finished_crawl(
        &app,
        json!({"url": "https://example.com/", "query": "refund", "adaptive": true}),
    )
    .await;
    assert_eq!(
        focused["pagesift"],
        json!({"stopReason": "coverage_reached", "coverage": 1.0, "termEvidence": {"refund": 3}, "pages": 1})
    );
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
