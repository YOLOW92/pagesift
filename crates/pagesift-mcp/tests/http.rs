//! 用 reqwest 走 Streamable HTTP：新版无状态请求、旧版会话（含 GET SSE 和 DELETE）、安全检查。

use std::net::SocketAddr;
use std::time::Duration;

use pagesift_mcp::{HttpConfig, McpServer, tools};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tokio::sync::oneshot;

struct Srv {
    url: String,
    addr: SocketAddr,
    _stop: oneshot::Sender<()>,
}

async fn start() -> Srv {
    let server = McpServer::builder("pagesift", "test")
        .tools(tools::placeholder_tools())
        .build()
        .unwrap();
    let config = HttpConfig {
        sse_keep_alive: Duration::from_millis(100),
        ..HttpConfig::default()
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        pagesift_mcp::serve_listener(server, &config, listener, async {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    Srv {
        url: format!("http://{addr}/mcp"),
        addr,
        _stop: tx,
    }
}

const ACCEPT_BOTH: &str = "application/json, text/event-stream";

fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": { "name": "e2e", "version": "1" },
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

/// 按新版规范带上镜像头。
fn modern_post(c: &Client, url: &str, body: &Value) -> reqwest::RequestBuilder {
    modern_post_named(c, url, body, body["params"]["name"].as_str())
}

fn modern_post_named(
    c: &Client,
    url: &str,
    body: &Value,
    name: Option<&str>,
) -> reqwest::RequestBuilder {
    let mut rb = c
        .post(url)
        .header("content-type", "application/json")
        .header("accept", ACCEPT_BOTH)
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", body["method"].as_str().unwrap());
    if let Some(name) = name {
        rb = rb.header("mcp-name", name);
    }
    rb.body(body.to_string())
}

fn version_header(v: &'static str) -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert(
        "mcp-protocol-version",
        reqwest::header::HeaderValue::from_static(v),
    );
    h
}

async fn json_of(resp: reqwest::Response) -> Value {
    let ct = resp.headers()["content-type"].to_str().unwrap().to_owned();
    assert!(ct.starts_with("application/json"), "{ct}");
    serde_json::from_slice(&resp.bytes().await.unwrap()).unwrap()
}

/// 读完一个 SSE 响应，返回每个 `data:` 里的 JSON。
async fn sse_events(mut resp: reqwest::Response) -> Vec<Value> {
    let ct = resp.headers()["content-type"].to_str().unwrap().to_owned();
    assert!(ct.starts_with("text/event-stream"), "{ct}");
    let mut text = String::new();
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(10), resp.chunk())
        .await
        .unwrap()
        .unwrap()
    {
        text.push_str(std::str::from_utf8(&chunk).unwrap());
    }
    parse_sse(&text)
}

fn parse_sse(text: &str) -> Vec<Value> {
    text.split("\n\n")
        .filter_map(|block| {
            let data: Vec<&str> = block
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(str::trim_start)
                .collect();
            (!data.is_empty()).then(|| serde_json::from_str(&data.join("\n")).unwrap())
        })
        .collect()
}

#[tokio::test]
async fn modern_stateless_flow() {
    let s = start().await;
    let c = Client::new();

    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": { "_meta": meta() } });
    let resp = modern_post(&c, &s.url, &body).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get("mcp-session-id").is_none());
    let v = json_of(resp).await;
    assert_eq!(v["result"]["resultType"], "complete");

    let body =
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": { "_meta": meta() } });
    let v = json_of(modern_post(&c, &s.url, &body).send().await.unwrap()).await;
    assert_eq!(v["result"]["tools"].as_array().unwrap().len(), 5);

    // 没有 progressToken：直接返回 JSON。
    let body = json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "pagesift_crawl", "arguments": { "url": "https://example.com", "limit": 5 }, "_meta": meta() } });
    let v = json_of(modern_post(&c, &s.url, &body).send().await.unwrap()).await;
    assert_eq!(v["result"]["structuredContent"]["id"], "placeholder-crawl");

    // 有 progressToken：SSE，先进度后响应，响应后流结束。
    let mut m = meta();
    m["progressToken"] = json!(7);
    let body = json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": { "name": "pagesift_scrape", "arguments": { "url": "https://example.com" }, "_meta": m } });
    let resp = modern_post(&c, &s.url, &body).send().await.unwrap();
    assert_eq!(resp.headers()["x-accel-buffering"], "no");
    let events = sse_events(resp).await;
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(events[0]["method"], "notifications/progress");
    assert_eq!(events[0]["params"]["progressToken"], 7);
    assert_eq!(events[2]["id"], 4);
    assert_eq!(events[2]["result"]["isError"], false);

    // 工具执行错误：isError 结果，HTTP 200。
    let body = json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": { "name": "pagesift_crawl_status", "arguments": { "id": "missing" }, "_meta": meta() } });
    let v = json_of(modern_post(&c, &s.url, &body).send().await.unwrap()).await;
    assert_eq!(v["result"]["isError"], true);
    assert_eq!(v["result"]["structuredContent"]["code"], "INVALID_REQUEST");
}

