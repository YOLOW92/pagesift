//! 用假的 WebSocket 服务器测试 CDP 协议层。

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use pagesift_cdp::{Connection, Error};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// 启动一个只接一条连接的假服务器。收到的每条命令转给测试，测试通过 `reply` 发回任意消息。
async fn fake_server() -> (
    String,
    mpsc::UnboundedReceiver<Value>,
    mpsc::UnboundedSender<Option<Value>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/devtools/browser/fake",
        listener.local_addr().unwrap()
    );
    let (req_tx, req_rx) = mpsc::unbounded_channel();
    let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<Option<Value>>();
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let (mut sink, mut stream) = ws.split();
        loop {
            tokio::select! {
                msg = stream.next() => match msg {
                    Some(Ok(Message::Text(t))) => { let _ = req_tx.send(serde_json::from_str(t.as_str()).unwrap()); }
                    Some(Ok(_)) => {}
                    _ => break,
                },
                out = reply_rx.recv() => match out {
                    Some(Some(v)) => sink.send(Message::text(v.to_string())).await.unwrap(),
                    // None 表示关闭连接。
                    _ => { let _ = sink.close().await; break; }
                },
            }
        }
    });
    (url, req_rx, reply_tx)
}

#[tokio::test]
async fn matches_out_of_order_responses_by_id() {
    let (url, mut reqs, reply) = fake_server().await;
    let conn = Connection::connect(&url).await.unwrap();

    let c1 = conn.clone();
    let a = tokio::spawn(async move { c1.call("Browser.getVersion", json!({}), None).await });
    let first = reqs.recv().await.unwrap();
    let c2 = conn.clone();
    let b =
        tokio::spawn(async move { c2.call("Target.getTargets", json!({ "x": 1 }), None).await });
    let second = reqs.recv().await.unwrap();
    assert_eq!(first["method"], "Browser.getVersion");
    assert_eq!(second["params"]["x"], 1);
    assert_ne!(first["id"], second["id"]);
    assert!(first.get("sessionId").is_none());

    reply
        .send(Some(
            json!({ "id": second["id"], "result": { "which": "second" } }),
        ))
        .unwrap();
    reply
        .send(Some(
            json!({ "id": first["id"], "result": { "which": "first" } }),
        ))
        .unwrap();
    assert_eq!(a.await.unwrap().unwrap()["which"], "first");
    assert_eq!(b.await.unwrap().unwrap()["which"], "second");
}

