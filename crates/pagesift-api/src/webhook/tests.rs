use serde_json::json;

use super::deliver::sign;
use super::*;

#[test]
fn signatures_match_rfc_4231() {
    // RFC 4231 测试用例 2
    assert_eq!(
        sign("Jefe", b"what do ya want for nothing?"),
        "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
}

#[test]
fn parses_strings_objects_and_rejects_private_targets() {
    let policy = SsrfPolicy::default();
    let c = WebhookConfig::parse(&json!("https://hooks.example.com/x"), &policy).unwrap();
    assert_eq!(c.events, all_events());
    let c = WebhookConfig::parse(
        &json!({"url": "https://hooks.example.com/x", "headers": {"X-Token": "t"},
                "metadata": {"run": "1"}, "events": ["completed"], "future": true}),
        &policy,
    )
    .unwrap();
    assert!(c.wants(Event::Completed) && !c.wants(Event::Page));
    for (v, code) in [
        (json!("http://127.0.0.1:9000/hook"), ErrorCode::SsrfBlocked),
        (
            json!("http://169.254.169.254/latest"),
            ErrorCode::SsrfBlocked,
        ),
        (json!("http://metadata.internal/"), ErrorCode::SsrfBlocked),
        (json!("file:///etc/passwd"), ErrorCode::SsrfBlocked),
        (
            json!({"url": "https://a.example", "events": ["done"]}),
            ErrorCode::InvalidRequest,
        ),
        (json!(42), ErrorCode::InvalidRequest),
    ] {
        assert_eq!(
            WebhookConfig::parse(&v, &policy).unwrap_err().code,
            code,
            "{v}"
        );
    }
}

mod queue {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::{Map, Value};
    use tokio::sync::mpsc;

    use super::super::{Event, WebhookConfig, WebhookSettings, Webhooks, ack_key, all_events};

    /// 接收端：前 `ok_first` 个请求回 200，之后的挂住不回（`None` 表示全部回 200）。收到的请求体逐个转给测试。
    async fn receiver(ok_first: Option<usize>) -> (String, mpsc::UnboundedReceiver<Value>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let seen = Arc::new(AtomicUsize::new(0));
        let app = axum::Router::new().route(
            "/hook",
            axum::routing::post(move |body: String| {
                let (tx, seen) = (tx.clone(), seen.clone());
                async move {
                    let n = seen.fetch_add(1, Ordering::SeqCst);
                    let _ = tx.send(serde_json::from_str::<Value>(&body).unwrap());
                    if ok_first.is_some_and(|k| n >= k) {
                        std::future::pending::<()>().await;
                    }
                    axum::http::StatusCode::OK
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/hook"), rx)
    }

    fn settings(queue: usize) -> WebhookSettings {
        let mut s = WebhookSettings {
            queue,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(40),
            timeout: Duration::from_secs(3600),
            ..Default::default()
        };
        s.ssrf.allow_ips = vec!["127.0.0.1".parse().unwrap()];
        s
    }

    fn cfg(url: &str) -> WebhookConfig {
        WebhookConfig {
            url: url.into(),
            headers: BTreeMap::new(),
            metadata: Map::new(),
            events: all_events(),
        }
    }

    async fn next(rx: &mut mpsc::UnboundedReceiver<Value>) -> Value {
        tokio::time::timeout(Duration::from_secs(20), rx.recv())
            .await
            .expect("webhook event arrives")
            .expect("receiver alive")
    }

    #[tokio::test]
    async fn full_memory_queue_backpressures_instead_of_dropping() {
        let (url, mut rx) = receiver(None).await;
        // 队列只有 1 个位置，40 个页面事件同时发：旧实现会丢，现在发送方等着
        let w = Webhooks::new(settings(1), Arc::default());
        let hook = w.start("job-m", "crawl", cfg(&url), None);
        let mut sends = tokio::task::JoinSet::new();
        for _ in 0..40 {
            let h = hook.clone();
            sends.spawn(async move { h.send(Event::Page, Vec::new(), None).await });
        }
        while sends.join_next().await.is_some() {}
        drop(hook);
        for want in 1..=40u64 {
            let b = next(&mut rx).await;
            assert_eq!(b["seq"], want, "{b}");
            assert_eq!(b["eventId"], format!("job-m:{want}"));
            assert_eq!(b["type"], "crawl.page");
        }
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn stored_queue_resumes_after_restart_from_the_acked_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let store = pagesift_store::Store::open_dir(dir.path()).unwrap();
        // 第一次运行：接收端只确认前 2 个，第 3 个挂住，像进程在投递途中被杀
        let (url_a, mut rx_a) = receiver(Some(2)).await;
        let w1 = Webhooks::new(settings(1), Arc::default());
        let hook1 = w1.start("job-s", "batch_scrape", cfg(&url_a), Some(store.clone()));
        hook1.send(Event::Started, Vec::new(), None).await;
        for _ in 0..4 {
            hook1.send(Event::Page, Vec::new(), None).await;
        }
        for want in 1..=3u64 {
            assert_eq!(next(&mut rx_a).await["seq"], want);
        }
        drop(hook1);
        let acked = store.tasks().kv_get(ack_key("job-s")).unwrap().unwrap();
        assert_eq!(acked, b"2");

        // “重启”：新投递器接上同一个存储，从游标之后续投，新事件接着编号
        let (url_b, mut rx_b) = receiver(None).await;
        let w2 = Webhooks::new(settings(1), Arc::default());
        let hook2 = w2.start("job-s", "batch_scrape", cfg(&url_b), Some(store.clone()));
        hook2.send(Event::Completed, Vec::new(), None).await;
        drop(hook2);
        let got: Vec<Value> = [
            next(&mut rx_b).await,
            next(&mut rx_b).await,
            next(&mut rx_b).await,
            next(&mut rx_b).await,
        ]
        .into();
        let seqs: Vec<u64> = got.iter().map(|b| b["seq"].as_u64().unwrap()).collect();
        assert_eq!(seqs, [3, 4, 5, 6]);
        assert_eq!(got[3]["eventId"], "job-s:6");
        assert_eq!(got[3]["type"], "batch_scrape.completed");
        assert_eq!(got[0]["type"], "batch_scrape.page");
    }

    #[tokio::test]
    async fn store_failures_fall_back_to_the_memory_queue() {
        let dir = tempfile::tempdir().unwrap();
        let store = pagesift_store::Store::open_dir(dir.path()).unwrap();
        // 任务 id 超过存储的键长上限：这个任务的落盘读写全部失败，投递任务和发送方都得降级到内存队列
        let job = "j".repeat(70_000);
        let (url, mut rx) = receiver(None).await;
        let w = Webhooks::new(settings(1), Arc::default());
        let hook = w.start(&job, "crawl", cfg(&url), Some(store.clone()));
        hook.send(Event::Started, Vec::new(), None).await;
        let mut sends = tokio::task::JoinSet::new();
        for _ in 0..10 {
            let h = hook.clone();
            sends.spawn(async move { h.send(Event::Page, Vec::new(), None).await });
        }
        while sends.join_next().await.is_some() {}
        hook.send(Event::Completed, Vec::new(), None).await;
        drop(hook);
        for want in 1..=12u64 {
            let b = next(&mut rx).await;
            assert_eq!(b["seq"], want);
            assert_eq!(b["eventId"], format!("{job}:{want}"));
        }
        assert!(rx.try_recv().is_err());
    }
}
