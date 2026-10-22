//! 部署成服务要用的运维能力：任务持久化和重启恢复、webhook、按 API key 的限流和配额、监控指标。

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use pagesift_api::types::{CrawlRequest, CrawlSummary, Document, Metadata, ScrapeOptions};
use pagesift_api::{
    ApiError, BoxFuture, Config, CrawlSink, Engine, ErrorCode, KeyLimits, WebhookSettings, router,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const BASE: &str = "https://site.example";

/// 假引擎：记下抓过的每个 URL。整站爬取是一棵二叉树（第 i 页链接到 2i+1、2i+2），
/// 按 [`CrawlSink`] 的约定支持断点续爬。
struct Fake {
    scraped: Arc<Mutex<Vec<String>>>,
    delay: Duration,
}

impl Fake {
    fn new(delay_ms: u64) -> (Arc<Self>, Arc<Mutex<Vec<String>>>) {
        let scraped = Arc::new(Mutex::new(Vec::new()));
        let fake = Arc::new(Fake {
            scraped: scraped.clone(),
            delay: Duration::from_millis(delay_ms),
        });
        (fake, scraped)
    }

    async fn get(&self, url: &str) -> Result<Document, ApiError> {
        tokio::time::sleep(self.delay).await;
        self.scraped.lock().unwrap().push(url.to_string());
        if url.contains("blocked") {
            return Err(ApiError::new(ErrorCode::RobotsDisallowed, "disallowed"));
        }
        Ok(Document {
            markdown: Some(format!("# {url}")),
            metadata: Metadata {
                source_url: Some(url.to_string()),
                status_code: 200,
                proxy_used: "basic".into(),
                ..Default::default()
            },
            ..Default::default()
        })
    }
}

fn page_url(i: usize) -> String {
    format!("{BASE}/p/{i}")
}

impl Engine for Fake {
    fn scrape<'a>(
        &'a self,
        url: &'a str,
        _opts: &'a ScrapeOptions,
    ) -> BoxFuture<'a, Result<Document, ApiError>> {
        Box::pin(self.get(url))
    }

    fn crawl<'a>(
        &'a self,
        req: &'a CrawlRequest,
        sink: Arc<dyn CrawlSink>,
    ) -> BoxFuture<'a, Result<(), ApiError>> {
        Box::pin(async move {
            let mut seen: HashSet<String> = HashSet::new();
            let mut queue: VecDeque<(String, usize)> = VecDeque::new();
            let mut done = 0;
            if let Some(r) = sink.resume() {
                seen.extend(r.enqueued);
                queue.extend(r.pending);
                done = r.done;
            } else {
                seen.insert(page_url(0));
                queue.push_back((page_url(0), 0));
                if sink.tracks_frontier() {
                    sink.enqueued(vec![(page_url(0), 0)]).await;
                }
            }
            while done < req.limit {
                if sink.cancelled().await {
                    return Ok(());
                }
                let Some((url, depth)) = queue.pop_front() else {
                    break;
                };
                let result = self.get(&url).await;
                let i: usize = url.rsplit('/').next().unwrap().parse().unwrap();
                let mut new = Vec::new();
                for c in [2 * i + 1, 2 * i + 2] {
                    let u = page_url(c);
                    if done + 1 + queue.len() < req.limit && seen.insert(u.clone()) {
                        queue.push_back((u.clone(), depth + 1));
                        new.push((u, depth + 1));
                    }
                }
                if sink.tracks_frontier() && !new.is_empty() {
                    sink.enqueued(new).await;
                }
                match result {
                    Ok(doc) => sink.page(doc).await,
                    Err(e) => sink.error(&url, e).await,
                }
                done += 1;
                sink.total((done + queue.len()).min(req.limit)).await;
            }
            if req.query.is_some() {
                sink.summary(CrawlSummary {
                    stop_reason: "limit_reached".into(),
                    coverage: None,
                    term_evidence: Default::default(),
                    pages: done,
                })
                .await;
            }
            Ok(())
        })
    }
}

