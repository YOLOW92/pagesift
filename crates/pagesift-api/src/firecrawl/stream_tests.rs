use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tower::ServiceExt;

use crate::test_engine::{FakeEngine, frames};
use crate::{Config, router};

const BASE: &str = "https://site.example";

fn get(path: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut b = Request::get(path);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    b.body(Body::empty()).unwrap()
}

async fn json_of(app: &axum::Router, req: Request<Body>) -> Value {
    let (_, _, f) = frames(app, req).await;
    serde_json::from_slice(&f.concat()).unwrap()
}

/// SSE 文本拆成 (事件名, id, data)。
fn events(text: &str) -> Vec<(String, Option<String>, Value)> {
    text.split("\n\n")
        .filter(|e| !e.is_empty())
        .map(|e| {
            let (mut name, mut id, mut data) = (String::new(), None, Value::Null);
            for line in e.lines() {
                if let Some(v) = line.strip_prefix("event: ") {
                    name = v.into();
                } else if let Some(v) = line.strip_prefix("id: ") {
                    id = Some(v.into());
                } else if let Some(v) = line.strip_prefix("data: ") {
                    data = serde_json::from_str(v).unwrap();
                }
            }
            (name, id, data)
        })
        .collect()
}

async fn app_with_gate(gate: Arc<Semaphore>) -> axum::Router {
    let engine = FakeEngine {
        gate: Some(gate),
        ..FakeEngine::open()
    };
    router(Arc::new(engine), Config::default())
}

async fn start_batch(app: &axum::Router, n: usize) -> String {
    let urls: Vec<String> = (0..n).map(|i| format!("{BASE}/p/{i}")).collect();
    let req = Request::post("/v2/batch/scrape")
        .header("content-type", "application/json")
        .body(Body::from(json!({ "urls": urls }).to_string()))
        .unwrap();
    json_of(app, req).await["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn pages_are_pushed_as_they_complete_then_done() {
    let gate = Arc::new(Semaphore::new(0));
    let app = app_with_gate(gate.clone()).await;
    let id = start_batch(&app, 3).await;
    gate.add_permits(1);
    let resp = app
        .clone()
        .oneshot(get(&format!("/v2/batch/scrape/{id}/stream"), &[]))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()["content-type"],
        "text/event-stream; charset=utf-8"
    );
    let mut body = resp.into_body();
    let mut text = String::new();
    // 第一页完成即推；剩下两页还卡在闸门上，流在等通知
    let first = tokio::time::timeout(Duration::from_secs(10), body.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    text.push_str(std::str::from_utf8(&first.into_data().unwrap()).unwrap());
    assert_eq!(events(&text).len(), 1);
    gate.add_permits(2);
    while let Some(f) = tokio::time::timeout(Duration::from_secs(10), body.frame())
        .await
        .unwrap()
    {
        text.push_str(std::str::from_utf8(&f.unwrap().into_data().unwrap()).unwrap());
    }
    let ev = events(&text);
    assert_eq!(ev.len(), 4, "{text}");
    let poll = json_of(&app, get(&format!("/v2/batch/scrape/{id}"), &[])).await;
    for (i, (name, eid, data)) in ev[..3].iter().enumerate() {
        assert_eq!(name, "page");
        assert_eq!(eid.as_deref(), Some(i.to_string().as_str()));
        // 和轮询接口里同一个下标的文档一样
        assert_eq!(data, &poll["data"][i]);
    }
    let (name, eid, data) = &ev[3];
    assert_eq!((name.as_str(), eid), ("done", &None));
    assert_eq!(data["status"], "completed");
    assert_eq!(data["completed"], 3);
}

#[tokio::test]
async fn resumes_after_the_cursor_and_speaks_ndjson() {
    let gate = Arc::new(Semaphore::new(3));
    let app = app_with_gate(gate).await;
    let id = start_batch(&app, 3).await;
    // 等任务做完：不带游标的流读到 done 为止
    let (_, _, f) = frames(&app, get(&format!("/v2/batch/scrape/{id}/stream"), &[])).await;
    assert_eq!(events(std::str::from_utf8(&f.concat()).unwrap()).len(), 4);

    let path = format!("/v2/batch/scrape/{id}/stream");
    let (_, _, f) = frames(&app, get(&format!("{path}?cursor=0"), &[])).await;
    let ev = events(std::str::from_utf8(&f.concat()).unwrap());
    let ids: Vec<_> = ev.iter().map(|e| (e.0.as_str(), e.1.as_deref())).collect();
    assert_eq!(
        ids,
        [("page", Some("1")), ("page", Some("2")), ("done", None)]
    );

    let (_, _, f) = frames(&app, get(&path, &[("last-event-id", "1")])).await;
    let ev = events(std::str::from_utf8(&f.concat()).unwrap());
    let ids: Vec<_> = ev.iter().map(|e| (e.0.as_str(), e.1.as_deref())).collect();
    assert_eq!(ids, [("page", Some("2")), ("done", None)]);

    let (status, headers, f) = frames(
        &app,
        get(
            &format!("{path}?cursor=1"),
            &[("accept", "application/x-ndjson")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/x-ndjson");
    let text = String::from_utf8(f.concat()).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(
        (lines[0]["seq"].clone(), lines[0]["type"].clone()),
        (json!(2), json!("page"))
    );
    assert!(lines[0]["data"]["markdown"].is_string());
    assert_eq!(lines[1]["type"], "done");
    assert_eq!(lines[1]["status"], "completed");

    let (status, _, _) = frames(&app, get("/v2/crawl/no-such-job/stream", &[])).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn idle_streams_send_keepalives() {
    use super::{Format, pump};
    let ndjson: Value = serde_json::from_slice(&Format::Ndjson.keepalive()).unwrap();
    assert_eq!(ndjson, json!({"type": "keepalive"}));

    let gate = Arc::new(Semaphore::new(0));
    let jobs = crate::jobs::Jobs::default();
    let engine = Arc::new(FakeEngine {
        gate: Some(gate.clone()),
        ..FakeEngine::open()
    });
    let id = jobs
        .start_batch(engine, vec![format!("{BASE}/p/0")], Default::default(), 1)
        .await;
    let changed = jobs.subscribe(&id).await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let interval = Duration::from_millis(20);
    tokio::spawn(pump(jobs, id, 0, Format::Sse, changed, tx, interval));
    // 页面卡在闸门上：先收到心跳
    for _ in 0..2 {
        let b = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&b[..], b": keepalive\n\n");
    }
    gate.add_permits(1);
    let mut rest = Vec::new();
    while let Some(b) = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
    {
        if &b[..] != b": keepalive\n\n" {
            rest.push(String::from_utf8(b.to_vec()).unwrap());
        }
    }
    let names: Vec<String> = events(&rest.concat()).into_iter().map(|e| e.0).collect();
    assert_eq!(names, ["page", "done"]);
}
