//! CDP 兼容性探测：按 pagesift 渲染时的顺序发命令，打印每条命令的结果和收到的事件。
//! 用来检查其他内核（Lightpanda、obscura、moli）支持哪些方法。
//! `cargo run -p pagesift-cdp --example cdp_probe -- ws://127.0.0.1:9222/... http://page`

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let ws_url = args.next().expect("usage: cdp_probe <ws-url> <page-url>");
    let page = args.next().expect("page url");
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url.as_str())
        .await
        .expect("connect");
    let mut id = 0u64;
    let mut events: BTreeMap<String, usize> = BTreeMap::new();
    macro_rules! call {
        ($method:expr, $params:expr, $session:expr) => {{
            id += 1;
            let mut msg = json!({ "id": id, "method": $method, "params": $params });
            if let Some(s) = $session.as_deref() {
                msg["sessionId"] = json!(s);
            }
            ws.send(Message::text(msg.to_string())).await.unwrap();
            let mut result = Value::Null;
            loop {
                match tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
                    Ok(Some(Ok(Message::Text(t)))) => {
                        let v: Value = serde_json::from_str(&t).unwrap();
                        if v["id"] == json!(id) {
                            let ok = v.get("error").is_none();
                            let body = if ok { &v["result"] } else { &v["error"] };
                            let s = body.to_string();
                            println!("{} {}: {}", if ok { "OK " } else { "ERR" }, $method, &s[..s.len().min(160)]);
                            result = v["result"].clone();
                            break;
                        }
                        if let Some(m) = v["method"].as_str() {
                            *events.entry(m.to_string()).or_default() += 1;
                            // 等结果期间也要放行被拦下的请求，否则导航和拦截互相等待
                            if m == "Fetch.requestPaused" {
                                let rid = v["params"]["requestId"].clone();
                                let url = v["params"]["request"]["url"].as_str().unwrap_or("").to_string();
                                println!("    paused {url} -> continue");
                                let mut cont = json!({ "id": 900000 + id, "method": "Fetch.continueRequest", "params": { "requestId": rid } });
                                if let Some(s) = v["sessionId"].as_str() { cont["sessionId"] = json!(s); }
                                ws.send(Message::text(cont.to_string())).await.unwrap();
                            }
                        } else if v["id"].as_u64().is_some_and(|i| i >= 900000) {
                            println!("    continue -> {}", v.get("error").map_or("ok".to_string(), |e| e.to_string()));
                        }
                    }
                    Ok(Some(Ok(_))) => {}
                    _ => {
                        println!("TIMEOUT/CLOSED {}", $method);
                        break;
                    }
                }
            }
            result
        }};
    }
    let none: Option<String> = None;
    let ctx = call!(
        "Target.createBrowserContext",
        json!({ "disposeOnDetach": true }),
        none
    );
    let mut params = json!({ "url": "about:blank" });
    if let Some(c) = ctx["browserContextId"].as_str() {
        params["browserContextId"] = json!(c);
    }
    let t = call!("Target.createTarget", params, none);
    let a = call!(
        "Target.attachToTarget",
        json!({ "targetId": t["targetId"], "flatten": true }),
        none
    );
    let sid: Option<String> = a["sessionId"].as_str().map(str::to_owned);
    for (m, p) in [
        ("Page.enable", json!({})),
        ("Network.enable", json!({})),
        ("Page.setLifecycleEventsEnabled", json!({ "enabled": true })),
        (
            "Emulation.setUserAgentOverride",
            json!({ "userAgent": "pagesift-probe" }),
        ),
        (
            "Emulation.setDeviceMetricsOverride",
            json!({ "width": 1280, "height": 900, "deviceScaleFactor": 1, "mobile": false }),
        ),
        (
            "Fetch.enable",
            json!({ "patterns": [{ "urlPattern": "*", "requestStage": "Request" }] }),
        ),
        (
            "Target.setAutoAttach",
            json!({ "autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true }),
        ),
        ("Page.navigate", json!({ "url": page })),
    ] {
        call!(m, p, sid);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while let Ok(Some(Ok(Message::Text(t)))) = tokio::time::timeout_at(deadline, ws.next()).await {
        let v: Value = serde_json::from_str(&t).unwrap();
        // 和 pagesift 一样：拦下的请求逐个放行
        if v["method"] == "Fetch.requestPaused" {
            id += 1;
            let mut msg = json!({ "id": id, "method": "Fetch.continueRequest", "params": { "requestId": v["params"]["requestId"] } });
            if let Some(s) = sid.as_deref() {
                msg["sessionId"] = json!(s);
            }
            ws.send(Message::text(msg.to_string())).await.unwrap();
        }
        let name = v["method"].as_str().map_or("(response)".to_string(), |m| {
            if m == "Page.lifecycleEvent" {
                format!(
                    "{m}:{}@{}",
                    v["params"]["name"].as_str().unwrap_or(""),
                    v["params"]["loaderId"].as_str().unwrap_or("-")
                )
            } else if m == "Fetch.requestPaused" || m.starts_with("Network.") {
                let url = v["params"]["request"]["url"]
                    .as_str()
                    .or(v["params"]["requestId"].as_str())
                    .unwrap_or("");
                format!("{m} {}", &url[..url.len().min(50)])
            } else {
                m.to_string()
            }
        });
        *events.entry(name).or_default() += 1;
    }
    println!("events: {events:?}");
    call!("Page.getFrameTree", json!({}), sid);
    call!(
        "Runtime.evaluate",
        json!({ "expression": "document.documentElement.outerHTML.length", "returnByValue": true }),
        sid
    );
}
