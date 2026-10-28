//! 服务加固：slowloris 防护（请求头/请求体/整请求超时）、参数上限、统一的错误格式。

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use pagesift_api::types::{Document, Metadata, ScrapeOptions};
use pagesift_api::{ApiError, BoxFuture, Config, Engine, Timeouts, router, serve_with};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tower::ServiceExt;

/// 假引擎：URL 里含 `hang` 时一直不返回。
struct Fake;

impl Engine for Fake {
    fn scrape<'a>(
        &'a self,
        url: &'a str,
        _opts: &'a ScrapeOptions,
    ) -> BoxFuture<'a, Result<Document, ApiError>> {
        Box::pin(async move {
            if url.contains("hang") {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
            Ok(Document {
                markdown: Some(format!("# {url}")),
                metadata: Metadata {
                    source_url: Some(url.to_string()),
                    status_code: 200,
                    ..Default::default()
                },
                ..Default::default()
            })
        })
    }
}

/// 测试用的短超时，免得等 30 秒。
fn short() -> Timeouts {
    Timeouts {
        header_read: Duration::from_millis(300),
        body: Duration::from_millis(300),
        upload_body: Duration::from_millis(300),
        request: Duration::from_millis(500),
    }
}

fn app(timeouts: Timeouts) -> axum::Router {
    router(
        Arc::new(Fake),
        Config {
            public_url: Some("http://pagesift.test".into()),
            timeouts,
            ..Default::default()
        },
    )
}

async fn send(
    app: &axum::Router,
    req: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, headers, v)
}

async fn post(app: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::post(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (s, _, v) = send(app, req).await;
    (s, v)
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let (s, _, v) = send(app, Request::get(uri).body(Body::empty()).unwrap()).await;
    (s, v)
}

/// 起一个真实监听的服务，返回地址和关停开关。
async fn spawn_server(
    timeouts: Timeouts,
) -> (
    std::net::SocketAddr,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(serve_with(listener, app(timeouts), timeouts, async move {
        let _ = rx.await;
    }));
    (addr, tx, handle)
}

/// 读到连接关闭为止，返回收到的全部字节。
async fn read_to_close(stream: &mut TcpStream) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return out,
            Ok(n) => out.extend_from_slice(&buf[..n]),
        }
    }
}

fn assert_error(v: &Value, code: &str) {
    assert_eq!(v["success"], false, "{v}");
    assert_eq!(v["code"], code, "{v}");
    assert!(v["error"].as_str().is_some_and(|e| !e.is_empty()), "{v}");
}

