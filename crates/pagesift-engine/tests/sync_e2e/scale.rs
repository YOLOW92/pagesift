use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::State;
use axum::response::Html;
use axum::routing::get;
use pagesift_graph::ClaimStatus;
use pagesift_plan::experience::{FamilyExperienceTable, FamilyObservation};
use pagesift_plan::render::family;
use pagesift_store::{
    FreshnessPolicy, ResourceLifecycleState, ResourceRecord, ResourceRole, Store, SyncBudget,
    SyncDelta, SyncTickStats, TickCommitRequest,
};
use serde_json::json;

use crate::common::{
    arc_state, product_html, run_opts, run_opts_forced, sample_spec, spawn_server, test_scraper,
    tick_and_get,
};

fn make_record(url: &str, key: &str, now_ms: u64, has_embedded: bool) -> ResourceRecord {
    ResourceRecord {
        resource_key: key.to_string(),
        url: url.to_string(),
        canonical_url: None,
        migrated_to: None,
        parent_url: None,
        depth: 1,
        role: ResourceRole::Detail,
        state: ResourceLifecycleState::Active,
        discovered_at_ms: 1_000,
        last_checked_at_ms: Some(1_000),
        last_attempted_at_ms: Some(1_000),
        last_valid_observed_at_ms: Some(1_000),
        last_substantive_change_at_ms: Some(1_000),
        next_due_at_ms: now_ms.saturating_sub(1_000),
        etag: None,
        last_modified: None,
        last_modified_at_ms: None,
        last_modified_dynamic: false,
        content_hash: Some("h0".to_string()),
        latest_doc_id: Some("d0".to_string()),
        last_status_code: Some(200),
        consecutive_failures: 0,
        consecutive_404s: 0,
        importance_weight: 1.0,
        last_strategy: Some("lightweight_http".to_string()),
        title: Some("Item".to_string()),
        claim_count: 1,
        last_validator_contract_id: None,
        last_authoritative_status: Some(200),
        baseline_sufficient: true,
        has_embedded_json: has_embedded,
        text_signature: None,
        confirmed_gone_at_ms: None,
        learn: pagesift_store::ResourceLearnStats::default(),
    }
}

