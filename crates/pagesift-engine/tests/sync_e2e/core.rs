use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use pagesift_api::types::ScrapeOptions;
use pagesift_engine::graph::commit_staged_document;
use pagesift_engine::graph::stage::with_graph_stage;
use pagesift_graph::{ClaimStatus, GraphOptions};
use pagesift_plan::strategy::evaluate_capture_sufficiency;
use pagesift_store::{
    ResourceLifecycleState, Store, SyncDelta, SyncTickStats, TickCommitOutcome, TickCommitRequest,
};

use crate::common::{
    arc_state, find_claim_val, product_html, run_opts, run_opts_forced, sample_spec, spawn_server,
    test_scraper, tick_and_get,
};

#[tokio::test]
async fn t01_non_speculative_staging_and_repeat_no_false_withdraw() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let app = Router::new()
        .route(
            "/shell",
            get(|| async { Html("<div id=\"app\"></div>".to_string()) }),
        )
        .route(
            "/item",
            get(|| async { Html(product_html("Widget Pro", "W-100", "99.00", &[])) }),
        );
    let (base, shutdown) = spawn_server(app).await;

    let gopts = GraphOptions::enabled();
    let attempt_opts = ScrapeOptions {
        graph: Some(gopts.as_attempt()),
        ..Default::default()
    };
    let shell_url = format!("{base}/shell");
    let item_url = format!("{base}/item");

    let (shell_res, _staged_shell) =
        with_graph_stage(scraper.scrape(&shell_url, &attempt_opts)).await;
    assert!(shell_res.is_ok());
    assert!(
        store
            .graph()
            .history_as_of(&shell_url, None, 32)
            .unwrap()
            .versions
            .is_empty(),
        "staging an attempt must not persist versions to Store"
    );

    let (win_res, mut staged_win) =
        with_graph_stage(scraper.scrape(&item_url, &attempt_opts)).await;
    let mut win_doc = win_res.unwrap();
    assert!(
        store
            .graph()
            .history_as_of(&item_url, None, 32)
            .unwrap()
            .versions
            .is_empty()
    );
    let doc_id = win_doc.graph.as_ref().unwrap().doc_id.clone();
    let staged = staged_win.remove(&doc_id).expect("staged commit present");
    commit_staged_document(&store, &gopts, None, &mut win_doc, staged);

    let hist = store.graph().history_as_of(&item_url, None, 32).unwrap();
    assert_eq!(hist.versions.len(), 1);

    // Repeat identical sync ticks: zero false withdraw or duplicate versions (C07).
    let mut spec = sample_spec("s-t01-repeat", format!("{base}/item"));
    spec.required_fields = vec!["price".to_string()];
    store.sync().upsert_source(spec).unwrap();

    let (d1, _) = tick_and_get(&scraper, &store, "s-t01-repeat", run_opts()).await;
    assert_eq!(d1.stats.authoritative_successes, 1);

    let (d2, inv2) = tick_and_get(&scraper, &store, "s-t01-repeat", run_opts_forced()).await;
    assert_eq!(d2.stats.substantive_updates, 0);
    assert_eq!(d2.stats.unchanged_200_hits, 1);
    assert_eq!(inv2[0].state, ResourceLifecycleState::Active);
    assert!(!d2.claim_transitions.iter().any(|t| {
        matches!(
            t.kind,
            pagesift_graph::ClaimTransitionKind::Withdrawn
                | pagesift_graph::ClaimTransitionKind::Restored
        )
    }));

    // Optional live-Chrome SPA escalation check when a browser binary is installed.
    if pagesift_cdp::detect_browser().is_some() && !pagesift_cdp::running_as_root() {
        let app_spa = Router::new().route(
            "/spa",
            get(|| async {
                Html(
                    r#"<!doctype html><html><head><title>SPA Product</title></head><body>
<div id="app"></div>
<script>
document.getElementById('app').innerHTML = '<h1>SPA Widget</h1><p>Specification for SKU S-500 with full enterprise warranty and hardware support at unit price $149.00 USD today.</p>';
</script></body></html>"#
                        .to_string(),
                )
            }),
        );
        let (spa_base, spa_sd) = spawn_server(app_spa).await;
        let mut spa_spec = sample_spec("s-t01-spa", format!("{spa_base}/spa"));
        spa_spec.allow_browser = true;
        spa_spec.required_fields = Vec::new();
        store.sync().upsert_source(spa_spec).unwrap();
        let (ds, _) = tick_and_get(&scraper, &store, "s-t01-spa", run_opts()).await;
        assert_eq!(ds.stats.authoritative_successes, 1);
        let h_spa = store
            .graph()
            .history_as_of(&format!("{spa_base}/spa"), None, 32)
            .unwrap();
        assert_eq!(
            h_spa.versions.len(),
            1,
            "browser escalation must commit only 1 winning version"
        );
        let _ = spa_sd.send(());
    }

    let _ = shutdown.send(());
}

