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
