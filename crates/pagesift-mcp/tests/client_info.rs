//! 工具拿到的 `clientInfo`：旧版只在 `initialize` 里带，之后的 `tools/call` 要用会话记住的；
//! 新版每个请求自己带 `_meta`。stdio 和 HTTP 两种传输、单条和批量都要对。

use std::time::Duration;

use pagesift_mcp::{
    BoxFuture, HttpConfig, McpServer, ToolContext, ToolDef, ToolHandler, ToolOutput, ToolResult,
};
use reqwest::Client;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};
use tokio::sync::oneshot;

/// 把收到的 clientInfo 名字原样返回。
struct Whoami;

impl ToolHandler for Whoami {
    fn definition(&self) -> ToolDef {
        ToolDef::new("whoami", json!({"type": "object"}))
    }
    fn call(&self, _: Map<String, Value>, ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        let name = ctx
            .client_info()
            .and_then(|c| c.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        Box::pin(async move { Ok(ToolOutput::text(name)) })
    }
}

fn server() -> McpServer {
    McpServer::builder("t", "1").tool(Whoami).build().unwrap()
}

fn init(version: &str, name: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": { "protocolVersion": version, "capabilities": {}, "clientInfo": { "name": name, "version": "1" } } })
}

fn call(id: u64) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": "whoami" } })
}

fn modern_call(id: u64, name: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": "whoami", "_meta": {
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": { "name": name, "version": "1" },
        "io.modelcontextprotocol/clientCapabilities": {}
    } } })
}

fn text_of(resp: &Value) -> &str {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text in {resp}"))
}

// ---------- stdio ----------

struct Stdio {
    w: tokio::io::DuplexStream,
    r: tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
}

impl Stdio {
    fn start() -> Stdio {
        let (cw, sr) = duplex(1 << 16);
        let (sw, cr) = duplex(1 << 16);
        tokio::spawn(pagesift_mcp::serve_stream(server(), sr, sw));
        Stdio {
            w: cw,
            r: BufReader::new(cr).lines(),
        }
    }

    async fn request(&mut self, v: Value) -> Value {
        self.w.write_all(v.to_string().as_bytes()).await.unwrap();
        self.w.write_all(b"\n").await.unwrap();
        let line = tokio::time::timeout(Duration::from_secs(5), self.r.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }
}

#[tokio::test]
async fn stdio_legacy_call_sees_initialize_client() {
    let mut s = Stdio::start();
    s.request(init("2025-06-18", "legacy-stdio")).await;
    assert_eq!(text_of(&s.request(call(1)).await), "legacy-stdio");
}

#[tokio::test]
async fn stdio_legacy_batch_sees_initialize_client() {
    let mut s = Stdio::start();
    s.request(init("2025-03-26", "batch-stdio")).await;
    let v = s.request(json!([call(1), call(2)])).await;
    for r in v.as_array().unwrap() {
        assert_eq!(text_of(r), "batch-stdio");
    }
}

#[tokio::test]
async fn stdio_modern_call_uses_request_meta() {
    let mut s = Stdio::start();
    assert_eq!(
        text_of(&s.request(modern_call(1, "modern-a")).await),
        "modern-a"
    );
    assert_eq!(
        text_of(&s.request(modern_call(2, "modern-b")).await),
        "modern-b"
    );
}

// ---------- HTTP ----------

async fn start_http() -> (String, oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        pagesift_mcp::serve_listener(server(), &HttpConfig::default(), listener, async {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    (url, tx)
}

async fn post(c: &Client, url: &str, body: &Value, headers: &[(&str, &str)]) -> reqwest::Response {
    let mut rb = c
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .body(body.to_string());
    for (k, v) in headers {
        rb = rb.header(*k, *v);
    }
    rb.send().await.unwrap()
}

async fn json_of(resp: reqwest::Response) -> Value {
    serde_json::from_slice(&resp.bytes().await.unwrap()).unwrap()
}

async fn legacy_session(c: &Client, url: &str, version: &str, name: &str) -> String {
    let resp = post(c, url, &init(version, name), &[]).await;
    resp.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn http_legacy_call_sees_initialize_client() {
    let (url, _stop) = start_http().await;
    let c = Client::new();
    let a = legacy_session(&c, &url, "2025-11-25", "legacy-a").await;
    let b = legacy_session(&c, &url, "2025-11-25", "legacy-b").await;
    for (sid, name) in [(&a, "legacy-a"), (&b, "legacy-b")] {
        let hs = [
            ("mcp-session-id", sid.as_str()),
            ("mcp-protocol-version", "2025-11-25"),
        ];
        let v = json_of(post(&c, &url, &call(1), &hs).await).await;
        assert_eq!(text_of(&v), name);
    }
}

#[tokio::test]
async fn http_legacy_batch_sees_initialize_client() {
    let (url, _stop) = start_http().await;
    let c = Client::new();
    let sid = legacy_session(&c, &url, "2025-03-26", "batch-http").await;
    let hs = [
        ("mcp-session-id", sid.as_str()),
        ("mcp-protocol-version", "2025-03-26"),
    ];
    let v = json_of(post(&c, &url, &json!([call(1), call(2)]), &hs).await).await;
    for r in v.as_array().unwrap() {
        assert_eq!(text_of(r), "batch-http");
    }
}

#[tokio::test]
async fn http_modern_call_uses_request_meta() {
    let (url, _stop) = start_http().await;
    let c = Client::new();
    let hs = [
        ("mcp-protocol-version", "2026-07-28"),
        ("mcp-method", "tools/call"),
        ("mcp-name", "whoami"),
    ];
    let v = json_of(post(&c, &url, &modern_call(1, "modern-http"), &hs).await).await;
    assert_eq!(text_of(&v), "modern-http");
}