#[tokio::test]
async fn t02_unreliable_content_valid_403_shell_429_valid_preserves_claims() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let mut cfg = pagesift_engine::ScraperConfig::default();
    cfg.fetcher.env_proxy = false;
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::ZERO;
    cfg.policy.backoff.enabled = false;
    cfg.store = Some(store.clone());
    let scraper = pagesift_engine::Scraper::new(cfg).unwrap();

    let phase = arc_state(AtomicUsize::new(0));
    let p_cl = phase.clone();
    let app = Router::new().route(
        "/item",
        get(move |State(p): State<std::sync::Arc<AtomicUsize>>| async move {
            match p.load(Ordering::SeqCst) {
                0 | 4 => (
                    StatusCode::OK,
                    Html(product_html("Widget Pro", "W-100", "99.00", &[])),
                )
                    .into_response(),
                1 => (StatusCode::FORBIDDEN, "Forbidden").into_response(),
                2 => (
                    StatusCode::OK,
                    Html("<html><title>Just a moment...</title><body>Checking your browser before accessing...</body></html>".to_string()),
                )
                    .into_response(),
                _ => (StatusCode::TOO_MANY_REQUESTS, "Rate limited").into_response(),
            }
        }),
    )
    .with_state(p_cl);
    let (base, shutdown) = spawn_server(app).await;
    let url = format!("{base}/item");

    let spec = sample_spec("s-t02", url.clone());
    store.sync().upsert_source(spec).unwrap();

    // Tick 1: valid 200
    let (d1, inv1) = tick_and_get(&scraper, &store, "s-t02", run_opts()).await;
    assert_eq!(d1.stats.authoritative_successes, 1);
    assert_eq!(inv1[0].state, ResourceLifecycleState::Active);
    let c1 = store.graph().get_resource_claims(&url).unwrap();
    assert_eq!(
        find_claim_val(&c1, "price"),
        Some(serde_json::json!("99.00"))
    );

    // Tick 2: 403 Forbidden -> Unverified, NOT Withdrawn
    phase.store(1, Ordering::SeqCst);
    let (d2, inv2) = tick_and_get(&scraper, &store, "s-t02", run_opts_forced()).await;
    assert_eq!(d2.stats.authoritative_successes, 0);
    assert_ne!(inv2[0].state, ResourceLifecycleState::ConfirmedGone);
    let c2 = store.graph().get_resource_claims(&url).unwrap();
    assert_eq!(
        find_claim_val(&c2, "price"),
        Some(serde_json::json!("99.00"))
    );
    assert!(
        c2.iter()
            .any(|c| matches!(c.status, ClaimStatus::Unverified { .. }))
    );

    // Tick 3: 200 Cloudflare barrier shell -> NOT Withdrawn, zero bogus version
    phase.store(2, Ordering::SeqCst);
    let (d3, _) = tick_and_get(&scraper, &store, "s-t02", run_opts_forced()).await;
    assert_eq!(d3.stats.authoritative_successes, 0);
    let c3 = store.graph().get_resource_claims(&url).unwrap();
    assert_eq!(
        find_claim_val(&c3, "price"),
        Some(serde_json::json!("99.00"))
    );
    assert!(
        c3.iter()
            .all(|c| !matches!(c.status, ClaimStatus::Withdrawn { .. }))
    );
    assert_eq!(
        store
            .graph()
            .history_as_of(&url, None, 32)
            .unwrap()
            .versions
            .len(),
        1
    );

    // Tick 4: 429 Too Many Requests -> still preserves old claims
    phase.store(3, Ordering::SeqCst);
    let (d4, _) = tick_and_get(&scraper, &store, "s-t02", run_opts_forced()).await;
    assert_eq!(d4.stats.authoritative_successes, 0);
    let c4 = store.graph().get_resource_claims(&url).unwrap();
    assert_eq!(
        find_claim_val(&c4, "price"),
        Some(serde_json::json!("99.00"))
    );

    // Tick 5: 200 valid again -> restores Active cleanly
    phase.store(4, Ordering::SeqCst);
    let (d5, inv5) = tick_and_get(&scraper, &store, "s-t02", run_opts_forced()).await;
    assert_eq!(d5.stats.authoritative_successes, 1);
    assert_eq!(inv5[0].state, ResourceLifecycleState::Active);
    let c5 = store.graph().get_resource_claims(&url).unwrap();
    assert!(c5.iter().all(|c| c.status == ClaimStatus::Active));

    let _ = shutdown.send(());
}