fn store(dir: &std::path::Path) -> pagesift_store::Store {
    let mut c = pagesift_store::Config::new(dir);
    c.background = false;
    pagesift_store::Store::open(c).unwrap()
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
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
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, headers, v)
}

/// 轮询任务状态直到满足条件（最多约 10 秒）。带上不限额的 key，不鉴权的服务会忽略它。
async fn wait(app: &axum::Router, path: &str, cond: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..1000 {
        let (_, _, v) = call(app, "GET", path, None, Some("plain")).await;
        if cond(&v) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting on {path}");
}

/// 一页一页取全部文档的 sourceURL。
async fn all_sources(app: &axum::Router, path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut next = Some(path.to_string());
    while let Some(p) = next {
        let (_, _, v) = call(app, "GET", &p, None, None).await;
        for d in v["data"].as_array().unwrap() {
            out.push(d["metadata"]["sourceURL"].as_str().unwrap().to_string());
        }
        next = v["next"]
            .as_str()
            .map(|n| n.trim_start_matches("http://pagesift.test").to_string());
    }
    out
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn persisted(engine: Arc<Fake>, dir: &std::path::Path) -> axum::Router {
    router(
        engine,
        Config {
            public_url: Some("http://pagesift.test".into()),
            store: Some(store(dir)),
            ..Default::default()
        },
    )
}

/// 爬到一半“崩溃”（直接丢掉运行时，进行中的任务全部中止），重启后从断点继续：
/// 已完成的页面不再抓，队列和去重表恢复，最后的结果不重不漏；再重启一次，结束的任务照样能查。
#[test]
fn crawl_resumes_after_restart_and_results_survive() {
    let dir = tempfile::tempdir().unwrap();
    let limit = 150;
    let (fake, scraped) = Fake::new(4);

    let rt = runtime();
    let (id, before) = rt.block_on(async {
        let app = persisted(fake.clone(), dir.path());
        let (status, _, v) = call(
            &app,
            "POST",
            "/v2/crawl",
            Some(json!({"url": format!("{BASE}/p/0"), "limit": limit})),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        let id = v["id"].as_str().unwrap().to_string();
        let path = format!("/v2/crawl/{id}");
        let v = wait(&app, &path, |v| v["completed"].as_u64().unwrap() >= 40).await;
        assert_eq!(v["status"], "scraping");
        (id, all_sources(&app, &path).await)
    });
    drop(rt);
    let done_before: HashSet<String> = before.iter().cloned().collect();
    assert!(done_before.len() >= 40 && done_before.len() < limit);
    let crashed_at = scraped.lock().unwrap().len();

    let path = format!("/v2/crawl/{id}");
    let rt = runtime();
    rt.block_on(async {
        let app = persisted(fake.clone(), dir.path());
        let v = wait(&app, &path, |v| v["status"] == "completed").await;
        assert_eq!(v["completed"], limit);
        assert_eq!(v["total"], limit);
        let sources = all_sources(&app, &path).await;
        let unique: HashSet<&String> = sources.iter().collect();
        assert_eq!(unique.len(), limit, "no page missing or duplicated");
        assert_eq!(sources.len(), limit);
        let expected: HashSet<String> = (0..limit).map(page_url).collect();
        assert_eq!(
            sources.iter().cloned().collect::<HashSet<_>>(),
            expected,
            "BFS order kept"
        );
    });
    drop(rt);
    let after: Vec<String> = scraped.lock().unwrap()[crashed_at..].to_vec();
    for u in &after {
        assert!(
            !done_before.contains(u),
            "{u} was scraped again after the restart"
        );
    }
    // 中断时正在抓的页面没完成，会重抓；其余的都是新页面
    assert!(after.len() <= limit - done_before.len() + 1);

    let rt = runtime();
    rt.block_on(async {
        let app = persisted(fake.clone(), dir.path());
        let (status, _, v) = call(&app, "GET", &path, None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["status"], "completed");
        assert_eq!(all_sources(&app, &path).await.len(), limit);
        let (_, _, active) = call(&app, "GET", "/v2/crawl/active", None, None).await;
        assert_eq!(active["crawls"], json!([]));
    });
    assert_eq!(
        scraped.lock().unwrap().len(),
        crashed_at + after.len(),
        "nothing re-run"
    );
}

#[test]
fn batch_resumes_after_restart_and_keeps_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut urls: Vec<String> = (0..60).map(|i| format!("{BASE}/b/{i}")).collect();
    urls.push(format!("{BASE}/blocked"));
    let (fake, scraped) = Fake::new(5);

    let rt = runtime();
    let id = rt.block_on(async {
        let app = persisted(fake.clone(), dir.path());
        let (_, _, v) = call(
            &app,
            "POST",
            "/v2/batch/scrape",
            Some(json!({"urls": urls, "maxConcurrency": 2})),
            None,
        )
        .await;
        let id = v["id"].as_str().unwrap().to_string();
        wait(&app, &format!("/v2/batch/scrape/{id}"), |v| {
            v["completed"].as_u64().unwrap() >= 10
        })
        .await;
        id
    });
    drop(rt);
    let crashed_at = scraped.lock().unwrap().len();

    let rt = runtime();
    rt.block_on(async {
        let app = persisted(fake.clone(), dir.path());
        let path = format!("/v2/batch/scrape/{id}");
        let v = wait(&app, &path, |v| v["status"] == "completed").await;
        assert_eq!(
            (v["completed"].clone(), v["total"].clone()),
            (json!(61), json!(61))
        );
        let sources = all_sources(&app, &path).await;
        assert_eq!(sources.iter().collect::<HashSet<_>>().len(), 60);
        assert_eq!(sources.len(), 60);
        let (_, _, errs) = call(&app, "GET", &format!("{path}/errors"), None, None).await;
        assert_eq!(errs["robotsBlocked"], json!([format!("{BASE}/blocked")]));
        assert_eq!(errs["errors"][0]["code"], "ROBOTS_DISALLOWED");
    });
    // 中断前完成的不重抓：重启后最多多抓并发数那么几个
    let total = scraped.lock().unwrap().len();
    assert!(total <= 61 + 2, "{total} scrapes for 61 URLs");
    assert!(crashed_at >= 10);
}

#[tokio::test]
async fn crawl_summary_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, _) = Fake::new(0);
    let expected = json!({"stopReason": "limit_reached", "pages": 3});
    let id = {
        let app = persisted(fake.clone(), dir.path());
        let (_, _, v) = call(
            &app,
            "POST",
            "/v2/crawl",
            Some(json!({"url": format!("{BASE}/p/0"), "limit": 3, "query": "anything"})),
            None,
        )
        .await;
        let id = v["id"].as_str().unwrap().to_string();
        let done = wait(&app, &format!("/v2/crawl/{id}"), |v| {
            v["status"] == "completed"
        })
        .await;
        assert_eq!(done["pagesift"], expected);
        id
    };
    let app = persisted(fake, dir.path());
    let (_, _, v) = call(&app, "GET", &format!("/v2/crawl/{id}"), None, None).await;
    assert_eq!(v["status"], "completed");
    assert_eq!(v["pagesift"], expected, "{v}");
}

