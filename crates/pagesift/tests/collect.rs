//! Real HTTP pagination, checkpoint/reopen, update/delete and robots tests.
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use pagesift::collect::{Adapter, Collection, ItemEvent};
use pagesift_net::{Fetcher, FetcherConfig};
use pagesift_policy::{Policy, PolicyConfig};
use pagesift_store::Store;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[tokio::test]
async fn wordpress_http_checkpoint_update_delete_and_robots() -> anyhow::Result<()> {
    let phase = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/robots.txt",
            get(|| async { "User-agent: *\nDisallow: /blocked\n" }),
        )
        .route(
            "/wp-json/wp/v2/posts",
            get(
                |State(phase): State<Arc<AtomicUsize>>,
                 Query(q): Query<HashMap<String, String>>| async move {
                    let page = q.get("page").map(String::as_str).unwrap_or("1");
                    let updated = phase.load(Ordering::SeqCst) > 0;
                    let rows = if updated {
                        json!([{"id":1,"link":"/new"},{"id":2}])
                    } else if page == "1" {
                        json!([{"id":1,"link":"/old"},{"id":2}])
                    } else {
                        json!([{"id":3}])
                    };
                    (
                        [
                            ("x-wp-totalpages", if updated { "1" } else { "2" }),
                            ("x-wp-total", if updated { "2" } else { "3" }),
                        ],
                        Json(rows),
                    )
                },
            ),
        )
        .with_state(phase.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
    let mut config = FetcherConfig::default();
    config.ssrf.allow_private_network = true;
    let mut pc = PolicyConfig::default();
    pc.rate.default_interval = std::time::Duration::from_millis(1);
    let policy = Policy::new(Arc::new(Fetcher::new(config)?), pc);
    let dir = tempfile::tempdir()?;
    let start = format!("{origin}/wp-json/wp/v2/posts?per_page=2&page=1").parse()?;
    let mut events: Vec<ItemEvent> = Vec::new();
    {
        let store = Store::open_dir(dir.path())?;
        let mut c = Collection::open(&store, "wp:posts", Adapter::WordPress)?;
        c.begin(&start, true)?;
        assert!(
            !c.run(&policy, 1, &mut |e| {
                events.push(e.clone());
                Ok(())
            })
            .await?
        );
        assert!(c.next_url().unwrap().ends_with("page=2"));
        assert_eq!(events.len(), 2);
    }
    {
        let store = Store::open_dir(dir.path())?;
        let mut c = Collection::open(&store, "wp:posts", Adapter::WordPress)?;
        c.begin(&start, true)?;
        assert!(
            c.run(&policy, 1, &mut |e| {
                events.push(e.clone());
                Ok(())
            })
            .await?
        );
        assert_eq!(events.len(), 3);
        phase.store(1, Ordering::SeqCst);
        c.begin(&start, true)?;
        assert!(
            c.run(&policy, 1, &mut |e| {
                events.push(e.clone());
                Ok(())
            })
            .await?
        );
        assert_eq!(events.len(), 5);
        assert_eq!(events[3].id, "1");
        assert_eq!(events[4].id, "3");
        assert!(events[4].value.is_none());
        c.begin(&start, true)?;
        assert!(
            c.run(&policy, 1, &mut |_| anyhow::bail!(
                "unchanged item must not be emitted"
            ))
            .await?
        );
        let mut denied = Collection::open(&store, "blocked", Adapter::WordPress)?;
        denied.begin(&format!("{origin}/blocked").parse()?, true)?;
        assert!(denied.run(&policy, 1, &mut |_| Ok(())).await.is_err());
        assert!(denied.is_active());
    }
    server.abort();
    Ok(())
}