#[tokio::test]
async fn modern_validation_errors() {
    let s = start().await;
    let c = Client::new();
    let call = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "pagesift_map", "arguments": { "url": "https://example.com" }, "_meta": meta() } });

    // 头和请求体不一致：400 + -32020。
    for bad in [Some("other"), None] {
        let resp = modern_post_named(&c, &s.url, &call, bad)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json_of(resp).await["error"]["code"], -32020);
    }
    // 同一个头出现两次也要拒绝。
    let resp = modern_post(&c, &s.url, &call)
        .header("mcp-name", "pagesift_map")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    // Base64 编码的 Mcp-Name 解码后一致即可。
    let resp = modern_post_named(&c, &s.url, &call, Some("=?base64?cGFnZXNpZnRfbWFw?="))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = c
        .post(&s.url)
        .header("content-type", "application/json")
        .header("accept", ACCEPT_BOTH)
        .body(call.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_of(resp).await["error"]["code"], -32020);

    // 不支持的版本：400 + -32022。
    let mut m = meta();
    m["io.modelcontextprotocol/protocolVersion"] = json!("2099-01-01");
    let body =
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": { "_meta": m } });
    let resp = modern_post(&c, &s.url, &body)
        .headers(version_header("2099-01-01"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_of(resp).await["error"]["code"], -32022);

    // 不存在的方法：404 + -32601。
    let body = json!({ "jsonrpc": "2.0", "id": 3, "method": "prompts/list", "params": { "_meta": meta() } });
    let resp = modern_post(&c, &s.url, &body).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_of(resp).await["error"]["code"], -32601);

    // 缺 clientCapabilities：400 + -32602。
    let body = json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/list",
        "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" } } });
    let resp = modern_post(&c, &s.url, &body).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_of(resp).await["error"]["code"], -32602);

    // 非法 JSON、批量、错误的 Content-Type、Accept。
    let post = |b: &'static str| {
        c.post(&s.url)
            .header("content-type", "application/json")
            .header("accept", ACCEPT_BOTH)
            .body(b)
    };
    let resp = post("{oops").send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_of(resp).await["error"]["code"], -32700);
    let resp = post(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_of(resp).await["error"]["code"], -32600);
    let resp = c
        .post(&s.url)
        .header("content-type", "text/plain")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let resp = c
        .post(&s.url)
        .header("content-type", "application/json")
        .header("accept", "text/html")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_ACCEPTABLE);
    // 新版下 GET/DELETE 没有意义。
    assert_eq!(
        c.get(&s.url).send().await.unwrap().status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        c.delete(&s.url).send().await.unwrap().status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    // 通知：202。
    let resp = post(r#"{"jsonrpc":"2.0","method":"notifications/whatever"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn legacy_session_flow() {
    let s = start().await;
    let c = Client::new();
    let post = |body: Value, sid: Option<&str>| {
        let mut rb = c
            .post(&s.url)
            .header("content-type", "application/json")
            .header("accept", ACCEPT_BOTH)
            .body(body.to_string());
        if let Some(sid) = sid {
            rb = rb
                .header("mcp-session-id", sid)
                .header("mcp-protocol-version", "2025-11-25");
        }
        rb
    };

    let resp = post(
        json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": { "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": { "name": "e2e", "version": "1" } } }),
        None,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let sid = resp.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let v = json_of(resp).await;
    assert_eq!(v["result"]["protocolVersion"], "2025-11-25");

    let resp = post(
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        Some(&sid),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    // 没有会话头的旧版请求被拒绝，未知会话 404。
    let resp = post(
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        None,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let resp = post(
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        Some("nope"),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // GET 打开会话的 SSE 流，收到保活注释。
    let mut stream = c
        .get(&s.url)
        .header("accept", "text/event-stream")
        .header("mcp-session-id", &sid)
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    assert!(
        stream.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let first = tokio::time::timeout(Duration::from_secs(5), stream.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(std::str::from_utf8(&first).unwrap().starts_with(':'));

    let v = json_of(
        post(
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            Some(&sid),
        )
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(v["result"]["tools"][4]["name"], "pagesift_batch_scrape");
    assert!(v["result"].get("resultType").is_none());

    let body = json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "pagesift_batch_scrape", "arguments": { "urls": ["https://a.example", "https://b.example"] },
                    "_meta": { "progressToken": "t" } } });
    let events = sse_events(post(body, Some(&sid)).send().await.unwrap()).await;
    let last = events.last().unwrap();
    assert_eq!(last["id"], 3);
    assert_eq!(
        last["result"]["structuredContent"]["id"],
        "placeholder-batch"
    );

    // DELETE 结束会话，GET 流随之关闭。
    let resp = c
        .delete(&s.url)
        .header("mcp-session-id", &sid)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let end = tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(Some(_)) = stream.chunk().await {}
    })
    .await;
    assert!(end.is_ok(), "GET stream should end after DELETE");
    let resp = post(
        json!({ "jsonrpc": "2.0", "id": 4, "method": "ping" }),
        Some(&sid),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn legacy_batch_on_2025_03_26() {
    let s = start().await;
    let c = Client::new();
    let resp = c
        .post(&s.url)
        .header("content-type", "application/json")
        .header("accept", ACCEPT_BOTH)
        .body(
            json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize",
                "params": { "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "e2e", "version": "1" } } })
            .to_string(),
        )
        .send()
        .await
        .unwrap();
    let sid = resp.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let batch = json!([
        { "jsonrpc": "2.0", "method": "notifications/initialized" },
        { "jsonrpc": "2.0", "id": 1, "method": "ping" },
        { "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "pagesift_map", "arguments": { "url": "https://example.com" } } }
    ]);
    let v = json_of(
        c.post(&s.url)
            .header("content-type", "application/json")
            .header("accept", ACCEPT_BOTH)
            .header("mcp-session-id", &sid)
            .body(batch.to_string())
            .send()
            .await
            .unwrap(),
    )
    .await;
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["id"], 1);
    assert_eq!(arr[1]["result"]["isError"], false);
}

#[tokio::test]
async fn dns_rebinding_protection() {
    let s = start().await;
    let c = Client::new();
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": { "_meta": meta() } });

    let resp = modern_post(&c, &s.url, &body)
        .header("origin", "http://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let v = json_of(resp).await;
    assert!(v.get("id").is_none());

    let resp = modern_post(&c, &s.url, &body)
        .header("origin", "http://localhost:6274")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 重绑定攻击时浏览器发出的 Host 是攻击者的域名。
    let port = s.addr.port();
    let resp = modern_post(&c, &s.url, &body)
        .header("host", format!("evil.example:{port}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let resp = modern_post(&c, &s.url, &body)
        .header("host", format!("localhost:{port}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn client_disconnect_cancels_request() {
    // 断开 SSE 连接后服务器继续正常工作（任务被取消，不会泄漏）。
    let s = start().await;
    let c = Client::new();
    let mut m = meta();
    m["progressToken"] = json!("x");
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "pagesift_scrape", "arguments": { "url": "https://example.com" }, "_meta": m } });
    let resp = modern_post(&c, &s.url, &body).send().await.unwrap();
    drop(resp);
    let body =
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": { "_meta": meta() } });
    let resp = modern_post(&c, &s.url, &body).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
