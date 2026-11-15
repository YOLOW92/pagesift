use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::response::Html;
use axum::routing::get;
use pagesift_api::ErrorCode;
use pagesift_graph::ClaimStatus;
use pagesift_plan::experience::FamilyExperienceTable;
use pagesift_plan::render::family;
use pagesift_store::ResourceLifecycleState;
use pagesift_store::Store;

use super::common::*;

#[tokio::test]
async fn p0_02_mid_tick_delete_and_recreate_aborts_stale_instance_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = std::sync::Arc::new(test_scraper(Some(store.clone())));

    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let started_tx = arc_state(tokio::sync::Mutex::new(Some(started_tx)));

    let app = Router::new()
        .route(
            "/slow-a",
            get(
                |State(tx): State<
                    std::sync::Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
                >| async move {
                    if let Some(sender) = tx.lock().await.take() {
                        let _ = sender.send(());
                    }
                    tokio::time::sleep(Duration::from_millis(120)).await;
                    Html(product_html("Stale A", "A-1", "999.00", &[]))
                },
            ),
        )
        .route(
            "/fresh-b",
            get(|| async { Html(product_html("Fresh B", "B-1", "10.00", &[])) }),
        )
        .with_state(started_tx);
    let (base, shutdown) = spawn_server(app).await;

    let mut spec_a = sample_spec("docs", format!("{base}/slow-a"));
    spec_a.budget.max_requests_per_tick = 1;
    let stored_a = store.sync().upsert_source(spec_a).unwrap();

    let s_tick = scraper.clone();
    let tick_handle = tokio::spawn(async move { s_tick.run_sync_tick("docs", run_opts()).await });

    // Once tick for A is actively fetching /slow-a, delete A and recreate B with the same id & rev=1.
    started_rx.await.unwrap();
    assert!(store.sync().delete_source("docs").unwrap());
    let spec_b = sample_spec("docs", format!("{base}/fresh-b"));
    let stored_b = store.sync().upsert_source(spec_b).unwrap();
    assert_eq!(stored_b.revision, 1);
    assert_ne!(stored_a.instance_id, stored_b.instance_id);

    let tick_err = tick_handle.await.unwrap().unwrap_err();
    assert_eq!(tick_err.code, ErrorCode::NotFound);

    // Instance B must have zero contaminated inventory, experience, deltas, or graph state from A.
    let view_b = store
        .sync()
        .build_state_view("docs", None)
        .unwrap()
        .unwrap();
    assert_eq!(view_b.total_ticks, 0);
    assert!(view_b.resources.is_empty());
    assert!(store.sync().list_deltas("docs", 16).unwrap().is_empty());
    assert!(
        store
            .graph()
            .get_resource_claims(&format!("{base}/slow-a"))
            .unwrap()
            .is_empty()
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn p1_03_budget_exhaustion_and_transport_errors_do_not_pollute_claims_or_family_stats() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = std::sync::Arc::new(test_scraper(Some(store.clone())));

    let slow_p2 = arc_state(AtomicBool::new(false));
    let app = Router::new()
        .route(
            "/",
            get(|| async {
                Html(product_html(
                    "Hub",
                    "HUB-1",
                    "1.00",
                    &[("/p1", "P1"), ("/p2", "P2")],
                ))
            }),
        )
        .route(
            "/p1",
            get(|| async { Html(product_html("P1", "P1-1", "10.00", &[])) }),
        )
        .route(
            "/p2",
            get(
                |State(slow): State<std::sync::Arc<AtomicBool>>| async move {
                    if slow.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(350)).await;
                    }
                    Html(product_html("P2", "P2-1", "20.00", &[]))
                },
            ),
        )
        .with_state(slow_p2.clone());
    let (base, shutdown) = spawn_server(app).await;

    let mut spec = sample_spec("s-p1-03", format!("{base}/"));
    spec.budget.max_requests_per_tick = 16;
    spec.budget.max_wall_clock_ms = 5_000;
    store.sync().upsert_source(spec.clone()).unwrap();

    // Tick 1: crawl hub + /p1 + /p2 cleanly so both claims are Active.
    let (_, inv1) = tick_and_get(&scraper, &store, "s-p1-03", run_opts_forced()).await;
    assert_eq!(inv1.len(), 3);
    let p2_url = format!("{base}/p2");
    assert_eq!(
        inv1.iter().find(|r| r.url == p2_url).unwrap().state,
        ResourceLifecycleState::Active
    );

    // Enable 350ms delay on /p2 and shrink wall-clock budget to 60ms so tick hits BudgetExhausted.
    slow_p2.store(true, Ordering::SeqCst);
    spec.budget.max_wall_clock_ms = 60;
    store.sync().upsert_source(spec).unwrap();
    let (d2, inv2) = tick_and_get(&scraper, &store, "s-p1-03", run_opts_forced()).await;
    assert_eq!(d2.stats.transient_failures, 0);
    let p2_after = inv2.iter().find(|r| r.url == p2_url).unwrap();
    assert_eq!(p2_after.state, ResourceLifecycleState::Active);
    assert_eq!(p2_after.consecutive_failures, 0);
    let p2_claims = store.graph().get_resource_claims(&p2_url).unwrap();
    assert!(
        p2_claims.iter().all(|c| c.status == ClaimStatus::Active),
        "budget exhaustion must not flip Active claims to Unverified: {p2_claims:?}"
    );

    // Transport failure on a deterministically closed loopback port must not record negative http_sufficient!
    let closed_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed_port = closed_listener.local_addr().unwrap().port();
    drop(closed_listener);
    let unreachable_url = format!("http://127.0.0.1:{closed_port}/items/1");
    let mut bad_spec = sample_spec("s-p1-03-net", unreachable_url.clone());
    bad_spec.budget.max_wall_clock_ms = 2_000;
    store.sync().upsert_source(bad_spec).unwrap();
    let (d_net, _) = tick_and_get(&scraper, &store, "s-p1-03-net", run_opts()).await;
    assert_eq!(d_net.stats.transient_failures, 1);
    let exp_table = FamilyExperienceTable::new();
    if let Some(bytes) = store.sync().get_experience_bytes("s-p1-03-net").unwrap() {
        exp_table.decode_merge_json(&bytes);
    }
    let fam_key = family(&unreachable_url);
    assert!(
        exp_table
            .get_family(&fam_key)
            .is_none_or(|s| s.http_sufficient.trials() == 0.0 && s.total_observations == 0),
        "transport failure must leave http_sufficient & total_observations uncensored/unpolluted"
    );

    let _ = shutdown.send(());
}
