//! P1-02 (Monitor delete vs in-flight check/persist/webhook races) and
//! P2-04 (revoked account background Monitor ownership & quota fallback) regression tests.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::http::StatusCode;
use pagesift_store::Store;
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify};

use super::tests::{call_as, statuses};
use crate::engine::{BoxFuture, Engine};
use crate::error::ApiError;
use crate::limits::KeyLimits;
use crate::server::{Config, router};
use crate::types::{Document, Metadata, ScrapeOptions};

struct BarrierEngine {
    hits: Arc<Mutex<Vec<String>>>,
    first_entered: Arc<Notify>,
    release_first: Arc<Notify>,
    first_hit: AtomicUsize,
}

impl BarrierEngine {
    fn new() -> (Arc<Self>, Arc<Notify>, Arc<Notify>) {
        let first_entered = Arc::new(Notify::new());
        let release_first = Arc::new(Notify::new());
        let eng = Arc::new(Self {
            hits: Arc::new(Mutex::new(Vec::new())),
            first_entered: first_entered.clone(),
            release_first: release_first.clone(),
            first_hit: AtomicUsize::new(0),
        });
        (eng, first_entered, release_first)
    }
}

impl Engine for BarrierEngine {
    fn scrape<'a>(
        &'a self,
        url: &'a str,
        _opts: &'a ScrapeOptions,
    ) -> BoxFuture<'a, Result<Document, ApiError>> {
        Box::pin(async move {
            self.hits.lock().await.push(url.to_owned());
            if self.first_hit.fetch_add(1, Ordering::SeqCst) == 0 {
                self.first_entered.notify_waiters();
                self.release_first.notified().await;
            }
            Ok(Document {
                markdown: Some(format!("# Body for {url}")),
                metadata: Metadata {
                    source_url: Some(url.into()),
                    status_code: 200,
                    ..Default::default()
                },
                ..Default::default()
            })
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p1_02_delete_during_in_flight_check_stops_subsequent_urls_and_never_resurrects() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let (engine, first_entered, release_first) = BarrierEngine::new();
    let api = router(
        engine.clone(),
        Config {
            store: Some(store.clone()),
            ..Default::default()
        },
    );

    let (st, created) = call_as(
        &api,
        None,
        "POST",
        "/v2/monitor",
        Some(json!({
            "targets": [{
                "type": "scrape",
                "urls": ["https://site.test/u1", "https://site.test/u2"]
            }]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{created}");
    let id = created["data"]["id"].as_str().unwrap().to_owned();

    // Wait deterministically until the scheduler's initial check enters URL #1.
    tokio::time::timeout(std::time::Duration::from_secs(5), first_entered.notified())
        .await
        .expect("initial check should enter first URL");

    // Delete the monitor while URL #1 is in flight.
    let (del_st, del_body) =
        call_as(&api, None, "DELETE", &format!("/v2/monitor/{id}"), None).await;
    assert_eq!(del_st, StatusCode::OK, "{del_body}");

    // Unblock URL #1 so the in-flight check finishes / exits.
    release_first.notify_waiters();
    // Give the background task a brief deterministic yield to unwind.
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let hits = engine.hits.lock().await.clone();
    assert_eq!(
        hits.len(),
        1,
        "deleted monitor must NOT keep scraping subsequent URLs after DELETE; hits={hits:?}"
    );
    assert!(
        store
            .tasks()
            .kv_get(format!("monitor/{id}"))
            .unwrap()
            .is_none(),
        "deleted monitor must NOT be resurrected in Store"
    );
}

#[test]
fn p2_04_revoked_account_monitor_must_not_run_under_default_limits_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let rt1 = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    let (id, owner) = rt1.block_on(async {
        let store = Store::open_dir(dir.path()).unwrap();
        let script = Arc::new(super::tests::Script::default());
        script.page(super::tests::A, 200, "# A\n\nInitial body.");
        let mut limits = HashMap::new();
        limits.insert(
            "sk-revoked".to_owned(),
            KeyLimits::new().with_pages_per_day(1),
        );
        limits.insert(
            "sk-other".to_owned(),
            KeyLimits::new().with_pages_per_day(1),
        );
        let api = super::tests::scripted(
            &script,
            Config {
                key_limits: limits,
                // Default limits are unlimited (typical server config).
                default_limits: KeyLimits::new(),
                store: Some(store.clone()),
                ..Default::default()
            },
        );
        let (st, created) = call_as(
            &api,
            Some("sk-revoked"),
            "POST",
            "/v2/monitor",
            Some(json!({
                "targets": [{ "type": "scrape", "urls": [super::tests::A] }]
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{created}");
        let id = created["data"]["id"].as_str().unwrap().to_owned();
        // Force it due immediately on next boot so scheduler would fire immediately if still active.
        let key = format!("monitor/{id}");
        let raw = store.tasks().kv_get(&key).unwrap().unwrap();
        let mut m: super::MonitorState = serde_json::from_slice(&raw).unwrap();
        for p in m.pages.values_mut() {
            p.next_due = 0.0;
        }
        store
            .tasks()
            .kv_put(&key, &serde_json::to_vec(&m).unwrap())
            .unwrap();
        (id, m.owner)
    });
    drop(rt1);

    // Boot 2: `sk-revoked` has been removed from configuration; only `sk-other` remains.
    let rt2 = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt2.block_on(async {
        let store = Store::open_dir(dir.path()).unwrap();
        let script = Arc::new(super::tests::Script::default());
        script.page(super::tests::A, 200, "# A\n\nUpdated after revocation.");
        let mut limits = HashMap::new();
        limits.insert(
            "sk-other".to_owned(),
            KeyLimits::new().with_pages_per_day(1),
        );
        let _api = super::tests::scripted(
            &script,
            Config {
                key_limits: limits,
                default_limits: KeyLimits::new(),
                store: Some(store.clone()),
                ..Default::default()
            },
        );
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert_eq!(
            script.scrapes.load(Ordering::SeqCst),
            0,
            "monitor belonging to revoked account {owner} must NOT run after restart"
        );
        let persisted: super::MonitorState = serde_json::from_slice(
            &store
                .tasks()
                .kv_get(format!("monitor/{id}"))
                .unwrap()
                .expect("revoked monitor data must be preserved"),
        )
        .unwrap();
        assert_eq!(
            persisted.status, "paused",
            "revoked monitor must be marked paused with diagnostic status"
        );
    });
    drop(rt2);
    let _ = Value::Null;
    let _ = statuses;
}

/// Starts a local loopback HTTP webhook receiver that records every delivered payload.
async fn spawn_webhook_sink() -> (String, Arc<AtomicUsize>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let c = c.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                c.fetch_add(1, Ordering::SeqCst);
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await;
            });
        }
    });
    (format!("http://{addr}/hook"), count)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p1_02_delete_before_persist_and_during_persist_never_resurrects_or_notifies() {
    use crate::monitor::MonitorTestHooks;
    use std::sync::{Condvar, Mutex as StdMutex};

    // Scenario A: DELETE arrives while check is paused right before persistence (`before_persist`).
    {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_dir(dir.path()).unwrap();
        let (hook_url, webhook_hits) = spawn_webhook_sink().await;
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let e2 = entered.clone();
        let r2 = release.clone();
        let script = Arc::new(super::tests::Script::default());
        script.page(super::tests::A, 200, "# Page A\n\nBody v1.");
        let mut cfg = Config {
            store: Some(store.clone()),
            monitor_hooks: Some(MonitorTestHooks {
                before_persist: Some(Arc::new(move |_id| {
                    let e = e2.clone();
                    let r = r2.clone();
                    Box::pin(async move {
                        e.notify_waiters();
                        r.notified().await;
                    })
                })),
                ..Default::default()
            }),
            ..Default::default()
        };
        cfg.webhook.ssrf.allow_private_network = true;
        let api = super::tests::scripted(&script, cfg);
        let (st, created) = call_as(
            &api,
            None,
            "POST",
            "/v2/monitor",
            Some(json!({
                "targets": [{ "type": "scrape", "urls": [super::tests::A] }],
                "webhook": { "url": hook_url }
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{created}");
        let id = created["data"]["id"].as_str().unwrap().to_owned();

        // Wait deterministically until check finishes scraping and pauses before persist.
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .expect("check should reach before_persist");

        let (del_st, _) = call_as(&api, None, "DELETE", &format!("/v2/monitor/{id}"), None).await;
        assert_eq!(del_st, StatusCode::OK);
        release.notify_waiters();

        // Concurrent /run after DELETE must return 404.
        let (run_st, _) = call_as(&api, None, "POST", &format!("/v2/monitor/{id}/run"), None).await;
        assert_eq!(run_st, StatusCode::NOT_FOUND);
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        assert!(
            store
                .tasks()
                .kv_get(format!("monitor/{id}"))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            webhook_hits.load(Ordering::SeqCst),
            0,
            "deleted monitor must not deliver ghost webhooks"
        );
    }

    // Scenario B: DELETE arrives WHILE `spawn_blocking` persist is mid-flight (`during_persist`),
    // AND outer `run_now` future is cancelled/dropped mid-persist. DELETE must wait on `persist_lock`
    // and delete AFTER the physical write finishes so the monitor NEVER resurrects.
    {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_dir(dir.path()).unwrap();
        let (hook_url, webhook_hits) = spawn_webhook_sink().await;
        let entered_persist = Arc::new(Notify::new());
        let gate = Arc::new((StdMutex::new(false), Condvar::new()));
        let persist_calls = Arc::new(AtomicUsize::new(0));
        let ep2 = entered_persist.clone();
        let g2 = gate.clone();
        let pc2 = persist_calls.clone();

        let script = Arc::new(super::tests::Script::default());
        script.page(super::tests::A, 200, "# Page A\n\nBody v1.");
        let mut cfg = Config {
            store: Some(store.clone()),
            // Pause scheduler via leader=false so only our explicit `run_now` triggers the 2nd persist.
            monitor_leader: Some(Arc::new(|| false)),
            monitor_hooks: Some(MonitorTestHooks {
                during_persist: Some(Arc::new(move |_id| {
                    // Call #0 is `create()`; block on Call #1 (`run_now` check persist).
                    if pc2.fetch_add(1, Ordering::SeqCst) == 1 {
                        ep2.notify_waiters();
                        let (lock, cv) = &*g2;
                        let mut released = lock.lock().unwrap();
                        while !*released {
                            released = cv.wait(released).unwrap();
                        }
                    }
                })),
                ..Default::default()
            }),
            ..Default::default()
        };
        cfg.webhook.ssrf.allow_private_network = true;
        let api = super::tests::scripted(&script, cfg);
        let (st, created) = call_as(
            &api,
            None,
            "POST",
            "/v2/monitor",
            Some(json!({
                "targets": [{ "type": "scrape", "urls": [super::tests::A] }],
                "webhook": { "url": hook_url }
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{created}");
        let id = created["data"]["id"].as_str().unwrap().to_owned();

        // Start manual `/run` in a task and wait until it enters physical persist.
        let api_run = api.clone();
        let id_run = id.clone();
        let run_handle = tokio::spawn(async move {
            call_as(
                &api_run,
                None,
                "POST",
                &format!("/v2/monitor/{id_run}/run"),
                None,
            )
            .await
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            entered_persist.notified(),
        )
        .await
        .expect("run_now should enter during_persist");

        // Cancel/abort the outer `run_now` future while blocking persist still holds `persist_guard`.
        run_handle.abort();
        let _ = run_handle.await;

        // Fire DELETE concurrently: it must wait for the in-flight physical write before kv_delete.
        let api_del = api.clone();
        let id_del = id.clone();
        let mut del_task = tokio::spawn(async move {
            call_as(
                &api_del,
                None,
                "DELETE",
                &format!("/v2/monitor/{id_del}"),
                None,
            )
            .await
        });

        // Verify DELETE is still waiting while physical persist has not finished yet.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(60), &mut del_task)
                .await
                .is_err(),
            "DELETE must wait on persist_lock until in-flight physical write finishes"
        );

        // Release the physical write -> persist finishes, then DELETE runs kv_delete and returns 200.
        {
            let (lock, cv) = &*gate;
            *lock.lock().unwrap() = true;
            cv.notify_all();
        }
        let (del_st, del_body) = tokio::time::timeout(std::time::Duration::from_secs(5), del_task)
            .await
            .expect("DELETE should finish once persist releases persist_lock")
            .unwrap();
        assert_eq!(del_st, StatusCode::OK, "{del_body}");
        assert!(
            store
                .tasks()
                .kv_get(format!("monitor/{id}"))
                .unwrap()
                .is_none(),
            "monitor must NEVER resurrect in Store after cancelled run_now + concurrent DELETE"
        );
        assert_eq!(
            webhook_hits.load(Ordering::SeqCst),
            0,
            "cancelled/deleted monitor must never send webhook"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p1_02_delete_storage_failure_is_not_200_and_is_retryable_and_isolated_across_monitors() {
    use crate::monitor::MonitorTestHooks;
    use std::sync::atomic::AtomicBool;

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let fail_once = Arc::new(AtomicBool::new(true));
    let f2 = fail_once.clone();

    let (engine, first_entered, release_first) = BarrierEngine::new();
    let api = router(
        engine.clone(),
        Config {
            store: Some(store.clone()),
            monitor_hooks: Some(MonitorTestHooks {
                fail_delete: Some(Arc::new(move |_id| f2.swap(false, Ordering::SeqCst))),
                ..Default::default()
            }),
            ..Default::default()
        },
    );

    // Create monitor M1 (will block on URL #1 inside BarrierEngine).
    let (st1, m1) = call_as(
        &api,
        None,
        "POST",
        "/v2/monitor",
        Some(json!({
            "targets": [{ "type": "scrape", "urls": ["https://site.test/m1"] }]
        })),
    )
    .await;
    assert_eq!(st1, StatusCode::OK, "{m1}");
    let id1 = m1["data"]["id"].as_str().unwrap().to_owned();
    tokio::time::timeout(std::time::Duration::from_secs(5), first_entered.notified())
        .await
        .expect("M1 enters slow scrape");

    // Create monitor M2 while M1 is blocked in slow scrape: M2 must run independently without blocking on M1!
    let (st2, m2) = call_as(
        &api,
        None,
        "POST",
        "/v2/monitor",
        Some(json!({
            "targets": [{ "type": "scrape", "urls": ["https://site.test/m2"] }]
        })),
    )
    .await;
    assert_eq!(st2, StatusCode::OK, "{m2}");
    let id2 = m2["data"]["id"].as_str().unwrap().to_owned();
    let (run2_st, run2_body) =
        call_as(&api, None, "POST", &format!("/v2/monitor/{id2}/run"), None).await;
    assert_eq!(
        run2_st,
        StatusCode::OK,
        "M2 must complete while M1 is blocked: {run2_body}"
    );

    // First DELETE on M1 encounters injected storage failure: must NOT return 200, yet must immediately
    // cancel & hide M1 (not blocked on M1's slow scrape!) and stay retryable.
    let (del_err_st, _) = call_as(&api, None, "DELETE", &format!("/v2/monitor/{id1}"), None).await;
    assert_eq!(
        del_err_st,
        StatusCode::INTERNAL_SERVER_ERROR,
        "storage delete failure must return 500, never fake 200"
    );
    let (get_st, _) = call_as(&api, None, "GET", &format!("/v2/monitor/{id1}"), None).await;
    assert_eq!(
        get_st,
        StatusCode::NOT_FOUND,
        "deleting monitor must be hidden from GET"
    );

    // Retry DELETE once storage recovers: succeeds with 200 and cleans Store.
    let (del_ok_st, _) = call_as(&api, None, "DELETE", &format!("/v2/monitor/{id1}"), None).await;
    assert_eq!(
        del_ok_st,
        StatusCode::OK,
        "retry DELETE must succeed once storage recovers"
    );
    release_first.notify_waiters();
    assert!(
        store
            .tasks()
            .kv_get(format!("monitor/{id1}"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn p2_04_shared_account_key_rotation_resume_crash_isolation_and_revoked_job_restore() {
    let dir = tempfile::tempdir().unwrap();

    // Boot 1: shared account "team-shared" has k1 + k2 (pages_per_day = 1),
    // plus an active monitor and an in-progress persisted BatchScrape job for "solo-revoked".
    let rt1 = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (shared_mon_id, crash_mon_id, revoked_job_id, shared_job_id) = rt1.block_on(async {
        let store = Store::open_dir(dir.path()).unwrap();
        let script = Arc::new(super::tests::Script::default());
        script.page(super::tests::A, 200, "# Shared\n\nBody.");
        let shared_lim = KeyLimits::new()
            .with_account("team-shared")
            .with_name("key-old-label")
            .with_pages_per_day(1);
        let mut limits = HashMap::new();
        limits.insert("k-shared-1".to_owned(), shared_lim.clone());
        limits.insert("k-shared-2".to_owned(), shared_lim);
        limits.insert(
            "k-solo".to_owned(),
            KeyLimits::new()
                .with_account("solo-revoked")
                .with_pages_per_day(1),
        );
        let api = super::tests::scripted(
            &script,
            Config {
                key_limits: limits,
                store: Some(store.clone()),
                monitor_leader: Some(Arc::new(|| false)),
                ..Default::default()
            },
        );
        let (st, m_shared) = call_as(
            &api,
            Some("k-shared-1"),
            "POST",
            "/v2/monitor",
            Some(json!({ "targets": [{ "type": "scrape", "urls": [super::tests::A] }] })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{m_shared}");
        let shared_mon_id = m_shared["data"]["id"].as_str().unwrap().to_owned();

        // Also create a second monitor under team-shared and mark it crash-paused on disk.
        let (st2, m_crash) = call_as(
            &api,
            Some("k-shared-1"),
            "POST",
            "/v2/monitor",
            Some(json!({ "targets": [{ "type": "scrape", "urls": [super::tests::A] }] })),
        )
        .await;
        assert_eq!(st2, StatusCode::OK, "{m_crash}");
        let crash_mon_id = m_crash["data"]["id"].as_str().unwrap().to_owned();
        let crash_key = format!("monitor/{crash_mon_id}");
        let mut cm: super::MonitorState =
            serde_json::from_slice(&store.tasks().kv_get(&crash_key).unwrap().unwrap()).unwrap();
        cm.status = "paused".into();
        cm.paused_reason = Some(super::PAUSED_REASON_CHECK_PANICKED.into());
        store
            .tasks()
            .kv_put(&crash_key, &serde_json::to_vec(&cm).unwrap())
            .unwrap();

        // Persist two in-progress BatchScrape jobs directly in Store (simulating crash mid-job):
        // one owned by `solo-revoked` (will be revoked), one owned by `team-shared` (retains k-shared-2).
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let revoked_job_id = "job-revoked-in-progress".to_owned();
        let shared_job_id = "job-shared-in-progress".to_owned();
        for (jid, owner, url) in [
            (
                &revoked_job_id,
                "solo-revoked",
                "https://site.test/revoked-job",
            ),
            (
                &shared_job_id,
                "team-shared",
                "https://site.test/shared-job",
            ),
        ] {
            let meta = json!({
                "kind": "batch_scrape",
                "state": "scraping",
                "created_ms": now_ms,
                "total": 1,
                "completed": 0,
                "owner": owner,
                "owner_authenticated": true,
                "tenant": owner,
                "webhook": null,
                "crawl": null,
                "batch": {
                    "urls": [url],
                    "options": {},
                    "concurrency": 1
                },
                "summary": null
            });
            store
                .tasks()
                .kv_put(format!("job/{jid}"), &serde_json::to_vec(&meta).unwrap())
                .unwrap();
        }
        (shared_mon_id, crash_mon_id, revoked_job_id, shared_job_id)
    });
    drop(rt1);

    // Boot 2: revoke `k-shared-1` (keeping `k-shared-2` with renamed display `name`) AND revoke `k-solo`.
    let rt2 = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt2.block_on(async {
        let store = Store::open_dir(dir.path()).unwrap();
        let script = Arc::new(super::tests::Script::default());
        script.page(super::tests::A, 200, "# Shared\n\nBody v2.");
        script.page("https://site.test/shared-job", 200, "# Shared Job Page");
        script.page("https://site.test/revoked-job", 200, "# MUST NOT BE SCRAPED");
        let mut limits = HashMap::new();
        limits.insert(
            "k-shared-2".to_owned(),
            KeyLimits::new()
                .with_account("team-shared")
                .with_name("rotated-display-name")
                .with_pages_per_day(2),
        );
        let api = super::tests::scripted(
            &script,
            Config {
                key_limits: limits,
                default_limits: KeyLimits::new(),
                store: Some(store.clone()),
                monitor_leader: Some(Arc::new(|| false)),
                ..Default::default()
            },
        );

        // 1. Shared monitor must remain active (NOT paused) when key-1 is rotated out while key-2 stays.
        let (st, v) = call_as(
            &api,
            Some("k-shared-2"),
            "GET",
            &format!("/v2/monitor/{shared_mon_id}"),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["data"]["status"], "active", "{v}");

        // 2. Crash-paused monitor must stay paused even though account is valid.
        let (st_c, vc) = call_as(
            &api,
            Some("k-shared-2"),
            "GET",
            &format!("/v2/monitor/{crash_mon_id}"),
            None,
        )
        .await;
        assert_eq!(st_c, StatusCode::OK, "{vc}");
        assert_eq!(vc["data"]["status"], "paused", "{vc}");
        assert_eq!(vc["data"]["pausedReason"], super::PAUSED_REASON_CHECK_PANICKED, "{vc}");

        // 3. Wait briefly for `shared_job_id` to complete; verify `revoked_job_id` became `failed` and NEVER scraped.
        for _ in 0..40 {
            let (_, jv) = call_as(
                &api,
                Some("k-shared-2"),
                "GET",
                &format!("/v2/batch/scrape/{shared_job_id}"),
                None,
            )
            .await;
            if jv["status"] == "completed" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let (_, shared_jv) = call_as(
            &api,
            Some("k-shared-2"),
            "GET",
            &format!("/v2/batch/scrape/{shared_job_id}"),
            None,
        )
        .await;
        assert_eq!(shared_jv["status"], "completed", "{shared_jv}");

        let revoked_meta: Value = serde_json::from_slice(
            &store
                .tasks()
                .kv_get(format!("job/{revoked_job_id}"))
                .unwrap()
                .expect("revoked job metadata preserved"),
        )
        .unwrap();
        assert_eq!(
            revoked_meta["state"], "failed",
            "revoked account in-progress job must be marked failed, not resumed under default_limits"
        );
        assert_eq!(
            revoked_meta["completed"], 0,
            "account_revoked note must not inflate completed page count via errors list: {revoked_meta}"
        );
        assert_eq!(
            store
                .tasks()
                .list_len(format!("job/{revoked_job_id}/errors")),
            0
        );
        assert_eq!(
            store
                .tasks()
                .list_len(format!("job/{revoked_job_id}/notes")),
            1
        );
        // Script should have scraped only the shared job URL (1), never the revoked job URL.
        assert_eq!(script.scrapes.load(Ordering::SeqCst), 1);
    });
    drop(rt2);

    // Boot 3: revoke ALL keys for `team-shared` -> monitor pauses with `account_revoked`.
    // Then Boot 4: restore `k-shared-3` under `team-shared` -> monitor auto-resumes to `active`!
    let rt3 = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt3.block_on(async {
        let store = Store::open_dir(dir.path()).unwrap();
        let script = Arc::new(super::tests::Script::default());
        // Unauthenticated restart must ALSO not adopt formerly authenticated tenant monitors!
        let api = super::tests::scripted(
            &script,
            Config {
                store: Some(store.clone()),
                ..Default::default()
            },
        );
        let (list_st, list_v) = call_as(&api, None, "GET", "/v2/monitor", None).await;
        assert_eq!(list_st, StatusCode::OK);
        assert_eq!(
            list_v["data"].as_array().unwrap().len(),
            0,
            "anonymous mode must not expose formerly authenticated tenant monitors"
        );
        let paused: super::MonitorState = serde_json::from_slice(
            &store
                .tasks()
                .kv_get(format!("monitor/{shared_mon_id}"))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(paused.status, "paused");
        assert_eq!(
            paused.paused_reason.as_deref(),
            Some(super::PAUSED_REASON_ACCOUNT_REVOKED)
        );
    });
    drop(rt3);

    // Boot 4: restore `team-shared` account -> `shared_mon_id` auto-resumes; `crash_mon_id` stays paused!
    let rt4 = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt4.block_on(async {
        let store = Store::open_dir(dir.path()).unwrap();
        let script = Arc::new(super::tests::Script::default());
        let mut limits = HashMap::new();
        limits.insert(
            "k-shared-3".to_owned(),
            KeyLimits::new()
                .with_account("team-shared")
                .with_pages_per_day(2),
        );
        let api = super::tests::scripted(
            &script,
            Config {
                key_limits: limits,
                store: Some(store.clone()),
                monitor_leader: Some(Arc::new(|| false)),
                ..Default::default()
            },
        );
        let (st, v) = call_as(
            &api,
            Some("k-shared-3"),
            "GET",
            &format!("/v2/monitor/{shared_mon_id}"),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(
            v["data"]["status"], "active",
            "restored account should auto-resume revoked monitor: {v}"
        );
        assert!(v["data"].get("pausedReason").is_none(), "{v}");

        let (_, vc) = call_as(
            &api,
            Some("k-shared-3"),
            "GET",
            &format!("/v2/monitor/{crash_mon_id}"),
            None,
        )
        .await;
        assert_eq!(
            vc["data"]["status"], "paused",
            "crash-paused monitor must NEVER auto-resume: {vc}"
        );
    });
}