#[tokio::test]
async fn slow_headers_are_cut_off() {
    let (addr, tx, handle) = spawn_server(short()).await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\n")
        .await
        .unwrap();
    let (mut rd, mut wr) = stream.into_split();
    // 每 50 ms 滴 1 字节，永远发不完请求头
    let drip = tokio::spawn(async move {
        loop {
            if wr.write_all(b"x").await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    // wall-clock: the server cuts the drip off after its 300 ms header-read timeout; 3 s is ten
    // times
    // that, and the bug (no cut-off) runs into the 5 s client timeout
    let started = Instant::now();
    let mut buf = [0u8; 1024];
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match rd.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "server kept a slowloris connection open");
    assert!(started.elapsed() < Duration::from_secs(3));
    drip.abort();

    // 正常请求不受影响
    let mut ok = TcpStream::connect(addr).await.unwrap();
    ok.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let resp = String::from_utf8(read_to_close(&mut ok).await).unwrap();
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");

    // 优雅关停：发信号后 serve 返回
    tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("serve did not shut down")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn slow_body_gets_408_json() {
    let (addr, _tx, _handle) = spawn_server(short()).await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            b"POST /v2/scrape HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: 200\r\nConnection: close\r\n\r\n{",
        )
        .await
        .unwrap();
    let (mut rd, mut wr) = stream.into_split();
    let drip = tokio::spawn(async move {
        for _ in 0..100 {
            if wr.write_all(b" ").await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    // wall-clock: the server answers 408 after its 300 ms body timeout; 3 s is ten times that, and
    // the bug (no timeout) runs into the 5 s client timeout
    let started = Instant::now();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), rd.read_to_end(&mut out)).await;
    drip.abort();
    assert!(started.elapsed() < Duration::from_secs(3));
    let resp = String::from_utf8_lossy(&out);
    assert!(resp.starts_with("HTTP/1.1 408"), "{resp}");
    let body: Value = serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_error(&body, "TIMEOUT");
}

#[tokio::test]
async fn whole_request_timeout_is_json() {
    let app = app(short());
    let (s, v) = post(
        &app,
        "/v2/scrape",
        json!({"url": "https://example.com/hang"}),
    )
    .await;
    assert_eq!(s, StatusCode::REQUEST_TIMEOUT);
    assert_error(&v, "TIMEOUT");
    // 默认的整请求超时比 scrape timeout 上限大
    let d = Timeouts::default();
    assert!(d.request > Duration::from_millis(pagesift_api::MAX_SCRAPE_TIMEOUT_MS));
    assert_eq!(d.header_read, Duration::from_secs(30));
    assert_eq!(d.body, Duration::from_secs(30));
}

#[tokio::test]
async fn parameters_have_upper_bounds() {
    let app = app(Timeouts::default());
    let long_url = format!("https://example.com/{}", "a".repeat(1 << 20));
    let many: Vec<String> = (0..10_001)
        .map(|i| format!("https://example.com/{i}"))
        .collect();
    for (uri, body, needle) in [
        (
            "/v2/scrape",
            json!({"url": "https://example.com", "timeout": 1_000_000_000_000_000_000u64}),
            "300000",
        ),
        (
            "/v2/scrape",
            json!({"url": "https://example.com", "timeout": 300_001}),
            "300000",
        ),
        (
            "/v2/crawl",
            json!({"url": "https://example.com", "limit": 1_000_000_000_000_000u64}),
            "100000",
        ),
        (
            "/v2/crawl",
            json!({"url": "https://example.com", "maxDepth": 1u64 << 63}),
            "100",
        ),
        (
            "/v2/crawl",
            json!({"url": "https://example.com", "maxDiscoveryDepth": 101}),
            "100",
        ),
        (
            "/v2/crawl",
            json!({"url": "https://example.com", "scrapeOptions": {"timeout": 400_000}}),
            "300000",
        ),
        (
            "/v2/map",
            json!({"url": "https://example.com", "limit": 100_001}),
            "100000",
        ),
        ("/v2/batch/scrape", json!({"urls": many}), "10000"),
        ("/v2/extract", json!({"urls": many, "prompt": "x"}), "10000"),
        ("/v2/scrape", json!({"url": long_url}), "8192"),
        ("/v2/crawl", json!({"url": long_url}), "8192"),
        (
            "/v2/extract",
            json!({"urls": ["https://example.com"], "schema": "not an object"}),
            "schema",
        ),
    ] {
        let (s, v) = post(&app, uri, body.clone()).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{uri} {v}");
        assert_error(&v, "INVALID_REQUEST");
        assert!(v["error"].as_str().unwrap().contains(needle), "{uri} {v}");
    }
    // 上限本身仍然接受，正常请求的响应结构不变
    let (s, v) = post(
        &app,
        "/v2/scrape",
        json!({"url": "https://example.com", "timeout": 300_000}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["success"], true);
    let ok_url = format!("https://example.com/{}", "a".repeat(8000));
    let (s, v) = post(&app, "/v2/scrape", json!({"url": ok_url})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = post(&app, "/v2/crawl", json!({"url": "https://example.com", "limit": 100_000, "maxDiscoveryDepth": 100, "maxDepth": 100})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["id"].is_string());
    let (s, v) = post(
        &app,
        "/v2/extract",
        json!({"urls": ["https://example.com"], "schema": {"type": "object"}}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn huge_skip_does_not_overflow() {
    let app = app(Timeouts::default());
    let (_, v) = post(
        &app,
        "/v2/batch/scrape",
        json!({"urls": ["https://example.com/a"]}),
    )
    .await;
    let id = v["id"].as_str().unwrap().to_string();
    let (s, v) = get(&app, &format!("/v2/batch/scrape/{id}?skip={}", usize::MAX)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["success"], true);
}

#[tokio::test]
async fn framework_rejections_use_the_json_error_format() {
    let app = app(Timeouts::default());
    // 查询参数反序列化失败
    let (s, v) = get(&app, "/v1/crawl/abc?skip=-1").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_error(&v, "INVALID_REQUEST");
    assert!(v["error"].as_str().unwrap().contains("skip"), "{v}");
    // 路径参数不是合法 UTF-8
    let (s, v) = get(&app, "/v2/crawl/%FF%FE").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_error(&v, "INVALID_REQUEST");
    // JSON 解析失败
    let req = Request::post("/v2/scrape")
        .header("content-type", "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    let (s, _, v) = send(&app, req).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_error(&v, "INVALID_REQUEST");
    // Jina 路由的 JSON 提取器缺 Content-Type
    let (s, _, v) = send(&app, Request::post("/r").body(Body::from("{}")).unwrap()).await;
    assert_eq!(s, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_error(&v, "INVALID_REQUEST");
    // 405 带 JSON body，保留 Allow 头
    let (s, h, v) = send(
        &app,
        Request::put("/v2/scrape").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
    assert_error(&v, "INVALID_REQUEST");
    assert!(
        h.get(header::ALLOW)
            .is_some_and(|a| a.to_str().unwrap().contains("POST"))
    );
    assert_eq!(h[header::CONTENT_TYPE], "application/json");
    // 404 照旧
    let (s, v) = get(&app, "/nope").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_error(&v, "NOT_FOUND");
    // 普通文本响应不受影响
    let (s, v) = get(&app, "/healthz").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v, Value::String("ok".into()));
}
