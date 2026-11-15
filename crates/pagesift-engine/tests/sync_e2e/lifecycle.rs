use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use pagesift_graph::ClaimStatus;
use pagesift_plan::experience::FamilyExperienceTable;
use pagesift_plan::strategy::{CaptureGoal, CaptureSignals, evaluate_capture_sufficiency};
use pagesift_store::{ResourceLifecycleState, Store};
use serde_json::json;

use crate::common::{
    arc_state, find_claim_val, product_html, run_opts, run_opts_forced, sample_spec, spawn_server,
    test_scraper, tick_and_get,
};

type InmLog = Arc<Mutex<Vec<Option<String>>>>;

#[tokio::test]
async fn t10_capture_sufficiency_grounded_evidence_and_contract_304_guard() {
    // Part A: ungrounded required fields fail sufficiency even when raw field exists, and 304 fails if contract mismatches.
    let goal = CaptureGoal {
        min_markdown_chars: 20,
        required_fields: vec!["price".to_string()],
        require_evidence_grounded: true,
        expect_links_for_hub: false,
    };
    let ungrounded = evaluate_capture_sufficiency(
        &goal,
        &CaptureSignals {
            status_code: 200,
            markdown_chars: 200,
            extracted_fields_present: vec!["price".to_string()],
            grounded_fields_present: Vec::new(),
            grounded_evidence_count: 0,
            ..Default::default()
        },
    );
    assert!(!ungrounded.sufficient);
    assert_eq!(ungrounded.ungrounded_fields, vec!["price".to_string()]);

    let stale_304 = evaluate_capture_sufficiency(
        &goal,
        &CaptureSignals {
            status_code: 304,
            not_modified_304: true,
            baseline_matches_contract: false,
            ..Default::default()
        },
    );
    assert!(!stale_304.sufficient);

    // Part B: live sync preserves old claims on incomplete capture and disables conditional 304 when contract changes.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let inm_seen = arc_state(Mutex::new(Vec::<Option<String>>::new()));
    let body_mode = arc_state(AtomicUsize::new(0));
    let app = Router::new().route(
        "/item",
        get(
            |State((seen, mode)): State<(InmLog, Arc<AtomicUsize>)>, headers: HeaderMap| async move {
                let inm = headers
                    .get(header::IF_NONE_MATCH)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                seen.lock().unwrap().push(inm.clone());
                if mode.load(Ordering::SeqCst) == 1 {
                    // Missing required `sku`, boilerplate text
                    return (
                        StatusCode::OK,
                        Html(
                            "<!doctype html><html><head><title>Partial</title></head><body><main><p>Short notice without required catalog details.</p></main></body></html>"
                                .to_string(),
                        ),
                    )
                        .into_response();
                }
                if inm.as_deref() == Some("\"v1\"") {
                    return (StatusCode::NOT_MODIFIED, "").into_response();
                }
                (
                    StatusCode::OK,
                    [(header::ETAG, "\"v1\"")],
                    Html(product_html("Widget Pro", "W-100", "99.00", &[])),
                )
                    .into_response()
            },
        ),
    )
    .with_state((inm_seen.clone(), body_mode.clone()));
    let (base, shutdown) = spawn_server(app).await;
    let url = format!("{base}/item");

    let mut spec = sample_spec("s-t10", url.clone());
    spec.required_fields = vec!["price".to_string(), "sku".to_string()];
    store.sync().upsert_source(spec.clone()).unwrap();

    let (d1, _) = tick_and_get(&scraper, &store, "s-t10", run_opts()).await;
    assert_eq!(d1.stats.authoritative_successes, 1);

    // Contract changes -> conditional 304 must NOT be sent (no If-None-Match on req 2).
    spec.extract_schema = Some(json!({
        "type": "object",
        "properties": {
            "price": { "type": "string" },
            "sku": { "type": "string" },
            "brand": { "type": "string" }
        }
    }));
    spec.revision = 2;
    let updated_spec = store.sync().upsert_source(spec).unwrap();
    assert!(updated_spec.revision >= 2);
    let (d2, _) = tick_and_get(&scraper, &store, "s-t10", run_opts_forced()).await;
    assert_eq!(d2.stats.conditional_304_hits, 0);
    {
        let log = inm_seen.lock().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(
            log[1], None,
            "contract change must suppress stale If-None-Match"
        );
    }

    // Mode 1: insufficient capture must NOT withdraw existing `price` claim.
    body_mode.store(1, Ordering::SeqCst);
    let (d3, _) = tick_and_get(&scraper, &store, "s-t10", run_opts_forced()).await;
    assert_eq!(d3.stats.insufficient_captures, 1);
    let claims = store.graph().get_resource_claims(&url).unwrap();
    assert_eq!(find_claim_val(&claims, "price"), Some(json!("99.00")));
    assert!(
        claims
            .iter()
            .any(|c| matches!(c.status, ClaimStatus::Unverified { .. })),
        "prior claim stays Unverified on insufficient capture"
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn t11_feedback_accuracy_failure_and_repeat_200() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let app = Router::new()
        .route(
            "/",
            get(|| async {
                Html(product_html(
                    "Catalog",
                    "CAT-1",
                    "1.00",
                    &[("/err", "Err"), ("/same", "Same")],
                ))
            }),
        )
        .route(
            "/err",
            get(|| async { (StatusCode::SERVICE_UNAVAILABLE, "Down") }),
        )
        .route(
            "/same",
            get(|| async { Html(product_html("Stable", "S-1", "55.00", &[])) }),
        );
    let (base, shutdown) = spawn_server(app).await;

    let mut spec = sample_spec("s-t11", format!("{base}/"));
    spec.required_fields = vec!["price".to_string()];
    store.sync().upsert_source(spec).unwrap();

    let (d1, _) = tick_and_get(&scraper, &store, "s-t11", run_opts()).await;
    assert_eq!(d1.stats.transient_failures, 1);

    let (d2, _) = tick_and_get(&scraper, &store, "s-t11", run_opts_forced()).await;
    assert_eq!(d2.stats.substantive_updates, 0);
    assert!(d2.stats.unchanged_200_hits >= 1);

    let exp_bytes = store.sync().get_experience_bytes("s-t11").unwrap().unwrap();
    let table = FamilyExperienceTable::new();
    assert!(table.decode_merge_json(&exp_bytes));
    let fams = table.list_families();
    assert!(!fams.is_empty());
    // Repeat unchanged 200 must never record spurious substantive change intervals.
    for f in &fams {
        assert!(
            f.substantive_change.changed_intervals_secs.is_empty(),
            "identical repeat 200 must not learn substantive change intervals on {}",
            f.family
        );
    }

    let _ = shutdown.send(());
}

#[tokio::test]
async fn t12_lifecycle_404_503_404_recovery_and_tombstone_cooldown() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let phase = arc_state(AtomicUsize::new(0));
    let hits = arc_state(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/item",
            get(
                |State((p, h)): State<(Arc<AtomicUsize>, Arc<AtomicUsize>)>| async move {
                    h.fetch_add(1, Ordering::SeqCst);
                    match p.load(Ordering::SeqCst) {
                        0 | 4 => (
                            StatusCode::OK,
                            Html(product_html("Lifecycle Item", "L-1", "40.00", &[])),
                        )
                            .into_response(),
                        1 | 3 => (StatusCode::NOT_FOUND, "Not Found").into_response(),
                        _ => (StatusCode::SERVICE_UNAVAILABLE, "Busy").into_response(),
                    }
                },
            ),
        )
        .with_state((phase.clone(), hits.clone()));
    let (base, shutdown) = spawn_server(app).await;
    let url = format!("{base}/item");

    let mut spec = sample_spec("s-t12", url.clone());
    spec.freshness.confirm_gone_after_404s = 2;
    spec.freshness.default_interval_secs = 3600;
    spec.freshness.min_interval_secs = 60;
    store.sync().upsert_source(spec).unwrap();

    // Step 1: 200 Active
    let (_, inv1) = tick_and_get(&scraper, &store, "s-t12", run_opts()).await;
    assert_eq!(inv1[0].state, ResourceLifecycleState::Active);

    // Step 2: 404 -> SuspectedGone (consecutive_404s = 1)
    phase.store(1, Ordering::SeqCst);
    let (_, inv2) = tick_and_get(&scraper, &store, "s-t12", run_opts_forced()).await;
    assert_eq!(inv2[0].state, ResourceLifecycleState::SuspectedGone);
    assert_eq!(inv2[0].consecutive_404s, 1);

    // Step 3: 503 -> TransientFailure, MUST reset consecutive_404s to 0 so 404->503->404 does not prematurely confirm Gone (C18)
    phase.store(2, Ordering::SeqCst);
    let (_, inv3) = tick_and_get(&scraper, &store, "s-t12", run_opts_forced()).await;
    assert_eq!(inv3[0].state, ResourceLifecycleState::TransientFailure);
    assert_eq!(inv3[0].consecutive_404s, 0);

    // Step 4a: 404 after 503 -> SuspectedGone (NOT ConfirmedGone yet!)
    phase.store(3, Ordering::SeqCst);
    let (_, inv4a) = tick_and_get(&scraper, &store, "s-t12", run_opts_forced()).await;
    assert_eq!(inv4a[0].state, ResourceLifecycleState::SuspectedGone);
    assert_eq!(inv4a[0].consecutive_404s, 1);

    // Step 4b: second consecutive 404 -> ConfirmedGone (consecutive_404s = 2)
    let (d4, inv4) = tick_and_get(&scraper, &store, "s-t12", run_opts_forced()).await;
    assert_eq!(inv4[0].state, ResourceLifecycleState::ConfirmedGone);
    assert_eq!(inv4[0].consecutive_404s, 2);
    assert!(inv4[0].confirmed_gone_at_ms.is_some());
    assert!(d4.disappeared_resources.iter().any(|r| r.url == url));

    // Step 5: both unforced and forced ticks respect tombstone cooldown (C20: hits must not increase).
    phase.store(4, Ordering::SeqCst);
    let hits_before_skip = hits.load(Ordering::SeqCst);
    let (_, _) = tick_and_get(&scraper, &store, "s-t12", run_opts()).await;
    let (_, _) = tick_and_get(&scraper, &store, "s-t12", run_opts_forced()).await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        hits_before_skip,
        "Gone tombstone cooldown must skip re-fetch even with force_all before cooldown expires"
    );

    // Step 6: after Gone cooldown expires + server recovers to 200, next tick restores Active + claims!
    let mut inv_expired = store.sync().get_inventory("s-t12").unwrap();
    inv_expired[0].next_due_at_ms = 1;
    store
        .sync()
        .put_inventory("s-t12", &inv_expired, 2)
        .unwrap();
    let (d6, inv6) = tick_and_get(&scraper, &store, "s-t12", run_opts()).await;
    assert_eq!(inv6[0].state, ResourceLifecycleState::Active);
    assert_eq!(inv6[0].consecutive_404s, 0);
    assert!(inv6[0].confirmed_gone_at_ms.is_none());
    assert_eq!(d6.disappeared_resources.len(), 0);
    let claims = store.graph().get_resource_claims(&url).unwrap();
    assert!(claims.iter().any(|c| c.status == ClaimStatus::Active));

    let _ = shutdown.send(());
}