#[tokio::test]
async fn t15_source_contracts_multi_schema_isolation_on_same_url() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let omit_avail = arc_state(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/shared",
            get(|State(m): State<std::sync::Arc<AtomicUsize>>| async move {
                let ld = if m.load(Ordering::SeqCst) == 0 {
                    json!({
                        "@context": "https://schema.org",
                        "@type": "Product",
                        "sku": "SHARED-1",
                        "name": "Shared Widget",
                        "brand": "AcmeBrand",
                        "offers": { "@type": "Offer", "price": "99.00", "priceCurrency": "USD" }
                    })
                } else {
                    json!({
                        "@context": "https://schema.org",
                        "@type": "Product",
                        "sku": "SHARED-1",
                        "name": "Shared Widget",
                        "offers": { "@type": "Offer", "price": "109.00", "priceCurrency": "USD" }
                    })
                };
                Html(format!(
                    r#"<!doctype html><html><head><title>Shared Widget</title><script type="application/ld+json">{ld}</script></head><body><main><h1>Shared Widget</h1><p>Product SHARED-1 from AcmeBrand currently sells for $99.00 to $109.00 USD with standard factory coverage and regional fulfillment.</p></main></body></html>"#
                ))
            }),
        )
        .with_state(omit_avail.clone());
    let (base, shutdown) = spawn_server(app).await;
    let url = format!("{base}/shared");

    let mut spec_a = sample_spec("src-contract-a", url.clone());
    spec_a.extract_schema =
        Some(json!({ "type": "object", "properties": { "price": { "type": "string" } } }));
    spec_a.required_fields = vec!["price".to_string()];

    let mut spec_b = sample_spec("src-contract-b", url.clone());
    spec_b.extract_schema =
        Some(json!({ "type": "object", "properties": { "brand": { "type": "string" } } }));
    spec_b.required_fields = vec!["sku".to_string()];

    let cid_a = spec_a.contract_id();
    let cid_b = spec_b.contract_id();
    assert_ne!(cid_a, cid_b);

    store.sync().upsert_source(spec_a).unwrap();
    store.sync().upsert_source(spec_b).unwrap();

    // Tick both sources while `brand` & `price` are both present.
    let _ = tick_and_get(&scraper, &store, "src-contract-a", run_opts()).await;
    let _ = tick_and_get(&scraper, &store, "src-contract-b", run_opts()).await;

    // Now remove `brand` and tick Source A ONLY.
    omit_avail.store(1, Ordering::SeqCst);
    let _ = tick_and_get(&scraper, &store, "src-contract-a", run_opts_forced()).await;

    let claims = store.graph().get_resource_claims(&url).unwrap();
    // Source A's `brand` claim may be withdrawn, but Source B's `brand` claim MUST stay Active!
    let b_brand = claims
        .iter()
        .find(|c| c.claim.contract_id == cid_b && c.claim.normalized_field == "brand")
        .expect("contract B brand claim exists");
    assert_eq!(
        b_brand.status,
        ClaimStatus::Active,
        "Source A update must never withdraw Source B claims under a distinct contract_id"
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn t16_conflict_history_multi_tick_resolution_and_reappearance() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let s2_mode = arc_state(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/",
            get(|| async {
                Html(product_html(
                    "Catalog",
                    "HUB-16",
                    "1.00",
                    &[("/s1", "S1"), ("/s2", "S2")],
                ))
            }),
        )
        .route(
            "/s1",
            get(|| async { Html(product_html("Item", "W-100", "99.00", &[])) }),
        )
        .route(
            "/s2",
            get(|State(m): State<std::sync::Arc<AtomicUsize>>| async move {
                let price = match m.load(Ordering::SeqCst) {
                    0 => "129.00",
                    1 => "99.00",
                    _ => "149.00",
                };
                Html(product_html("Item", "W-100", price, &[]))
            }),
        )
        .with_state(s2_mode.clone());
    let (base, shutdown) = spawn_server(app).await;

    let spec = sample_spec("s-t16", format!("{base}/"));
    store.sync().upsert_source(spec).unwrap();

    // Tick 1 (t1): /s1 = 99.00 vs /s2 = 129.00 -> active conflict!
    let (d1, _) = tick_and_get(&scraper, &store, "s-t16", run_opts()).await;
    let t1 = d1.completed_at_ms;
    assert!(
        !d1.conflicts.is_empty(),
        "Tick 1 must report active conflict"
    );
    let (c1, _, _) = store
        .graph()
        .list_conflicts_as_of(Some("sku:W-100"), None, 20, 0)
        .unwrap();
    assert_eq!(c1.len(), 1);
    assert_eq!(c1[0].variants.len(), 2);

    tokio::time::sleep(std::time::Duration::from_millis(8)).await;

    // Tick 2 (t2): /s2 aligns to 99.00 -> active conflict resolves to 0!
    s2_mode.store(1, Ordering::SeqCst);
    let (d2, _) = tick_and_get(&scraper, &store, "s-t16", run_opts_forced()).await;
    assert!(
        d2.conflicts.is_empty(),
        "Tick 2 consensus must resolve active conflict"
    );
    let (c2_now, _, _) = store
        .graph()
        .list_conflicts_as_of(Some("sku:W-100"), None, 20, 0)
        .unwrap();
    assert!(c2_now.is_empty());

    // Historical query at t1 still reproduces the t1 conflict!
    let (c_at_t1, _, _) = store
        .graph()
        .list_conflicts_as_of(Some("sku:W-100"), Some(t1), 20, 0)
        .unwrap();
    assert_eq!(c_at_t1.len(), 1);
    assert_eq!(c_at_t1[0].variants.len(), 2);

    tokio::time::sleep(std::time::Duration::from_millis(8)).await;

    // Tick 3 (t3): /s2 diverges to 149.00 -> conflict reappears cleanly!
    s2_mode.store(2, Ordering::SeqCst);
    let (d3, _) = tick_and_get(&scraper, &store, "s-t16", run_opts_forced()).await;
    assert_eq!(d3.conflicts.len(), 1);
    let (c3_now, _, _) = store
        .graph()
        .list_conflicts_as_of(Some("sku:W-100"), None, 20, 0)
        .unwrap();
    assert_eq!(c3_now.len(), 1);

    let _ = shutdown.send(());
}