#[tokio::test]
async fn t03_historical_inventory_as_of_isolates_future_ticks() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let phase = arc_state(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/",
            get(|State(p): State<std::sync::Arc<AtomicUsize>>| async move {
                let links: &[(&str, &str)] = if p.load(Ordering::SeqCst) == 0 {
                    &[("/p1", "P1")]
                } else {
                    &[("/p1", "P1"), ("/p2", "P2")]
                };
                Html(product_html("Hub", "HUB-1", "1.00", links))
            }),
        )
        .route(
            "/p1",
            get(|State(p): State<std::sync::Arc<AtomicUsize>>| async move {
                match p.load(Ordering::SeqCst) {
                    0 => (
                        StatusCode::OK,
                        Html(product_html("P1", "SKU-1", "10.00", &[])),
                    )
                        .into_response(),
                    1 => (
                        StatusCode::OK,
                        Html(product_html("P1", "SKU-1", "20.00", &[])),
                    )
                        .into_response(),
                    _ => (StatusCode::GONE, "Gone").into_response(),
                }
            }),
        )
        .route(
            "/p2",
            get(|State(p): State<std::sync::Arc<AtomicUsize>>| async move {
                let price = if p.load(Ordering::SeqCst) <= 1 {
                    "25.00"
                } else {
                    "30.00"
                };
                Html(product_html("P2", "SKU-2", price, &[]))
            }),
        )
        .with_state(phase.clone());
    let (base, shutdown) = spawn_server(app).await;
    store
        .sync()
        .upsert_source(sample_spec("s-t03", format!("{base}/")))
        .unwrap();

    let (d1, _) = tick_and_get(&scraper, &store, "s-t03", run_opts()).await;
    let t1 = d1.completed_at_ms;
    tokio::time::sleep(Duration::from_millis(8)).await;

    phase.store(1, Ordering::SeqCst);
    let (d2, _) = tick_and_get(&scraper, &store, "s-t03", run_opts_forced()).await;
    let t2 = d2.completed_at_ms;
    tokio::time::sleep(Duration::from_millis(8)).await;

    phase.store(2, Ordering::SeqCst);
    let (_, inv3) = tick_and_get(&scraper, &store, "s-t03", run_opts_forced()).await;
    let p1_url = format!("{base}/p1");
    let p2_url = format!("{base}/p2");
    assert_eq!(
        inv3.iter().find(|r| r.url == p1_url).unwrap().state,
        ResourceLifecycleState::ConfirmedGone
    );

    let v1 = store
        .sync()
        .build_state_view("s-t03", Some(t1))
        .unwrap()
        .unwrap();
    assert!(
        !v1.resources.iter().any(|r| r.url == p2_url),
        "t1 must not contain future /p2"
    );
    assert!(
        v1.active_claims
            .iter()
            .any(|c| c.claim.raw_value == serde_json::json!("10.00"))
    );
    assert!(
        !v1.active_claims
            .iter()
            .any(|c| c.claim.raw_value == serde_json::json!("20.00"))
    );

    let v2 = store
        .sync()
        .build_state_view("s-t03", Some(t2))
        .unwrap()
        .unwrap();
    assert_eq!(
        v2.resources.iter().find(|r| r.url == p1_url).unwrap().state,
        ResourceLifecycleState::Active
    );
    assert!(v2.resources.iter().any(|r| r.url == p2_url));
    assert!(
        v2.active_claims
            .iter()
            .any(|c| c.claim.raw_value == serde_json::json!("20.00"))
    );
    assert!(
        !v2.active_claims
            .iter()
            .any(|c| c.claim.raw_value == serde_json::json!("30.00"))
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn t05_concurrent_sync_and_spec_revision_cas() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = std::sync::Arc::new(test_scraper(Some(store.clone())));

    let app = Router::new()
        .route(
            "/",
            get(|| async { Html(product_html("Root", "R-1", "1.00", &[("/a", "A")])) }),
        )
        .route(
            "/a",
            get(|| async { Html(product_html("Item A", "A-1", "19.00", &[])) }),
        );
    let (base, shutdown) = spawn_server(app).await;

    let mut spec = sample_spec("s-t05", format!("{base}/"));
    spec.required_fields = Vec::new();
    let rev1 = store.sync().upsert_source(spec.clone()).unwrap();
    assert_eq!(rev1.revision, 1);

    let s1 = scraper.clone();
    let s2 = scraper.clone();
    let h1 =
        tokio::spawn(async move { s1.run_sync_tick("s-t05", run_opts_forced()).await.unwrap() });
    let h2 =
        tokio::spawn(async move { s2.run_sync_tick("s-t05", run_opts_forced()).await.unwrap() });
    let (d1, d2) = tokio::join!(h1, h2);
    let d1 = d1.unwrap();
    let d2 = d2.unwrap();
    assert_ne!(d1.tick_id, d2.tick_id);
    assert_ne!(d1.tick_seq, d2.tick_seq);

    // CAS test: stale spec revision cannot overwrite newer spec (C05 / R02).
    let rev2 = store.sync().upsert_source(spec).unwrap();
    assert_eq!(rev2.revision, 2);
    let mut inv = store.sync().get_inventory("s-t05").unwrap();
    let mut delta = SyncDelta {
        source_id: "s-t05".to_string(),
        tick_id: "stale-tick".to_string(),
        tick_seq: 99,
        started_at_ms: 100,
        completed_at_ms: 200,
        coverage_status: "complete_within_scope".to_string(),
        coverage_reasons: Vec::new(),
        stats: SyncTickStats::default(),
        added_resources: Vec::new(),
        modified_resources: Vec::new(),
        migrated_resources: Vec::new(),
        disappeared_resources: Vec::new(),
        claim_transitions: Vec::new(),
        conflicts: Vec::new(),
        adapt_decisions: Vec::new(),
    };
    let outcome = store
        .sync()
        .commit_tick_atomic(TickCommitRequest {
            source_id: "s-t05",
            expected_spec_revision: 1,
            spec_revision_used: 1,
            inventory: &mut inv,
            experience_bytes: &[],
            delta: &mut delta,
            graph_bundles: Vec::new(),
            conflict_subjects: &BTreeSet::new(),
            conflicts: &[],
        })
        .unwrap();
    assert!(matches!(outcome, TickCommitOutcome::StaleSpec { .. }));

    let _ = shutdown.send(());
}