#[tokio::test]
async fn t13_scope_pruning_and_redirect_chain_identity() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let ext_app = Router::new().route(
        "/external",
        get(|| async { Html(product_html("External", "E-1", "9.00", &[])) }),
    );
    let (ext_base, ext_sd) = spawn_server(ext_app).await;
    let ext_url = format!("{ext_base}/external");

    let app = Router::new()
        .route(
            "/",
            get(|| async {
                Html(product_html(
                    "Root",
                    "R-1",
                    "1.00",
                    &[
                        ("/docs/a", "DocA"),
                        ("/blog/b", "BlogB"),
                        ("/old-perm", "OldPerm"),
                        ("/tmp-redir", "Tmp"),
                        ("/off-domain", "OffDomain"),
                    ],
                ))
            }),
        )
        .route(
            "/docs/a",
            get(|| async { Html(product_html("Doc A", "DA-1", "11.00", &[])) }),
        )
        .route(
            "/blog/b",
            get(|| async { Html(product_html("Blog B", "BB-1", "12.00", &[])) }),
        )
        .route(
            "/old-perm",
            get(|| async {
                (
                    StatusCode::MOVED_PERMANENTLY,
                    [(header::LOCATION, "/mid-perm")],
                    "",
                )
            }),
        )
        .route(
            "/mid-perm",
            get(|| async {
                (
                    StatusCode::MOVED_PERMANENTLY,
                    [(header::LOCATION, "/new-perm")],
                    "",
                )
            }),
        )
        .route(
            "/new-perm",
            get(|| async { Html(product_html("New Perm", "NP-1", "15.00", &[])) }),
        )
        .route(
            "/tmp-redir",
            get(|| async { (StatusCode::FOUND, [(header::LOCATION, "/mid-perm")], "") }),
        )
        .route(
            "/off-domain",
            get(move |State(target): State<String>| async move {
                (
                    StatusCode::MOVED_PERMANENTLY,
                    [(header::LOCATION, target)],
                    "",
                )
            }),
        )
        .with_state(ext_url);
    let (base, shutdown) = spawn_server(app).await;

    let mut spec = sample_spec("s-t13", format!("{base}/"));
    spec.required_fields = vec!["price".to_string()];
    store.sync().upsert_source(spec.clone()).unwrap();

    let (d1, inv1) = tick_and_get(&scraper, &store, "s-t13", run_opts()).await;
    assert!(inv1.iter().any(|r| r.url == format!("{base}/blog/b")));

    // 301 -> 301 chain migrates /old-perm -> /new-perm cleanly.
    let old_rec = inv1
        .iter()
        .find(|r| r.url == format!("{base}/old-perm"))
        .unwrap();
    assert_eq!(old_rec.state, ResourceLifecycleState::Migrated);
    assert_eq!(
        old_rec.migrated_to.as_deref(),
        Some(format!("{base}/new-perm").as_str())
    );
    assert!(
        d1.migrated_resources
            .iter()
            .any(|m| m.url == format!("{base}/old-perm"))
    );

    // 302 -> 301 breaks permanent migration chain (R03).
    let tmp_rec = inv1
        .iter()
        .find(|r| r.url == format!("{base}/tmp-redir"))
        .unwrap();
    assert_ne!(tmp_rec.state, ResourceLifecycleState::Migrated);
    assert!(tmp_rec.migrated_to.is_none());

    // Off-domain 301 must not migrate into external authority.
    let off_rec = inv1
        .iter()
        .find(|r| r.url == format!("{base}/off-domain"))
        .unwrap();
    assert!(off_rec.migrated_to.is_none());

    // Updating excludePaths prunes /blog/b from active inventory on next tick (C21).
    spec.exclude_paths = vec!["/blog/".to_string()];
    store.sync().upsert_source(spec).unwrap();
    let (_, inv2) = tick_and_get(&scraper, &store, "s-t13", run_opts()).await;
    assert!(
        !inv2.iter().any(|r| r.url == format!("{base}/blog/b")),
        "updated excludePaths must prune stale /blog/b from inventory"
    );

    let _ = shutdown.send(());
    let _ = ext_sd.send(());
}