#[tokio::test]
async fn t18_adapt_strategy_embedded_json_and_order_invariance() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let app = Router::new().route(
        "/next-item",
        get(|| async {
            Html(
                r#"<!doctype html>
<html>
<head><title>Next Item</title></head>
<body>
  <main><h1>Next Item</h1><p>Hydration embedded JSON product N-10 with price $77.00 USD and full enterprise hardware specification details included.</p></main>
  <script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{"sku":"N-10","price":"77.00"}}}</script>
</body>
</html>"#
                    .to_string(),
            )
        }),
    );
    let (base, shutdown) = spawn_server(app).await;

    let mut spec = sample_spec("s-t18", format!("{base}/next-item"));
    spec.required_fields = vec!["price".to_string()];
    store.sync().upsert_source(spec.clone()).unwrap();

    // Tick 1 discovers `__NEXT_DATA__` -> `has_embedded_json = true`.
    let (_, inv1) = tick_and_get(&scraper, &store, "s-t18", run_opts()).await;
    assert!(inv1[0].has_embedded_json);

    // Tick 2 chooses `HttpWithEmbeddedJson` when validators are absent.
    let (d2, _) = tick_and_get(&scraper, &store, "s-t18", run_opts_forced()).await;
    assert!(
        d2.adapt_decisions
            .iter()
            .any(|a| a.get("strategy").and_then(|v| v.as_str()) == Some("http_with_embedded_json")),
        "tick 2 must pick HttpWithEmbeddedJson when has_embedded_json is learned: {:?}",
        d2.adapt_decisions
    );

    // Scheduler order invariance + barrier fairness (C29):
    let now_ms = 100_000;
    let table = FamilyExperienceTable::new();
    let bad_url = "https://example.com/bad/1";
    let bad_fam = family(bad_url);
    for i in 0..8 {
        table.record_observation(
            &bad_fam,
            &FamilyObservation {
                observed_at_ms: now_ms - i * 100,
                http_sufficient: Some(false),
                browser_gain: None,
                conditional_hit: None,
                embedded_json_observed: None,
                substantive_change: None,
                yielded_claims_or_links: Some(false),
                encountered_barrier: Some(true),
                cost_ms: 120.0,
                ..Default::default()
            },
        );
    }

    let r_bad = make_record(bad_url, "k-bad", now_ms, false);
    let r_good1 = make_record("https://example.com/good/1", "k-g1", now_ms, false);
    let r_good2 = make_record("https://example.com/good/2", "k-g2", now_ms, false);

    let mut sched_spec = sample_spec("sched", "https://example.com/".to_string());
    sched_spec.budget.max_requests_per_tick = 2;

    let fwd = vec![r_bad.clone(), r_good1.clone(), r_good2.clone()];
    let rev = vec![r_good2, r_good1, r_bad];
    let (c_fwd, _, _) = pagesift_engine::sync::scheduler::select_tick_candidates(
        &sched_spec,
        &fwd,
        &table,
        now_ms,
        false,
    );
    let (c_rev, _, _) = pagesift_engine::sync::scheduler::select_tick_candidates(
        &sched_spec,
        &rev,
        &table,
        now_ms,
        false,
    );

    let urls_fwd: Vec<&str> = c_fwd.iter().map(|c| c.record.url.as_str()).collect();
    let urls_rev: Vec<&str> = c_rev.iter().map(|c| c.record.url.as_str()).collect();
    assert_eq!(urls_fwd, urls_rev, "scheduler must be order-invariant");
    assert!(
        urls_fwd.iter().all(|u| u.contains("/good/")),
        "healthy items must not be starved behind barriered family: {urls_fwd:?}"
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn t19_event_truth_and_no_duplicate_gone() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let item_phase = arc_state(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/",
            get(|| async {
                Html(product_html(
                    "Root",
                    "R-19",
                    "1.00",
                    &[("/p1", "P1"), ("/p2", "P2")],
                ))
            }),
        )
        .route(
            "/p1",
            get(|State(p): State<std::sync::Arc<AtomicUsize>>| async move {
                match p.load(Ordering::SeqCst) {
                    0 => Html(product_html("P1", "P1", "10.00", &[])).into_response(),
                    1 => Html(product_html("P1", "P1", "25.00", &[])).into_response(),
                    _ => (axum::http::StatusCode::GONE, "Gone").into_response(),
                }
            }),
        )
        .route(
            "/p2",
            get(|| async { Html(product_html("P2", "P2", "30.00", &[])) }),
        )
        .with_state(item_phase.clone());
    let (base, shutdown) = spawn_server(app).await;

    use axum::response::IntoResponse;

    let spec = sample_spec("s-t19", format!("{base}/"));
    store.sync().upsert_source(spec.clone()).unwrap();

    // Tick 1: 3 resources discovered -> `added_resources` has /p1 and /p2 (+ seed).
    let (d1, _) = tick_and_get(&scraper, &store, "s-t19", run_opts()).await;
    assert_eq!(d1.added_resources.len(), 3);

    // Tick 2: /p1 price changes -> `modified_resources` has /p1.
    item_phase.store(1, Ordering::SeqCst);
    let (d2, _) = tick_and_get(&scraper, &store, "s-t19", run_opts_forced()).await;
    assert!(
        d2.modified_resources
            .iter()
            .any(|r| r.url == format!("{base}/p1"))
    );

    // Tick 3: /p1 410 Gone -> `disappeared_resources` has /p1 ONCE.
    item_phase.store(2, Ordering::SeqCst);
    let (d3, _) = tick_and_get(&scraper, &store, "s-t19", run_opts_forced()).await;
    assert_eq!(d3.disappeared_resources.len(), 1);

    // Tick 4: /p1 already Gone -> `disappeared_resources` MUST NOT re-emit it (C30)!
    let (d4, _) = tick_and_get(&scraper, &store, "s-t19", run_opts_forced()).await;
    assert!(
        d4.disappeared_resources.is_empty(),
        "already Gone resource must not be re-emitted in subsequent deltas"
    );

    // target_items = Some(1) early stop check:
    let mut s_target = sample_spec("s-t19-target", format!("{base}/"));
    s_target.budget.target_items = Some(1);
    store.sync().upsert_source(s_target).unwrap();
    let (dt, _) = tick_and_get(&scraper, &store, "s-t19-target", run_opts()).await;
    assert!(dt.stats.requests_executed <= 2);

    let _ = shutdown.send(());
}