#[tokio::test]
async fn protocol_errors_carry_method_and_code() {
    let (url, mut reqs, reply) = fake_server().await;
    let conn = Connection::connect(&url).await.unwrap();
    let c = conn.clone();
    let call =
        tokio::spawn(async move { c.call("Page.navigate", json!({ "url": "x" }), None).await });
    let req = reqs.recv().await.unwrap();
    // 未知 id 的响应和格式错误的消息都要被忽略。
    reply
        .send(Some(json!({ "id": 999_999, "result": {} })))
        .unwrap();
    reply.send(Some(json!("not an object"))).unwrap();
    reply
        .send(Some(
            json!({ "id": req["id"], "error": { "code": -32000, "message": "Cannot navigate" } }),
        ))
        .unwrap();
    match call.await.unwrap() {
        Err(Error::Protocol {
            method,
            code,
            message,
        }) => {
            assert_eq!(
                (method.as_str(), code, message.as_str()),
                ("Page.navigate", -32000, "Cannot navigate")
            );
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn flatten_sessions_and_event_routing() {
    let (url, mut reqs, reply) = fake_server().await;
    let conn = Connection::connect(&url).await.unwrap();

    let c = conn.clone();
    let attach = tokio::spawn(async move { c.attach("T1").await });
    let req = reqs.recv().await.unwrap();
    assert_eq!(req["method"], "Target.attachToTarget");
    assert_eq!(req["params"], json!({ "targetId": "T1", "flatten": true }));
    reply
        .send(Some(
            json!({ "id": req["id"], "result": { "sessionId": "S1" } }),
        ))
        .unwrap();
    let session = attach.await.unwrap().unwrap();
    assert_eq!(session.id(), "S1");

    let mut page_events = session.events();
    let mut all_events = conn.subscribe(None);

    let s = session.clone();
    let call = tokio::spawn(async move {
        s.call("Runtime.evaluate", json!({ "expression": "1" }))
            .await
    });
    let req = reqs.recv().await.unwrap();
    assert_eq!(req["sessionId"], "S1");

    reply
        .send(Some(
            json!({ "method": "Page.loadEventFired", "sessionId": "S2", "params": { "n": 0 } }),
        ))
        .unwrap();
    reply
        .send(Some(
            json!({ "method": "Page.loadEventFired", "sessionId": "S1", "params": { "n": 1 } }),
        ))
        .unwrap();
    reply
        .send(Some(
            json!({ "method": "Target.targetCreated", "params": { "n": 2 } }),
        ))
        .unwrap();
    reply
        .send(Some(
            json!({ "id": req["id"], "sessionId": "S1", "result": { "result": { "value": 1 } } }),
        ))
        .unwrap();

    assert_eq!(call.await.unwrap().unwrap()["result"]["value"], 1);
    let ev = page_events.recv().await.unwrap();
    assert_eq!(
        (ev.method.as_str(), ev.params["n"].as_i64()),
        ("Page.loadEventFired", Some(1))
    );
    let ns: Vec<i64> = [
        all_events.recv().await.unwrap(),
        all_events.recv().await.unwrap(),
        all_events.recv().await.unwrap(),
    ]
    .iter()
    .map(|e| e.params["n"].as_i64().unwrap())
    .collect();
    assert_eq!(ns, [0, 1, 2]);
    // 只有 S1 的事件进了 session 的订阅。
    assert!(
        tokio::time::timeout(Duration::from_millis(50), page_events.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn close_fails_pending_calls_and_ends_streams() {
    let (url, mut reqs, reply) = fake_server().await;
    let conn = Connection::connect(&url).await.unwrap();
    let mut events = conn.subscribe(None);
    let c = conn.clone();
    let pending = tokio::spawn(async move { c.call("Browser.getVersion", json!({}), None).await });
    reqs.recv().await.unwrap();
    reply.send(None).unwrap();
    assert!(matches!(
        pending.await.unwrap(),
        Err(Error::ConnectionClosed)
    ));
    assert!(events.recv().await.is_none());
    assert!(conn.is_closed());
    assert!(matches!(
        conn.call("Browser.getVersion", json!({}), None).await,
        Err(Error::ConnectionClosed)
    ));
}

#[tokio::test]
async fn abandoned_call_does_not_break_later_ones() {
    let (url, mut reqs, reply) = fake_server().await;
    let conn = Connection::connect(&url).await.unwrap();
    let slow = tokio::time::timeout(
        Duration::from_millis(20),
        conn.call("Slow.method", json!({}), None),
    )
    .await;
    assert!(slow.is_err());
    let abandoned = reqs.recv().await.unwrap();
    let c = conn.clone();
    let next = tokio::spawn(async move { c.call("Fast.method", json!({}), None).await });
    let req = reqs.recv().await.unwrap();
    reply
        .send(Some(
            json!({ "id": abandoned["id"], "result": { "late": true } }),
        ))
        .unwrap();
    reply
        .send(Some(json!({ "id": req["id"], "result": { "ok": true } })))
        .unwrap();
    assert_eq!(next.await.unwrap().unwrap(), json!({ "ok": true }));
}

struct AllowAllGuard;

impl pagesift_cdp::RequestGuard for AllowAllGuard {
    fn check<'a>(
        &'a self,
        _req: &'a pagesift_cdp::SubRequest,
    ) -> pagesift_cdp::BoxFuture<'a, pagesift_cdp::Verdict> {
        Box::pin(async { pagesift_cdp::Verdict::Allow })
    }
}

/// 启动可编程故障注入的假 CDP 服务器，记录所有收到的命令并按策略响应。
async fn spawn_fault_cdp(
    fail_on: impl Fn(&str, Option<&str>, &Value) -> Option<(i64, &'static str)> + Send + Sync + 'static,
    on_navigate_event: Option<Value>,
) -> (
    String,
    std::sync::Arc<std::sync::Mutex<Vec<(String, Option<String>)>>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/devtools/browser/fault",
        listener.local_addr().unwrap()
    );
    let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let rec = recorded.clone();
    let fail_on = std::sync::Arc::new(fail_on);
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let rec = rec.clone();
            let fail_on = fail_on.clone();
            let nav_ev = on_navigate_event.clone();
            tokio::spawn(async move {
                let Ok(ws) = tokio_tungstenite::accept_async(tcp).await else {
                    return;
                };
                let (mut sink, mut stream) = ws.split();
                while let Some(Ok(msg)) = stream.next().await {
                    let Message::Text(text) = msg else { continue };
                    let Ok(v) = serde_json::from_str::<Value>(text.as_str()) else {
                        continue;
                    };
                    let id = v["id"].as_u64().unwrap_or(0);
                    let method = v["method"].as_str().unwrap_or_default().to_owned();
                    let sid = v
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let params = v.get("params").cloned().unwrap_or(Value::Null);
                    rec.lock().unwrap().push((method.clone(), sid.clone()));

                    if let Some((code, msg)) = fail_on(&method, sid.as_deref(), &params) {
                        let mut err = json!({
                            "id": id,
                            "error": { "code": code, "message": msg }
                        });
                        if let Some(s) = &sid {
                            err["sessionId"] = Value::String(s.clone());
                        }
                        let _ = sink.send(Message::text(err.to_string())).await;
                        continue;
                    }

                    let result = match method.as_str() {
                        "Browser.getVersion" => json!({
                            "product": "Chrome/130.0.0.0",
                            "userAgent": "Mozilla/5.0 Chrome/130.0.0.0 Safari/537.36"
                        }),
                        "Target.createBrowserContext" => json!({ "browserContextId": "CTX1" }),
                        "Target.createTarget" => json!({ "targetId": "T1" }),
                        "Target.attachToTarget" => json!({ "sessionId": "S_PAGE" }),
                        "Page.getFrameTree" => {
                            json!({ "frameTree": { "frame": { "id": "F_MAIN" } } })
                        }
                        "Page.navigate" => json!({ "frameId": "F_MAIN", "loaderId": "L1" }),
                        "Page.createIsolatedWorld" => json!({ "executionContextId": 1 }),
                        "Runtime.callFunctionOn" => json!({
                            "result": {
                                "type": "object",
                                "value": {
                                    "html": "<html><body><p>ok</p></body></html>",
                                    "url": "http://example.com/"
                                }
                            }
                        }),
                        "DOM.getDocument" => json!({ "root": { "nodeId": 1 } }),
                        "DOM.getOuterHTML" => {
                            json!({ "outerHTML": "<html><body><p>ok</p></body></html>" })
                        }
                        "Runtime.evaluate" => {
                            json!({ "result": { "type": "string", "value": "http://example.com/" } })
                        }
                        _ => json!({}),
                    };
                    let mut reply = json!({ "id": id, "result": result });
                    if let Some(s) = &sid {
                        reply["sessionId"] = Value::String(s.clone());
                    }
                    let _ = sink.send(Message::text(reply.to_string())).await;

                    if method == "Page.navigate" {
                        if let Some(ev) = &nav_ev {
                            let _ = sink.send(Message::text(ev.to_string())).await;
                        } else {
                            for name in ["init", "DOMContentLoaded", "load"] {
                                let ev = json!({
                                    "method": "Page.lifecycleEvent",
                                    "sessionId": "S_PAGE",
                                    "params": { "frameId": "F_MAIN", "loaderId": "L1", "name": name }
                                });
                                let _ = sink.send(Message::text(ev.to_string())).await;
                            }
                        }
                    }
                }
            });
        }
    });
    (url, recorded)
}

#[tokio::test]
async fn p1_03_strict_security_setup_failures_abort_before_navigation() {
    for fail_method in [
        "Page.addScriptToEvaluateOnNewDocument",
        "Network.setBlockedURLs",
        "Network.setBypassServiceWorker",
    ] {
        let (ws_url, recorded) = spawn_fault_cdp(
            move |method, _sid, params| {
                if method == fail_method {
                    // For Page.addScriptToEvaluateOnNewDocument, fail on the transport blocker script.
                    if method == "Page.addScriptToEvaluateOnNewDocument"
                        && !params["source"]
                            .as_str()
                            .unwrap_or("")
                            .contains("WebSocket")
                    {
                        return None;
                    }
                    return Some((-32000, "injected security setup failure"));
                }
                None
            },
            None,
        )
        .await;

        let pool = pagesift_cdp::BrowserPool::new(
            pagesift_cdp::PoolConfig {
                source: pagesift_cdp::BrowserSource::Remote(ws_url),
                max_browsers: 1,
                max_tabs_per_browser: 1,
                health_check_interval: Duration::ZERO,
                idle_timeout: Duration::ZERO,
                ..Default::default()
            },
            std::sync::Arc::new(AllowAllGuard),
        )
        .await
        .unwrap();

        let opts = pagesift_cdp::RenderOptions {
            network_budget: Some(std::sync::Arc::new(pagesift_itna::NetworkBudget::new(
                Duration::from_secs(5),
                4,
                16,
            ))),
            allowed_hosts: vec!["example.com".into()],
            bypass_service_worker: true,
            wait_until: pagesift_cdp::WaitUntil::DomContentLoaded,
            timeout: Duration::from_secs(3),
            ..Default::default()
        };

        let err = pool
            .render("http://example.com/", &opts)
            .await
            .expect_err(&format!(
                "render must fail closed when {fail_method} fails under strict security"
            ));
        assert!(
            matches!(err, Error::InterceptionUnavailable(_)),
            "expected InterceptionUnavailable when {fail_method} fails, got {err:?}"
        );
        {
            let calls = recorded.lock().unwrap();
            assert!(
                !calls.iter().any(|(m, _)| m == "Page.navigate"),
                "Page.navigate must NEVER be sent after {fail_method} failed; calls={calls:?}"
            );
        }
        pool.shutdown().await;
    }
}

#[tokio::test]
async fn p1_03_child_target_security_setup_failure_never_resumes_child() {
    for (target_type, fail_method) in [
        ("worker", "Runtime.evaluate"),
        ("worker", "Network.setBlockedURLs"),
        ("iframe", "Page.addScriptToEvaluateOnNewDocument"),
        ("iframe", "Network.setBlockedURLs"),
        ("iframe", "Network.setBypassServiceWorker"),
        ("page", "Page.addScriptToEvaluateOnNewDocument"),
        ("page", "Network.setBlockedURLs"),
        ("worklet", "Runtime.evaluate"),
        ("shared_worker", "Fetch.enable"),
        ("service_worker", "Fetch.enable"),
    ] {
        let attach_event = json!({
            "method": "Target.attachedToTarget",
            "sessionId": "S_PAGE",
            "params": {
                "sessionId": "S_CHILD",
                "targetInfo": {
                    "targetId": "T_CHILD",
                    "type": target_type
                }
            }
        });
        let (ws_url, recorded) = spawn_fault_cdp(
            move |method, sid, _params| {
                if sid == Some("S_CHILD") && method == fail_method {
                    return Some((-32000, "injected child setup failure"));
                }
                None
            },
            Some(attach_event),
        )
        .await;

        let pool = pagesift_cdp::BrowserPool::new(
            pagesift_cdp::PoolConfig {
                source: pagesift_cdp::BrowserSource::Remote(ws_url),
                max_browsers: 1,
                max_tabs_per_browser: 1,
                health_check_interval: Duration::ZERO,
                idle_timeout: Duration::ZERO,
                ..Default::default()
            },
            std::sync::Arc::new(AllowAllGuard),
        )
        .await
        .unwrap();

        let opts = pagesift_cdp::RenderOptions {
            network_budget: Some(std::sync::Arc::new(pagesift_itna::NetworkBudget::new(
                Duration::from_secs(5),
                4,
                16,
            ))),
            bypass_service_worker: true,
            wait_until: pagesift_cdp::WaitUntil::DomContentLoaded,
            timeout: Duration::from_secs(2),
            ..Default::default()
        };

        let err = pool
            .render("http://example.com/", &opts)
            .await
            .expect_err(&format!(
                "render must fail when {target_type} child {fail_method} fails"
            ));
        assert!(
            matches!(err, Error::InterceptionUnavailable(_)),
            "expected InterceptionUnavailable for {target_type} {fail_method}, got {err:?}"
        );
        {
            let calls = recorded.lock().unwrap();
            assert!(
                !calls
                    .iter()
                    .any(|(m, s)| m == "Runtime.runIfWaitingForDebugger"
                        && s.as_deref() == Some("S_CHILD")),
                "child {target_type} must NOT be resumed when {fail_method} failed; calls={calls:?}"
            );
        }
        pool.shutdown().await;
    }

    // ServiceWorker / SharedWorker pipeline Network/Runtime security calls alongside
    // Runtime.runIfWaitingForDebugger to avoid Chromium's ServiceWorkerDevToolsAgentHost
    // startup deadlock, and MUST still fail closed when any security call fails.
    for (target_type, fail_method) in [
        ("shared_worker", "Network.setBlockedURLs"),
        ("shared_worker", "Runtime.evaluate"),
        ("service_worker", "Network.setBlockedURLs"),
        ("service_worker", "Runtime.evaluate"),
    ] {
        let attach_event = json!({
            "method": "Target.attachedToTarget",
            "sessionId": "S_PAGE",
            "params": {
                "sessionId": "S_CHILD",
                "targetInfo": {
                    "targetId": "T_CHILD",
                    "type": target_type
                }
            }
        });
        let (ws_url, _recorded) = spawn_fault_cdp(
            move |method, sid, _params| {
                if sid == Some("S_CHILD") && method == fail_method {
                    return Some((-32000, "injected pipelined worker setup failure"));
                }
                None
            },
            Some(attach_event),
        )
        .await;
        let pool = pagesift_cdp::BrowserPool::new(
            pagesift_cdp::PoolConfig {
                source: pagesift_cdp::BrowserSource::Remote(ws_url),
                max_browsers: 1,
                max_tabs_per_browser: 1,
                health_check_interval: Duration::ZERO,
                idle_timeout: Duration::ZERO,
                ..Default::default()
            },
            std::sync::Arc::new(AllowAllGuard),
        )
        .await
        .unwrap();
        let opts = pagesift_cdp::RenderOptions {
            network_budget: Some(std::sync::Arc::new(pagesift_itna::NetworkBudget::new(
                Duration::from_secs(5),
                4,
                16,
            ))),
            bypass_service_worker: true,
            wait_until: pagesift_cdp::WaitUntil::DomContentLoaded,
            timeout: Duration::from_secs(2),
            ..Default::default()
        };
        let err = pool
            .render("http://example.com/", &opts)
            .await
            .expect_err(&format!(
                "render must fail closed when pipelined {target_type} {fail_method} fails"
            ));
        assert!(
            matches!(err, Error::InterceptionUnavailable(_)),
            "expected InterceptionUnavailable for {target_type} {fail_method}, got {err:?}"
        );
        pool.shutdown().await;
    }
}

#[tokio::test]
async fn p1_03_non_intercept_engine_rejects_allowed_hosts_and_bypass_sw() {
    let (ws_url, recorded) = spawn_fault_cdp(|_, _, _| None, None).await;
    let pool = pagesift_cdp::BrowserPool::new(
        pagesift_cdp::PoolConfig {
            source: pagesift_cdp::BrowserSource::Remote(ws_url),
            args_override: Some(vec!["--block-private-networks".into()]),
            engine_network_guard: true,
            max_browsers: 1,
            max_tabs_per_browser: 1,
            health_check_interval: Duration::ZERO,
            idle_timeout: Duration::ZERO,
            ..Default::default()
        },
        std::sync::Arc::new(AllowAllGuard),
    )
    .await
    .unwrap();

    let err_hosts = pool
        .render(
            "http://example.com/",
            &pagesift_cdp::RenderOptions {
                allowed_hosts: vec!["example.com".into()],
                timeout: Duration::from_secs(2),
                ..Default::default()
            },
        )
        .await
        .expect_err("allowed_hosts must be rejected when env.intercept is false");
    assert!(
        matches!(err_hosts, Error::InvalidOptions(_)),
        "{err_hosts:?}"
    );

    let err_sw = pool
        .render(
            "http://example.com/",
            &pagesift_cdp::RenderOptions {
                bypass_service_worker: true,
                timeout: Duration::from_secs(2),
                ..Default::default()
            },
        )
        .await
        .expect_err("bypass_service_worker must be rejected when env.intercept is false");
    assert!(matches!(err_sw, Error::InvalidOptions(_)), "{err_sw:?}");

    {
        let calls = recorded.lock().unwrap();
        assert!(!calls.iter().any(|(m, _)| m == "Page.navigate"));
    }
    pool.shutdown().await;
}

#[tokio::test]
async fn p1_03_blocked_urls_method_not_found_degrades_only_outside_strict_security() {
    // 1. Non-strict ordinary render: Network.setBlockedURLs returning -32601 (method not found)
    //    degrades gracefully so minimal CDP engines still render normal pages.
    let (ws_url, _recorded) = spawn_fault_cdp(
        |method, _sid, _params| {
            if method == "Network.setBlockedURLs" {
                return Some((-32601, "'Network.setBlockedURLs' wasn't found"));
            }
            None
        },
        None,
    )
    .await;
    let pool = pagesift_cdp::BrowserPool::new(
        pagesift_cdp::PoolConfig {
            source: pagesift_cdp::BrowserSource::Remote(ws_url),
            max_browsers: 1,
            max_tabs_per_browser: 1,
            health_check_interval: Duration::ZERO,
            idle_timeout: Duration::ZERO,
            ..Default::default()
        },
        std::sync::Arc::new(AllowAllGuard),
    )
    .await
    .unwrap();
    let ok = pool
        .render(
            "http://example.com/",
            &pagesift_cdp::RenderOptions {
                wait_until: pagesift_cdp::WaitUntil::DomContentLoaded,
                timeout: Duration::from_secs(2),
                ..Default::default()
            },
        )
        .await;
    assert!(
        ok.is_ok(),
        "non-strict render should allow -32601 on Network.setBlockedURLs: {ok:?}"
    );

    // 2. Strict security mode (allowed_hosts / network_budget / bypass_service_worker):
    //    even -32601 on Network.setBlockedURLs MUST fail closed before navigation.
    let err_strict = pool
        .render(
            "http://example.com/",
            &pagesift_cdp::RenderOptions {
                allowed_hosts: vec!["example.com".into()],
                wait_until: pagesift_cdp::WaitUntil::DomContentLoaded,
                timeout: Duration::from_secs(2),
                ..Default::default()
            },
        )
        .await
        .expect_err("strict security render must reject -32601 on Network.setBlockedURLs");
    assert!(
        matches!(err_strict, Error::InterceptionUnavailable(_)),
        "{err_strict:?}"
    );
    pool.shutdown().await;

    // 3. Non-strict render with non-32601 failure (e.g. internal/protocol failure) MUST fail closed.
    let (ws_url2, recorded2) = spawn_fault_cdp(
        |method, _sid, _params| {
            if method == "Network.setBlockedURLs" {
                return Some((-32000, "internal network error"));
            }
            None
        },
        None,
    )
    .await;
    let pool2 = pagesift_cdp::BrowserPool::new(
        pagesift_cdp::PoolConfig {
            source: pagesift_cdp::BrowserSource::Remote(ws_url2),
            max_browsers: 1,
            max_tabs_per_browser: 1,
            health_check_interval: Duration::ZERO,
            idle_timeout: Duration::ZERO,
            ..Default::default()
        },
        std::sync::Arc::new(AllowAllGuard),
    )
    .await
    .unwrap();
    let err_non_32601 = pool2
        .render(
            "http://example.com/",
            &pagesift_cdp::RenderOptions {
                wait_until: pagesift_cdp::WaitUntil::DomContentLoaded,
                timeout: Duration::from_secs(2),
                ..Default::default()
            },
        )
        .await
        .expect_err("non-32601 Network.setBlockedURLs failure must fail closed");
    assert!(
        matches!(err_non_32601, Error::InterceptionUnavailable(_)),
        "{err_non_32601:?}"
    );
    assert!(
        !recorded2
            .lock()
            .unwrap()
            .iter()
            .any(|(m, _)| m == "Page.navigate"),
        "must abort before Page.navigate"
    );
    pool2.shutdown().await;
}