#[tokio::test]
async fn cancelled_jobs_stay_cancelled_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, _) = Fake::new(20);
    let id = {
        let app = persisted(fake.clone(), dir.path());
        let (_, _, v) = call(
            &app,
            "POST",
            "/v2/crawl",
            Some(json!({"url": format!("{BASE}/p/0"), "limit": 1000})),
            None,
        )
        .await;
        let id = v["id"].as_str().unwrap().to_string();
        let (status, _, _) = call(&app, "DELETE", &format!("/v2/crawl/{id}"), None, None).await;
        assert_eq!(status, StatusCode::OK);
        // 等爬取任务看到取消、放开存储，再“重启”
        tokio::time::sleep(Duration::from_millis(100)).await;
        id
    };
    let app = persisted(fake, dir.path());
    let (_, _, v) = call(&app, "GET", &format!("/v2/crawl/{id}"), None, None).await;
    assert_eq!(v["status"], "cancelled");
}

/// webhook 接收端：前 `fail_first` 次返回 500，之后 200；记下收到的每个请求。
async fn receiver(fail_first: usize) -> (String, Arc<Mutex<Vec<(HeaderMap, Value, u16)>>>) {
    let got: Arc<Mutex<Vec<(HeaderMap, Value, u16)>>> = Arc::default();
    let g = got.clone();
    let app = axum::Router::new().route(
        "/hook",
        axum::routing::post(move |headers: HeaderMap, body: String| {
            let g = g.clone();
            async move {
                let mut g = g.lock().unwrap();
                let status = if g.len() < fail_first { 500 } else { 200 };
                g.push((headers, serde_json::from_str(&body).unwrap(), status));
                StatusCode::from_u16(status).unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/hook"), got)
}

fn hook_settings(dns: Option<Arc<dyn pagesift_net::DnsLookup>>) -> WebhookSettings {
    let mut w = WebhookSettings {
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(40),
        max_attempts: 4,
        ..Default::default()
    };
    // 测试的接收端在本机：单独放行回环地址，其余内网地址照样拒绝
    w.ssrf.allow_ips = vec!["127.0.0.1".parse().unwrap()];
    if let Some(d) = dns {
        w.dns = d;
    }
    w
}

async fn metrics(app: &axum::Router) -> String {
    let (_, _, m) = call(app, "GET", "/metrics", None, None).await;
    m.as_str().unwrap().to_string()
}

#[tokio::test]
async fn webhooks_deliver_in_order_with_retries() {
    let (url, got) = receiver(2).await;
    let (fake, _) = Fake::new(1);
    let mut settings = hook_settings(None);
    settings.secret = Some("hook-key".into());
    let app = router(
        fake,
        Config {
            webhook: settings,
            ..Default::default()
        },
    );
    let (status, _, v) = call(
        &app,
        "POST",
        "/v2/batch/scrape",
        Some(json!({
            "urls": [format!("{BASE}/a"), format!("{BASE}/b"), format!("{BASE}/blocked")],
            "maxConcurrency": 1,
            "webhook": {"url": url, "headers": {"X-Hook-Secret": "s3"}, "metadata": {"run": "7"}},
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    for _ in 0..500 {
        let done = got
            .lock()
            .unwrap()
            .iter()
            .any(|(_, b, s)| *s == 200 && b["type"] == "batch_scrape.completed");
        if done {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let got = got.lock().unwrap().clone();
    // 前两次投递失败（started 重试了两次），之后按顺序成功
    assert_eq!(got.len(), 2 + 4, "{got:?}");
    let delivered: Vec<&Value> = got
        .iter()
        .filter(|(_, _, s)| *s == 200)
        .map(|(_, b, _)| b)
        .collect();
    let types: Vec<&str> = delivered
        .iter()
        .map(|b| b["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        [
            "batch_scrape.started",
            "batch_scrape.page",
            "batch_scrape.page",
            "batch_scrape.completed"
        ]
    );
    for (h, b, _) in &got {
        assert_eq!(h["x-hook-secret"], "s3");
        assert_eq!(h["content-type"], "application/json");
        let sig = h["x-firecrawl-signature"].to_str().unwrap();
        assert!(sig.starts_with("sha256=") && sig.len() == 7 + 64, "{sig}");
        assert_eq!(b["id"], id.as_str());
        assert_eq!(b["metadata"], json!({"run": "7"}));
    }
    let pages: HashSet<&str> = delivered[1..3]
        .iter()
        .map(|b| b["data"][0]["markdown"].as_str().unwrap())
        .collect();
    assert_eq!(
        pages,
        HashSet::from([
            format!("# {BASE}/a").as_str(),
            format!("# {BASE}/b").as_str()
        ])
    );
    assert_eq!(delivered[1]["success"], true);
    assert_eq!(delivered[3]["data"], json!([]));
    // The receiver records the event before sending its HTTP response. Seeing
    // `completed` there does not yet mean the sender has observed the ACK and
    // incremented its delivery counter.
    let mut m = metrics(&app).await;
    for _ in 0..100 {
        if m.contains("pagesift_webhook_deliveries_total{outcome=\"delivered\"} 4") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        m = metrics(&app).await;
    }
    assert!(
        m.contains("pagesift_webhook_deliveries_total{outcome=\"retried\"} 2"),
        "{m}"
    );
    assert!(
        m.contains("pagesift_webhook_deliveries_total{outcome=\"delivered\"} 4"),
        "{m}"
    );
}

#[tokio::test]
async fn webhook_gives_up_after_max_attempts_without_blocking_the_job() {
    let (url, got) = receiver(usize::MAX).await;
    let (fake, _) = Fake::new(1);
    let app = router(
        fake,
        Config {
            webhook: hook_settings(None),
            ..Default::default()
        },
    );
    let (_, _, v) = call(
        &app,
        "POST",
        "/v2/crawl",
        Some(json!({"url": format!("{BASE}/p/0"), "limit": 5,
                    "webhook": {"url": url, "events": ["completed"]}})),
        None,
    )
    .await;
    let id = v["id"].as_str().unwrap().to_string();
    // 任务不等 webhook
    wait(&app, &format!("/v2/crawl/{id}"), |v| {
        v["status"] == "completed"
    })
    .await;
    for _ in 0..500 {
        if metrics(&app).await.contains("outcome=\"failed\"} 1") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let got = got.lock().unwrap();
    assert_eq!(
        got.len(),
        4,
        "max_attempts deliveries, only the completed event"
    );
    assert!(got.iter().all(|(_, b, _)| b["type"] == "crawl.completed"));
}

/// 把所有域名都解析到一个内网地址。
struct PrivateDns;

impl pagesift_net::DnsLookup for PrivateDns {
    fn lookup<'a>(
        &'a self,
        _host: &'a str,
    ) -> pagesift_net::BoxFuture<'a, std::io::Result<Vec<IpAddr>>> {
        Box::pin(async { Ok(vec!["10.0.0.7".parse().unwrap()]) })
    }
}

#[tokio::test]
async fn webhooks_to_private_addresses_are_refused() {
    let (fake, _) = Fake::new(1);
    let app = router(fake.clone(), Config::default());
    for url in [
        "http://127.0.0.1:9/hook",
        "http://169.254.169.254/latest/meta-data",
        "http://[::1]/hook",
        "http://0x7f000001/hook",
        "http://hooks.internal/x",
        "ftp://hooks.example.com/x",
    ] {
        for path in ["/v2/crawl", "/v2/batch/scrape"] {
            let body = json!({"url": format!("{BASE}/p/0"), "urls": [format!("{BASE}/a")], "webhook": url});
            let (status, _, v) = call(&app, "POST", path, Some(body), None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{url} {path}");
            assert_eq!(
                (v["success"].clone(), v["code"].as_str()),
                (json!(false), Some("SSRF_BLOCKED")),
                "{url}"
            );
        }
    }

    // 域名本身没问题，但解析到内网：投递时拦下，不连接、不重试
    let (receiver_url, got) = receiver(0).await;
    let port = receiver_url.rsplit(':').next().unwrap();
    let app = router(
        fake,
        Config {
            webhook: hook_settings(Some(Arc::new(PrivateDns))),
            ..Default::default()
        },
    );
    let (status, _, _) = call(
        &app,
        "POST",
        "/v2/batch/scrape",
        Some(json!({"urls": [format!("{BASE}/a")], "webhook": format!("http://hooks.example.com:{port}")})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let mut m = String::new();
    for _ in 0..500 {
        m = metrics(&app).await;
        if m.contains("outcome=\"blocked\"} 3") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        m.contains("pagesift_webhook_deliveries_total{outcome=\"blocked\"} 3"),
        "{m}"
    );
    assert!(!m.contains("outcome=\"retried\""), "{m}");
    assert!(got.lock().unwrap().is_empty());
}

fn limited(limits: KeyLimits, engine: Arc<Fake>, dir: Option<&std::path::Path>) -> axum::Router {
    router(
        engine,
        Config {
            api_keys: vec!["plain".into()],
            key_limits: HashMap::from([("limited".to_string(), limits)]),
            store: dir.map(store),
            ..Default::default()
        },
    )
}

#[tokio::test]
async fn requests_per_minute_returns_firecrawl_429() {
    let (fake, _) = Fake::new(0);
    let app = limited(
        KeyLimits {
            requests_per_minute: Some(2),
            ..Default::default()
        },
        fake,
        None,
    );
    let body = json!({"url": format!("{BASE}/a")});
    for _ in 0..2 {
        let (status, _, _) = call(
            &app,
            "POST",
            "/v2/scrape",
            Some(body.clone()),
            Some("limited"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, headers, v) = call(
        &app,
        "POST",
        "/v2/scrape",
        Some(body.clone()),
        Some("limited"),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["success"], false);
    assert_eq!(v["code"], "RATE_LIMIT_EXCEEDED");
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("2 requests per minute")
    );
    let retry: u64 = headers["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((1..=30).contains(&retry), "{retry}");
    // 轮询类请求不计数；没设限额的 key 不受影响；鉴权照旧
    let (status, _, _) = call(&app, "GET", "/v2/crawl/active", None, Some("limited")).await;
    assert_eq!(status, StatusCode::OK);
    for _ in 0..5 {
        let (status, _, _) = call(
            &app,
            "POST",
            "/v2/scrape",
            Some(body.clone()),
            Some("plain"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, _, _) = call(&app, "POST", "/v2/scrape", Some(body), Some("nope")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let m = metrics(&app).await;
    assert!(
        m.contains("pagesift_errors_total{code=\"RATE_LIMIT_EXCEEDED\",source=\"http\"} 1"),
        "{m}"
    );
}

#[tokio::test]
async fn concurrent_jobs_are_capped_per_key() {
    let (fake, _) = Fake::new(30);
    let app = limited(
        KeyLimits {
            max_concurrent_jobs: Some(1),
            ..Default::default()
        },
        fake,
        None,
    );
    let urls: Vec<String> = (0..6).map(|i| format!("{BASE}/c/{i}")).collect();
    let body = json!({"urls": urls, "maxConcurrency": 1});
    let (status, _, v) = call(
        &app,
        "POST",
        "/v2/batch/scrape",
        Some(body.clone()),
        Some("limited"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = v["id"].as_str().unwrap().to_string();
    let (status, _, v) = call(
        &app,
        "POST",
        "/v2/crawl",
        Some(json!({"url": format!("{BASE}/p/0")})),
        Some("limited"),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["code"], "CONCURRENCY_LIMIT_EXCEEDED");
    // 别的 key 不受影响
    let (status, _, _) = call(
        &app,
        "POST",
        "/v2/batch/scrape",
        Some(body.clone()),
        Some("plain"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // 第一个任务结束后名额空出来
    wait(&app, &format!("/v2/batch/scrape/{id}"), |v| {
        v["status"] == "completed"
    })
    .await;
    let (status, _, _) = call(
        &app,
        "POST",
        "/v2/batch/scrape",
        Some(body),
        Some("limited"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn daily_page_quota_stops_jobs_and_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, scraped) = Fake::new(0);
    let limits = KeyLimits {
        name: Some("team-q".into()),
        pages_per_day: Some(5),
        ..Default::default()
    };
    {
        let app = limited(limits.clone(), fake.clone(), Some(dir.path()));
        for i in 0..2 {
            let (status, _, _) = call(
                &app,
                "POST",
                "/v2/scrape",
                Some(json!({"url": format!("{BASE}/q/{i}")})),
                Some("limited"),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }
        // 剩 3 页：10 个 URL 的批量任务做完 3 页就停，记一条 QUOTA_EXCEEDED
        let urls: Vec<String> = (0..10).map(|i| format!("{BASE}/qb/{i}")).collect();
        let (status, _, v) = call(
            &app,
            "POST",
            "/v2/batch/scrape",
            Some(json!({"urls": urls, "maxConcurrency": 1})),
            Some("limited"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let path = format!("/v2/batch/scrape/{}", v["id"].as_str().unwrap());
        let v = wait(&app, &path, |v| v["status"] == "completed").await;
        assert_eq!(v["data"].as_array().unwrap().len(), 3);
        let (_, _, errs) = call(
            &app,
            "GET",
            &format!("{path}/errors"),
            None,
            Some("limited"),
        )
        .await;
        assert_eq!(errs["errors"][0]["code"], "QUOTA_EXCEEDED", "{errs}");
    }
    assert_eq!(scraped.lock().unwrap().len(), 5);
    // 重启后配额不清零
    let app = limited(limits, fake, Some(dir.path()));
    let (status, headers, v) = call(
        &app,
        "POST",
        "/v2/scrape",
        Some(json!({"url": format!("{BASE}/q/9")})),
        Some("limited"),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["code"], "QUOTA_EXCEEDED");
    assert!(headers.contains_key("retry-after"));
    let (status, _, v) = call(
        &app,
        "POST",
        "/v2/crawl",
        Some(json!({"url": format!("{BASE}/p/0")})),
        Some("limited"),
    )
    .await;
    assert_eq!(
        (status, v["code"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("QUOTA_EXCEEDED"))
    );
    let (status, _, _) = call(
        &app,
        "POST",
        "/v2/scrape",
        Some(json!({"url": format!("{BASE}/q/9")})),
        Some("plain"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unlimited keys are unaffected");
}

#[tokio::test]
async fn metrics_cover_latency_jobs_pages_and_errors() {
    let (fake, _) = Fake::new(0);
    let app = router(fake, Config::default());
    call(
        &app,
        "POST",
        "/v2/scrape",
        Some(json!({"url": format!("{BASE}/a")})),
        None,
    )
    .await;
    call(
        &app,
        "POST",
        "/v2/scrape",
        Some(json!({"url": format!("{BASE}/blocked")})),
        None,
    )
    .await;
    call(
        &app,
        "POST",
        "/v2/scrape",
        Some(json!({"url": "ftp://x"})),
        None,
    )
    .await;
    let (_, _, v) = call(
        &app,
        "POST",
        "/v2/crawl",
        Some(json!({"url": format!("{BASE}/p/0"), "limit": 7})),
        None,
    )
    .await;
    wait(
        &app,
        &format!("/v2/crawl/{}", v["id"].as_str().unwrap()),
        |v| v["status"] == "completed",
    )
    .await;
    let m = metrics(&app).await;
    for want in [
        "pagesift_http_request_duration_seconds_count{route=\"/v2/scrape\"} 3",
        "pagesift_http_request_duration_seconds_bucket{route=\"/v2/scrape\",le=\"+Inf\"} 3",
        "pagesift_http_requests_total{route=\"/v2/scrape\",status=\"403\"} 1",
        "pagesift_http_requests_in_flight 1",
        "pagesift_pages_scraped_total{source=\"scrape\"} 1",
        "pagesift_pages_scraped_total{source=\"crawl\"} 7",
        "pagesift_errors_total{code=\"ROBOTS_DISALLOWED\",source=\"http\"} 1",
        "pagesift_errors_total{code=\"INVALID_REQUEST\",source=\"http\"} 1",
        "pagesift_jobs_started_total{kind=\"crawl\"} 1",
        "pagesift_jobs{kind=\"crawl\",status=\"completed\"} 1",
    ] {
        assert!(m.contains(want), "missing {want}\n{m}");
    }
}

/// Simulate the exact durable boundary after result append and before done-set
/// insertion. Batch identity must remain positional even when URLs are equal.
#[test]
fn saved_completion_receipts_repair_missing_done_marks() {
    for batch in [false, true] {
        for failed in [false, true] {
            for legacy in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let id = if batch {
                    "receipt-batch"
                } else {
                    "receipt-crawl"
                };
                let url = page_url(0);
                let req: CrawlRequest =
                    serde_json::from_value(json!({"url":url,"limit":2})).unwrap();
                let created = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64;
                {
                    let s = store(dir.path());
                    let t = s.tasks();
                    let meta = json!({"kind":if batch {"batch_scrape"} else {"crawl"},"state":"scraping",
                "created_ms":created,"total":2,"completed":0,"owner":null,"webhook":null,
                "crawl":if batch {Value::Null} else {serde_json::to_value(&req).unwrap()},
                "batch":if batch {json!({"urls":[url,url],"options":{},"concurrency":1})} else {Value::Null}});
                    t.kv_put(format!("job/{id}"), &serde_json::to_vec(&meta).unwrap())
                        .unwrap();
                    let mut doc = serde_json::to_value(Document {
                        markdown: Some("Already delivered".into()),
                        metadata: pagesift_api::types::Metadata {
                            source_url: Some(url.clone()),
                            ..Default::default()
                        },
                        ..Default::default()
                    })
                    .unwrap();
                    if failed {
                        doc = json!({"id":"saved-error", "timestamp":"2026-10-22T00:00:00Z", "url":url,
                    "code":"UPSTREAM_ERROR", "error":"previously delivered failure"});
                    }
                    let done = if batch { "0".to_string() } else { url.clone() };
                    if legacy {
                        t.set_insert(format!("job/{id}/done"), &done).unwrap();
                    } else {
                        doc["_pagesiftDone"] = json!(done);
                    }
                    t.list_push(
                        format!("job/{id}/{}", if failed { "errors" } else { "pages" }),
                        &serde_json::to_vec(&doc).unwrap(),
                    )
                    .unwrap();
                    if !batch {
                        for u in [url.clone(), page_url(1)] {
                            t.list_push(
                                format!("job/{id}/frontier"),
                                &serde_json::to_vec(&(u, 0usize)).unwrap(),
                            )
                            .unwrap();
                        }
                    }
                }
                let (fake, scraped) = Fake::new(1);
                runtime().block_on(async {
                    let app = persisted(fake, dir.path());
                    let path = format!("/v2/{}/{id}", if batch { "batch/scrape" } else { "crawl" });
                    let v = wait(&app, &path, |v| v["status"] == "completed").await;
                    assert_eq!(v["completed"], 2, "{v}");
                    let sources = all_sources(&app, &path).await;
                    assert_eq!(sources.len(), 2 - usize::from(failed));
                    let (_, _, errors) =
                        call(&app, "GET", &format!("{path}/errors"), None, None).await;
                    assert_eq!(
                        errors["errors"].as_array().unwrap().len(),
                        usize::from(failed)
                    );
                    assert!(!errors.to_string().contains("_pagesiftDone"));
                    if !batch {
                        assert_eq!(
                            sources.iter().cloned().collect::<HashSet<_>>(),
                            if failed {
                                [page_url(1)].into_iter().collect()
                            } else {
                                [url.clone(), page_url(1)].into_iter().collect()
                            }
                        );
                    }
                    assert_eq!(
                        scraped.lock().unwrap().len(),
                        1,
                        "already persisted result must not run again"
                    );
                    assert!(
                        !v.to_string().contains("_pagesiftDone"),
                        "private receipt leaked to API"
                    );
                });
            }
        }
    }
}