#[test]
fn t20_scaling_inventory_performance_and_checkpoint_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let ss = store.sync();

    let mut spec = sample_spec("s-t20-scale", "https://scale.example.com/".to_string());
    spec.freshness = FreshnessPolicy::default();
    spec.budget = SyncBudget {
        max_requests_per_tick: 128,
        max_browser_renders_per_tick: 8,
        max_wall_clock_ms: 30_000,
        target_items: None,
    };
    let stored = ss.upsert_source(spec.clone()).unwrap();

    let now_ms = 500_000;
    let mut inv = Vec::with_capacity(10_000);
    for i in 0..10_000usize {
        inv.push(make_record(
            &format!("https://scale.example.com/items/{i}"),
            &format!("rk-{i}"),
            now_ms,
            false,
        ));
    }
    ss.put_inventory("s-t20-scale", &inv, now_ms).unwrap();

    // Schedule selection across 10k records respects max_requests_per_tick bound cleanly.
    let exp = FamilyExperienceTable::new();
    let (cands, unvisited_due, _) =
        pagesift_engine::sync::scheduler::select_tick_candidates(&spec, &inv, &exp, now_ms, false);
    assert_eq!(cands.len(), 128);
    assert_eq!(unvisited_due, 10_000 - 128);

    // 70 atomic ticks with deltas/checkpoints stay bounded by retention cap (`MAX_DELTAS_PER_SOURCE = 64`).
    for step in 1..=70u64 {
        let ts = now_ms + step * 100;
        let mut delta = SyncDelta {
            source_id: "s-t20-scale".to_string(),
            instance_id: stored.instance_id.clone(),
            tick_id: format!("tick-{step:03}"),
            tick_seq: step,
            started_at_ms: ts - 50,
            completed_at_ms: ts,
            coverage_status: "partial_budget_restricted".to_string(),
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
        let exp_bytes = Vec::new();
        let subjects = BTreeSet::new();
        ss.commit_tick_atomic(TickCommitRequest {
            source_id: "s-t20-scale",
            expected_instance_id: &stored.instance_id,
            expected_spec_revision: 1,
            spec_revision_used: 1,
            inventory: &mut inv,
            experience_bytes: &exp_bytes,
            delta: &mut delta,
            graph_bundles: Vec::new(),
            conflict_subjects: &subjects,
            conflicts: &[],
        })
        .unwrap();
    }

    let view = ss.build_state_view("s-t20-scale", None).unwrap().unwrap();
    assert_eq!(view.resources.len(), 10_000);
    assert_eq!(view.total_ticks, 70);
    assert!(
        view.retained_ticks <= 64 && view.retained_ticks > 0,
        "checkpoints/deltas must be retention-bounded: {}",
        view.retained_ticks
    );
}