#[tokio::test]
async fn t14_validator_overwrite_clears_stale_etag() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let mode = arc_state(AtomicUsize::new(0));
    let seen_inm = arc_state(Mutex::new(Vec::<Option<String>>::new()));
    let app = Router::new()
        .route(
            "/doc",
            get(
                |State((m, seen)): State<(Arc<AtomicUsize>, InmLog)>, headers: HeaderMap| async move {
                    let inm = headers
                        .get(header::IF_NONE_MATCH)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    seen.lock().unwrap().push(inm);
                    if m.load(Ordering::SeqCst) == 0 {
                        (
                            StatusCode::OK,
                            [(header::ETAG, "\"v1\"")],
                            Html(product_html("V1", "E-1", "10.00", &[])),
                        )
                            .into_response()
                    } else {
                        // Response deliberately omits ETag and Last-Modified headers (C23).
                        (
                            StatusCode::OK,
                            Html(product_html("V2", "E-1", "20.00", &[])),
                        )
                            .into_response()
                    }
                },
            ),
        )
        .with_state((mode.clone(), seen_inm.clone()));
    let (base, shutdown) = spawn_server(app).await;

    let spec = sample_spec("s-t14", format!("{base}/doc"));
    store.sync().upsert_source(spec).unwrap();

    // Tick 1: receives ETag "v1"
    let (_, inv1) = tick_and_get(&scraper, &store, "s-t14", run_opts()).await;
    assert_eq!(inv1[0].etag.as_deref(), Some("\"v1\""));

    // Tick 2: returns 200 without ETag -> stale ETag must be cleared to None (C23).
    mode.store(1, Ordering::SeqCst);
    let (_, inv2) = tick_and_get(&scraper, &store, "s-t14", run_opts_forced()).await;
    assert_eq!(inv2[0].etag, None);
    assert_eq!(inv2[0].last_modified, None);

    // Tick 3: server must NOT receive old `If-None-Match: "v1"`.
    let (_, _) = tick_and_get(&scraper, &store, "s-t14", run_opts_forced()).await;
    let log = seen_inm.lock().unwrap();
    assert_eq!(log.len(), 3);
    assert_eq!(
        log[2], None,
        "cleared validator must not resend stale ETag v1"
    );

    let _ = shutdown.send(());
}
