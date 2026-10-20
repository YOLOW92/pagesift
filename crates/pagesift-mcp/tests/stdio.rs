//! 以子进程方式启动 stdio 服务器，走完新旧两代客户端的完整流程。

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

struct Proc {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
}

impl Proc {
    fn spawn() -> Proc {
        let mut child = Command::new(env!("CARGO_BIN_EXE_pagesift-mcp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn pagesift-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        Proc {
            child,
            stdin,
            stdout,
        }
    }

    async fn send(&mut self, v: Value) {
        let mut line = serde_json::to_vec(&v).unwrap();
        line.push(b'\n');
        self.stdin.write_all(&line).await.unwrap();
    }

    async fn recv(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(10), self.stdout.next_line())
            .await
            .expect("timed out waiting for server")
            .unwrap()
            .expect("server closed stdout");
        // 每行都必须是一条完整的 JSON-RPC 消息。
        let v: Value = serde_json::from_str(&line).expect("stdout line is valid JSON");
        assert_eq!(v["jsonrpc"], "2.0", "{line}");
        v
    }

    async fn request(&mut self, v: Value) -> Value {
        self.send(v).await;
        self.recv().await
    }
}

fn modern_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": { "name": "e2e", "version": "1" },
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

#[tokio::test]
async fn legacy_initialize_list_call() {
    let mut p = Proc::spawn();
    let r = p
        .request(json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "e2e", "version": "1" } }
        }))
        .await;
    assert_eq!(r["id"], 1);
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(r["result"]["serverInfo"]["name"], "pagesift");
    assert!(r["result"]["capabilities"]["tools"].is_object());
    p.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .await;

    let r = p
        .request(json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" }))
        .await;
    assert_eq!(r["result"], json!({}));

    let r = p
        .request(json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }))
        .await;
    let names: Vec<&str> = r["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "pagesift_scrape",
            "pagesift_map",
            "pagesift_crawl",
            "pagesift_crawl_status",
            "pagesift_batch_scrape"
        ]
    );

    let r = p
        .request(json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "pagesift_scrape", "arguments": { "url": "https://example.com" } }
        }))
        .await;
    assert_eq!(r["id"], 4);
    assert_eq!(r["result"]["isError"], false);
    assert_eq!(
        r["result"]["structuredContent"]["metadata"]["statusCode"],
        200
    );
    assert!(
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Example Domain")
    );

    // 参数不合法：工具结果 isError，不是协议错误。
    let r = p
        .request(json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": { "name": "pagesift_scrape", "arguments": { "url": 42 } }
        }))
        .await;
    assert_eq!(r["result"]["isError"], true);

    // 未知工具：协议错误 -32602。
    let r = p
        .request(json!({ "jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": { "name": "firecrawl_scrape" } }))
        .await;
    assert_eq!(r["error"]["code"], -32602);

    // 关闭 stdin 后进程应当正常退出。
    drop(p.stdin);
    let status = tokio::time::timeout(Duration::from_secs(10), p.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn modern_discover_list_call_with_progress() {
    let mut p = Proc::spawn();
    let r = p
        .request(json!({ "jsonrpc": "2.0", "id": "d", "method": "server/discover", "params": { "_meta": modern_meta() } }))
        .await;
    assert_eq!(r["result"]["resultType"], "complete");
    assert!(
        r["result"]["supportedVersions"]
            .as_array()
            .unwrap()
            .contains(&json!("2026-07-28"))
    );
    assert_eq!(
        r["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "pagesift"
    );

    let r = p
        .request(json!({ "jsonrpc": "2.0", "id": "l", "method": "tools/list", "params": { "_meta": modern_meta() } }))
        .await;
    assert_eq!(r["result"]["cacheScope"], "public");
    assert!(r["result"]["ttlMs"].as_u64().is_some());
    assert_eq!(r["result"]["tools"].as_array().unwrap().len(), 5);

    let mut meta = modern_meta();
    meta["progressToken"] = json!("p1");
    p.send(json!({
        "jsonrpc": "2.0", "id": "c", "method": "tools/call",
        "params": { "name": "pagesift_map", "arguments": { "url": "https://example.com" }, "_meta": meta.clone() }
    }))
    .await;
    let r = p.recv().await;
    assert_eq!(
        r["result"]["structuredContent"]["links"][1]["url"],
        "https://example.com/about"
    );

    p.send(json!({
        "jsonrpc": "2.0", "id": "s", "method": "tools/call",
        "params": { "name": "pagesift_scrape", "arguments": { "url": "https://example.com" }, "_meta": meta }
    }))
    .await;
    let mut progress = Vec::new();
    let result = loop {
        let v = p.recv().await;
        if v["method"] == "notifications/progress" {
            assert_eq!(v["params"]["progressToken"], "p1");
            progress.push(v["params"]["progress"].as_f64().unwrap());
        } else {
            break v;
        }
    };
    assert_eq!(progress, [0.0, 1.0]);
    assert_eq!(result["id"], "s");
    assert_eq!(result["result"]["resultType"], "complete");

    // 不支持的版本：-32022，附带支持的版本列表。
    let mut bad = modern_meta();
    bad["io.modelcontextprotocol/protocolVersion"] = json!("1900-01-01");
    let r = p
        .request(json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/list", "params": { "_meta": bad } }))
        .await;
    assert_eq!(r["error"]["code"], -32022);
    assert!(
        r["error"]["data"]["supported"]
            .as_array()
            .unwrap()
            .contains(&json!("2025-11-25"))
    );
}

#[tokio::test]
async fn garbage_input_gets_errors_not_crashes() {
    let mut p = Proc::spawn();
    for line in [
        "{not json",
        "[]",
        "\"str\"",
        "{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"x\"}",
    ] {
        p.stdin.write_all(line.as_bytes()).await.unwrap();
        p.stdin.write_all(b"\n").await.unwrap();
        let r = p.recv().await;
        assert!(
            r["error"]["code"].as_i64().unwrap() <= -32600,
            "{line}: {r}"
        );
        assert!(r["id"].is_null());
    }
    // 非 UTF-8 字节。
    p.stdin.write_all(b"\xff\xfe\n").await.unwrap();
    assert_eq!(p.recv().await["error"]["code"], -32700);
    let r = p
        .request(json!({ "jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": { "_meta": modern_meta() } }))
        .await;
    assert_eq!(r["id"], 1);
}