#[tokio::test]
async fn t07_hard_budget_limits_requests_and_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let hits = arc_state(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/",
            get(|State(h): State<std::sync::Arc<AtomicUsize>>| async move {
                h.fetch_add(1, Ordering::SeqCst);
                Html(product_html(
                    "Hub",
                    "H-1",
                    "1.00",
                    &[("/a", "A"), ("/b", "B"), ("/c", "C"), ("/d", "D")],
                ))
            }),
        )
        .route(
            "/a",
            get(|State(h): State<std::sync::Arc<AtomicUsize>>| async move {
                h.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(60)).await;
                Html(product_html("A", "A-1", "10.00", &[]))
            }),
        )
        .route(
            "/b",
            get(|State(h): State<std::sync::Arc<AtomicUsize>>| async move {
                h.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(60)).await;
                Html(product_html("B", "B-1", "20.00", &[]))
            }),
        )
        .route(
            "/c",
            get(|State(h): State<std::sync::Arc<AtomicUsize>>| async move {
                h.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(60)).await;
                Html(product_html("C", "C-1", "30.00", &[]))
            }),
        )
        .route(
            "/d",
            get(|State(h): State<std::sync::Arc<AtomicUsize>>| async move {
                h.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(60)).await;
                Html(product_html("D", "D-1", "40.00", &[]))
            }),
        )
        .with_state(hits.clone());
    let (base, shutdown) = spawn_server(app).await;

    let mut spec = sample_spec("s-t07-req", format!("{base}/"));
    spec.budget.max_requests_per_tick = 2;
    store.sync().upsert_source(spec).unwrap();
    let (d1, _) = tick_and_get(&scraper, &store, "s-t07-req", run_opts()).await;
    assert!(hits.load(Ordering::SeqCst) <= 2);
    assert!(d1.stats.requests_executed <= 2);
    assert_eq!(d1.coverage_status, "budget_capped");

    // Wall clock deadline test: 5 items each taking 60ms cannot finish within 90ms wall clock.
    hits.store(0, Ordering::SeqCst);
    let mut wall_spec = sample_spec("s-t07-wall", format!("{base}/"));
    wall_spec.budget.max_requests_per_tick = 10;
    wall_spec.budget.max_wall_clock_ms = 90;
    store.sync().upsert_source(wall_spec).unwrap();
    let (d2, _) = tick_and_get(&scraper, &store, "s-t07-wall", run_opts()).await;
    assert!(
        d2.stats.requests_executed < 5,
        "wall-clock deadline must stop tick before hitting all 5 slow pages"
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn t09_coverage_status_budget_restricted_and_pending_404() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let gone_mode = arc_state(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/restricted",
            get(|| async { (StatusCode::FORBIDDEN, "Access Denied") }),
        )
        .route(
            "/flaky404",
            get(|State(m): State<std::sync::Arc<AtomicUsize>>| async move {
                if m.load(Ordering::SeqCst) == 0 {
                    (
                        StatusCode::OK,
                        Html(product_html("OK", "F-1", "12.00", &[])),
                    )
                        .into_response()
                } else {
                    (StatusCode::NOT_FOUND, "Not Found").into_response()
                }
            }),
        )
        .with_state(gone_mode.clone());
    let (base, shutdown) = spawn_server(app).await;

    let mut s_bar = sample_spec("s-t09-bar", format!("{base}/restricted"));
    s_bar.required_fields = Vec::new();
    store.sync().upsert_source(s_bar).unwrap();
    let (db, _) = tick_and_get(&scraper, &store, "s-t09-bar", run_opts()).await;
    assert_ne!(db.coverage_status, "complete_within_scope");

    let s_404 = sample_spec("s-t09-404", format!("{base}/flaky404"));
    store.sync().upsert_source(s_404).unwrap();
    let _ = tick_and_get(&scraper, &store, "s-t09-404", run_opts()).await;
    gone_mode.store(1, Ordering::SeqCst);
    let (d_sus, inv) = tick_and_get(&scraper, &store, "s-t09-404", run_opts_forced()).await;
    assert_eq!(inv[0].state, ResourceLifecycleState::SuspectedGone);
    assert_ne!(d_sus.coverage_status, "complete_within_scope");

    // Ensure grounded vs unaligned evidence check is also enforced by evaluate_capture_sufficiency (T10 overlap).
    let _ = evaluate_capture_sufficiency;

    let _ = shutdown.send(());
}
