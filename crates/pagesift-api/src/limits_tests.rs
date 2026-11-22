use super::*;

fn caller(limits: KeyLimits) -> Caller {
    Caller::new(Some("secret-key"), Arc::new(limits))
}

#[test]
fn key_limits_builder_helpers_match_struct_fields() {
    let built = KeyLimits::new()
        .with_name("svc-a")
        .with_account("acct-prod")
        .with_requests_per_minute(60)
        .with_max_concurrent_jobs(4)
        .with_pages_per_day(1000);
    assert_eq!(
        built,
        KeyLimits {
            name: Some("svc-a".into()),
            account: Some("acct-prod".into()),
            requests_per_minute: Some(60),
            max_concurrent_jobs: Some(4),
            pages_per_day: Some(1000),
        }
    );
}

#[test]
fn token_bucket_allows_a_burst_then_429s() {
    let l = Limiter::default();
    let c = caller(KeyLimits::new().with_requests_per_minute(3));
    for _ in 0..3 {
        l.check_rate(&c).unwrap();
    }
    let e = l.check_rate(&c).unwrap_err();
    assert_eq!(e.code, ErrorCode::RateLimitExceeded);
    assert!(e.retry_after.unwrap() >= 1);
    // 别的调用方有自己的桶
    let other = Caller::new(Some("other"), c.limits.clone());
    l.check_rate(&other).unwrap();
    // 不限速的调用方不受影响
    l.check_rate(&caller(KeyLimits::default())).unwrap();
}

#[test]
fn accounts_isolate_same_display_name_and_share_only_explicit_account() {
    // Two distinct keys with the same display `name` MUST NOT share tenant/account identity.
    let key_a = Caller::new(
        Some("secret-a"),
        Arc::new(KeyLimits::new().with_name("team-a")),
    );
    let key_b = Caller::new(
        Some("secret-b"),
        Arc::new(KeyLimits::new().with_name("team-a")),
    );
    assert_eq!(&*key_a.display_name, "team-a");
    assert_eq!(&*key_b.display_name, "team-a");
    assert!(key_a.account.starts_with("key-") && !key_a.account.contains("secret"));
    assert!(key_b.account.starts_with("key-") && !key_b.account.contains("secret"));
    assert_ne!(key_a.account, key_b.account);
    assert_ne!(key_a.tenant_owner(), key_b.tenant_owner());

    // Multi-key rotation/sharing works ONLY when `account` is explicitly configured.
    let shared_a = Caller::new(
        Some("secret-rot-1"),
        Arc::new(
            KeyLimits::new()
                .with_name("primary-key")
                .with_account("acme-prod"),
        ),
    );
    let shared_b = Caller::new(
        Some("secret-rot-2"),
        Arc::new(
            KeyLimits::new()
                .with_name("secondary-key")
                .with_account("acme-prod"),
        ),
    );
    assert_eq!(&*shared_a.account, "acme-prod");
    assert_eq!(shared_a.account, shared_b.account);
    assert_eq!(shared_a.tenant_owner(), shared_b.tenant_owner());

    // Reserved names in `account` are ignored and fall back to key fingerprint.
    let reserved = Caller::new(
        Some("secret-r"),
        Arc::new(KeyLimits::new().with_account("anonymous")),
    );
    assert!(reserved.account.starts_with("key-"));
    assert!(reserved.authenticated);

    assert_eq!(&*Caller::new(None, Arc::default()).account, "anonymous");
}

/// 网关：限额记在账本里，行为与单主机一致（桶先突发后 429，配额用完 QUOTA_EXCEEDED）。
#[tokio::test]
async fn ledger_backed_limits_match_the_local_ones() {
    let ledger = Arc::new(
        pagesift_cluster::Ledger::open(pagesift_cluster::LedgerConfig::new(
            pagesift_cluster::MonoClock::new(),
            false,
        ))
        .unwrap(),
    );
    let limits = Arc::new(
        KeyLimits::new()
            .with_requests_per_minute(2)
            .with_pages_per_day(2),
    );
    let c = Caller::new(Some("k"), limits.clone());
    let accounts = HashMap::from([(c.account.to_string(), limits)]);
    let l = Arc::new(Limiter::with_ledger(ledger, &accounts));
    l.check_rate(&c).unwrap();
    l.check_rate(&c).unwrap();
    let e = l.check_rate(&c).unwrap_err();
    assert_eq!(e.code, ErrorCode::RateLimitExceeded);
    assert!(e.retry_after.unwrap() >= 1);
    assert_eq!(l.remaining_pages(&c), Some(2));
    l.reserve_page(&c).await.unwrap().commit().await;
    assert!(l.check_quota(&c).is_ok());
    l.reserve_page(&c).await.unwrap().commit().await;
    assert_eq!(l.remaining_pages(&c), Some(0));
    assert_eq!(
        l.check_quota(&c).unwrap_err().code,
        ErrorCode::QuotaExceeded
    );
    // 不限额的调用方不受影响
    l.check_rate(&caller(KeyLimits::default())).unwrap();
}

#[tokio::test]
async fn concurrent_page_reservations_stop_at_quota() {
    let c = caller(KeyLimits::new().with_pages_per_day(1));
    let limiter = Arc::new(Limiter::new(None));
    let (a, b) = tokio::join!(limiter.reserve_page(&c), limiter.reserve_page(&c));
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(limiter.remaining_pages(&c), Some(0));
    // 丢掉未结算的预留：额度回来
    drop(a);
    drop(b);
    assert_eq!(limiter.remaining_pages(&c), Some(1));
}

#[tokio::test]
async fn twenty_concurrent_reservations_allow_exactly_one() {
    let c = Arc::new(caller(KeyLimits::new().with_pages_per_day(1)));
    let limiter = Arc::new(Limiter::new(None));
    let mut tasks = Vec::new();
    for _ in 0..20 {
        let (limiter, c) = (limiter.clone(), c.clone());
        tasks.push(tokio::spawn(async move { limiter.reserve_page(&c).await }));
    }
    let mut granted = 0;
    let mut holds = Vec::new();
    for task in tasks {
        match task.await.unwrap() {
            Ok(hold) => {
                granted += 1;
                holds.push(hold);
            }
            Err(e) => assert_eq!(e.code, ErrorCode::QuotaExceeded),
        }
    }
    assert_eq!(granted, 1);
    assert_eq!(limiter.remaining_pages(&c), Some(0));
    drop(holds);
    assert_eq!(limiter.remaining_pages(&c), Some(1));
}

#[tokio::test]
async fn committed_pages_survive_restart_and_holds_refund() {
    let dir = tempfile::tempdir().unwrap();
    let c = caller(KeyLimits::new().with_pages_per_day(2));
    let limiter = Arc::new(Limiter::new(Some(Store::open_dir(dir.path()).unwrap())));
    let hold = limiter.reserve_page(&c).await.unwrap();
    assert_eq!(limiter.remaining_pages(&c), Some(1));
    drop(hold);
    assert_eq!(limiter.remaining_pages(&c), Some(2));
    let hold = limiter.reserve_page(&c).await.unwrap();
    hold.commit().await;
    assert_eq!(limiter.remaining_pages(&c), Some(1));
    drop(limiter);
    // 重启：已结算的 1 页还在，未结算的预留不落盘
    let limiter = Limiter::new(Some(Store::open_dir(dir.path()).unwrap()));
    assert_eq!(limiter.remaining_pages(&c), Some(1));
}

#[tokio::test]
async fn failed_reservation_is_not_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let c = caller(KeyLimits::new().with_pages_per_day(2));
    let limiter = Arc::new(Limiter::new(Some(Store::open_dir(dir.path()).unwrap())));
    let a = limiter.reserve_page(&c).await.unwrap();
    let b = limiter.reserve_page(&c).await.unwrap();
    a.commit().await;
    // B 失败：只退内存里的在途预留，磁盘上不能留下这一页
    drop(b);
    assert_eq!(limiter.remaining_pages(&c), Some(1));
    drop(limiter);
    let limiter = Limiter::new(Some(Store::open_dir(dir.path()).unwrap()));
    assert_eq!(limiter.remaining_pages(&c), Some(1));
}

#[tokio::test]
async fn concurrent_commits_persist_every_settled_page_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let c = Arc::new(caller(KeyLimits::new().with_pages_per_day(20)));
    let limiter = Arc::new(Limiter::new(Some(Store::open_dir(dir.path()).unwrap())));
    let mut tasks = Vec::new();
    for _ in 0..20 {
        let (limiter, c) = (limiter.clone(), c.clone());
        tasks.push(tokio::spawn(async move {
            let hold = limiter.reserve_page(&c).await.unwrap();
            hold.commit().await;
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(limiter.remaining_pages(&c), Some(0));
    drop(limiter);
    // 任何一次乱序写盘都会让重启后的已用页数少于 20
    let limiter = Limiter::new(Some(Store::open_dir(dir.path()).unwrap()));
    assert_eq!(limiter.remaining_pages(&c), Some(0));
}

#[tokio::test]
async fn ledger_reservations_are_atomic_across_limiter_instances() {
    let ledger = Arc::new(
        pagesift_cluster::Ledger::open(pagesift_cluster::LedgerConfig::new(
            pagesift_cluster::MonoClock::new(),
            false,
        ))
        .unwrap(),
    );
    let limits = Arc::new(KeyLimits::new().with_pages_per_day(1));
    let c = Arc::new(Caller::new(Some("k"), limits.clone()));
    let accounts = HashMap::from([(c.account.to_string(), limits)]);
    let a = Arc::new(Limiter::with_ledger(ledger.clone(), &accounts));
    let b = Arc::new(Limiter::with_ledger(ledger.clone(), &accounts));
    let mut tasks = Vec::new();
    for i in 0..20 {
        let (l, c) = (if i % 2 == 0 { a.clone() } else { b.clone() }, c.clone());
        tasks.push(tokio::spawn(async move { l.reserve_page(&c).await }));
    }
    let mut holds = Vec::new();
    let mut granted = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(hold) => {
                granted += 1;
                holds.push(hold);
            }
            Err(e) => assert_eq!(e.code, ErrorCode::QuotaExceeded),
        }
    }
    assert_eq!(granted, 1);
    assert_eq!(a.remaining_pages(&c), Some(0));
    drop(holds);
    assert_eq!(a.remaining_pages(&c), Some(1));
}

#[tokio::test]
async fn page_quota_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let c = caller(KeyLimits::new().with_pages_per_day(5));
    {
        let store = Store::open_dir(dir.path()).unwrap();
        let l = Arc::new(Limiter::new(Some(store)));
        assert_eq!(l.remaining_pages(&c), Some(5));
        for _ in 0..4 {
            l.reserve_page(&c).await.unwrap().commit().await;
        }
        assert!(l.check_quota(&c).is_ok());
        l.reserve_page(&c).await.unwrap().commit().await;
        assert_eq!(
            l.check_quota(&c).unwrap_err().code,
            ErrorCode::QuotaExceeded
        );
    }
    let l = Limiter::new(Some(Store::open_dir(dir.path()).unwrap()));
    assert_eq!(l.remaining_pages(&c), Some(0));
    let jobs = caller(KeyLimits::new().with_max_concurrent_jobs(1));
    assert!(l.check_jobs(&jobs, 0).is_ok());
    assert_eq!(
        l.check_jobs(&jobs, 1).unwrap_err().code,
        ErrorCode::ConcurrencyLimitExceeded
    );
}

#[tokio::test]
async fn commit_cancelled_during_write_lock_wait_does_not_leak_held_quota() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let l = Arc::new(Limiter::new(Some(store.clone())));
    let c = caller(KeyLimits::new().with_pages_per_day(2));
    let acct_lock = {
        let mut locks = l.write_locks.lock().unwrap();
        locks
            .entry(c.account.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let guard = acct_lock.lock_owned().await;
    let hold = l.reserve_page(&c).await.unwrap();
    // Cancel `commit()` mid-await while the per-account write lock is held.
    let mut fut = Box::pin(hold.commit());
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    assert!(fut.as_mut().poll(&mut cx).is_pending());
    drop(fut);
    // `held` is already 0 and transitioned to `settled == 1` (no stuck hold).
    {
        let map = l.usage.lock().unwrap();
        let u = map.get(&c.account).unwrap();
        assert_eq!(u.held, 0);
        assert_eq!(u.settled, 1);
    }
    assert_eq!(l.remaining_pages(&c), Some(1));
    // Release write lock and wait until the detached persistence task writes to Store.
    drop(guard);
    let key = usage_key(&c.account);
    for _ in 0..100 {
        if store.tasks().kv_get(&key).unwrap().is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        store.tasks().kv_get(&key).unwrap().is_some(),
        "detached persistence task must flush settled quota to disk"
    );
    drop(l);
    drop(store);
    let reopened = Limiter::new(Some(Store::open_dir(dir.path()).unwrap()));
    assert_eq!(reopened.remaining_pages(&c), Some(1));
}

#[tokio::test]
async fn p1_01_direct_rust_config_rejects_conflicting_shared_account_limits_and_zero_rpm() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    // 1. Conflicting shared-account caps across every limit field (`pages_per_day`,
    //    `requests_per_minute`, `max_concurrent_jobs`), including None vs Some and Some(0) vs Some(1).
    let cases = [
        (
            "pages_per_day None vs Some(1)",
            KeyLimits::new().with_account("shared-team"),
            KeyLimits::new()
                .with_account("shared-team")
                .with_pages_per_day(1),
        ),
        (
            "pages_per_day Some(0) vs Some(1)",
            KeyLimits::new()
                .with_account("shared-team")
                .with_pages_per_day(0),
            KeyLimits::new()
                .with_account("shared-team")
                .with_pages_per_day(1),
        ),
        (
            "requests_per_minute None vs Some(10)",
            KeyLimits::new().with_account("shared-team"),
            KeyLimits::new()
                .with_account("shared-team")
                .with_requests_per_minute(10),
        ),
        (
            "max_concurrent_jobs Some(1) vs Some(2)",
            KeyLimits::new()
                .with_account("shared-team")
                .with_max_concurrent_jobs(1),
            KeyLimits::new()
                .with_account("shared-team")
                .with_max_concurrent_jobs(2),
        ),
    ];
    for (label, lim_a, lim_b) in cases {
        let mut key_limits = HashMap::new();
        key_limits.insert("k-a".to_owned(), lim_a);
        key_limits.insert("k-b".to_owned(), lim_b);
        let err = crate::try_router(
            Arc::new(crate::test_engine::FakeEngine::open()),
            crate::Config {
                key_limits: key_limits.clone(),
                ..Default::default()
            },
        )
        .expect_err(&format!("try_router must reject {label}"));
        assert_eq!(err.code, ErrorCode::InvalidRequest, "{label}: {err}");

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::router(
                Arc::new(crate::test_engine::FakeEngine::open()),
                crate::Config {
                    key_limits,
                    ..Default::default()
                },
            )
        }));
        assert!(
            panicked.is_err(),
            "router() must also fail closed for {label}"
        );
    }

    // 2. Invalid explicit account IDs in key_limits or default_limits must be rejected fail-closed.
    for bad in [
        "",
        "   ",
        "anonymous",
        "key-123",
        "__http_anon__x",
        "bad/acct",
        "bad\nline",
    ] {
        let mut key_limits = HashMap::new();
        key_limits.insert("k-bad".to_owned(), KeyLimits::new().with_account(bad));
        assert!(
            crate::try_router(
                Arc::new(crate::test_engine::FakeEngine::open()),
                crate::Config {
                    key_limits,
                    ..Default::default()
                },
            )
            .is_err(),
            "key_limits.account = {bad:?} must be rejected"
        );
        assert!(
            crate::try_router(
                Arc::new(crate::test_engine::FakeEngine::open()),
                crate::Config {
                    default_limits: KeyLimits::new().with_account(bad),
                    ..Default::default()
                },
            )
            .is_err(),
            "default_limits.account = {bad:?} must be rejected"
        );
    }

    // 3. Valid shared account across two keys shares RPM and daily page quota via HTTP router.
    let shared_lim = KeyLimits::new()
        .with_account("team-alpha")
        .with_requests_per_minute(2)
        .with_pages_per_day(1);
    let mut valid_keys = HashMap::new();
    valid_keys.insert("k-1".to_owned(), shared_lim.clone());
    valid_keys.insert("k-2".to_owned(), shared_lim);
    let app = crate::try_router(
        Arc::new(crate::test_engine::FakeEngine::open()),
        crate::Config {
            key_limits: valid_keys,
            ..Default::default()
        },
    )
    .expect("consistent shared account limits must succeed");
    let scrape_req = |k: &str| {
        Request::builder()
            .method("POST")
            .uri("/v2/scrape")
            .header("authorization", format!("Bearer {k}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"url":"https://example.com"}"#))
            .unwrap()
    };
    let r1 = app.clone().oneshot(scrape_req("k-1")).await.unwrap();
    assert_eq!(r1.status(), StatusCode::OK);
    // Second key under the same shared account sees the shared 1-page daily quota exhausted.
    let r2 = app.clone().oneshot(scrape_req("k-2")).await.unwrap();
    assert_eq!(r2.status(), StatusCode::TOO_MANY_REQUESTS);
    let r2_body: serde_json::Value = serde_json::from_slice(
        &http_body_util::BodyExt::collect(r2.into_body())
            .await
            .unwrap()
            .to_bytes(),
    )
    .unwrap();
    assert_eq!(r2_body["code"], "QUOTA_EXCEEDED", "{r2_body}");
    // Third request across the shared account exceeds RPM = 2.
    let r3 = app.oneshot(scrape_req("k-1")).await.unwrap();
    assert_eq!(r3.status(), StatusCode::TOO_MANY_REQUESTS);
    let r3_body: serde_json::Value = serde_json::from_slice(
        &http_body_util::BodyExt::collect(r3.into_body())
            .await
            .unwrap()
            .to_bytes(),
    )
    .unwrap();
    assert_eq!(r3_body["code"], "RATE_LIMIT_EXCEEDED", "{r3_body}");

    // 4. Zero limits (`requests_per_minute: Some(0)`, `pages_per_day: Some(0)`,
    //    `max_concurrent_jobs: Some(0)`) behave identically on local Limiter and Cluster Ledger.
    let c0 = caller(
        KeyLimits::new()
            .with_requests_per_minute(0)
            .with_pages_per_day(0)
            .with_max_concurrent_jobs(0),
    );
    let local = Arc::new(Limiter::default());
    assert_eq!(
        local.check_rate(&c0).unwrap_err().code,
        ErrorCode::RateLimitExceeded
    );
    assert_eq!(
        local
            .reserve_page(&c0)
            .await
            .err()
            .expect("0 pages_per_day must fail")
            .code,
        ErrorCode::QuotaExceeded
    );
    assert_eq!(
        local.check_jobs(&c0, 0).unwrap_err().code,
        ErrorCode::ConcurrencyLimitExceeded
    );

    let clock = pagesift_cluster::ManualClock::new();
    let ledger = Arc::new(
        pagesift_cluster::Ledger::open(pagesift_cluster::LedgerConfig::new(clock, false)).unwrap(),
    );
    let accounts = HashMap::from([(c0.account.to_string(), c0.limits.clone())]);
    let cluster = Arc::new(Limiter::with_ledger(ledger, &accounts));
    assert_eq!(
        cluster.check_rate(&c0).unwrap_err().code,
        ErrorCode::RateLimitExceeded
    );
    assert_eq!(
        cluster
            .reserve_page(&c0)
            .await
            .err()
            .expect("0 pages_per_day must fail")
            .code,
        ErrorCode::QuotaExceeded
    );
    assert_eq!(
        cluster.check_jobs(&c0, 0).unwrap_err().code,
        ErrorCode::ConcurrencyLimitExceeded
    );
}

#[tokio::test]
async fn p2_05_ledger_release_and_settle_failure_during_lease_gap_reconcile_without_stuck_hold() {
    use pagesift_cluster::Clock;
    let clock = pagesift_cluster::ManualClock::new();
    let ledger = Arc::new(
        pagesift_cluster::Ledger::open(pagesift_cluster::LedgerConfig::new(clock.clone(), false))
            .unwrap(),
    );
    ledger.require_lease();
    ledger.extend_lease(clock.now_ms() + 10_000);

    let limits = Arc::new(KeyLimits::new().with_pages_per_day(2));
    let c = Caller::new(Some("sk-ledger-fail"), limits.clone());
    let accounts = HashMap::from([(c.account.to_string(), limits)]);
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let limiter =
        Arc::new(Limiter::with_ledger(ledger.clone(), &accounts).with_metrics(metrics.clone()));

    // 1. Reserve page #1, expire lease before drop -> release fails transiently and queues pending.
    let hold1 = limiter.reserve_page(&c).await.unwrap();
    assert_eq!(limiter.remaining_pages(&c), Some(1));
    clock.advance(std::time::Duration::from_secs(20));
    drop(hold1);
    assert_eq!(limiter.pending_settlement_count(), 1);

    // 2. Lease recovers -> automatic reconcile releases hold1 idempotently (balance back to 2).
    ledger.extend_lease(clock.now_ms() + 10_000);
    assert_eq!(
        limiter.remaining_pages(&c),
        Some(2),
        "failed ledger release during transient lease gap must reconcile when lease recovers"
    );
    assert_eq!(limiter.pending_settlement_count(), 0);

    // 3. Reserve page #2, expire lease before commit -> commit returns recoverable Failed and queues Settle.
    let hold2 = limiter.reserve_page(&c).await.unwrap();
    clock.advance(std::time::Duration::from_secs(20));
    let outcome = hold2.commit().await;
    assert!(
        matches!(
            outcome,
            SettleOutcome::Failed {
                recoverable: true,
                ..
            }
        ),
        "lease expiry on commit must surface recoverable Failed, got {outcome:?}"
    );
    assert_eq!(limiter.pending_settlement_count(), 1);
    assert!(limiter.settlement_counters().0 >= 2);

    // 4. Lease recovers -> reconcile settles hold2 exactly once (never double-charges).
    ledger.extend_lease(clock.now_ms() + 10_000);
    assert_eq!(limiter.remaining_pages(&c), Some(1));
    assert_eq!(limiter.pending_settlement_count(), 0);
    // Repeated reconcile calls must remain strictly idempotent.
    limiter.reconcile_ledger();
    assert_eq!(limiter.remaining_pages(&c), Some(1));
    assert_eq!(ledger.open_hold_count(&c.account), 0);
}

#[tokio::test]
async fn p2_05_http_exposes_recoverable_warning_metrics_and_unrecoverable_demotion_500() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use pagesift_cluster::Clock;
    use tower::ServiceExt;

    let clock = pagesift_cluster::ManualClock::new();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let ledger = Arc::new(
        pagesift_cluster::Ledger::open(
            pagesift_cluster::LedgerConfig::new(clock.clone(), true)
                .with_journal(pagesift_cluster::Journal::new(store)),
        )
        .unwrap(),
    );
    ledger.require_lease();
    ledger.extend_lease(clock.now_ms() + 10_000);

    // Custom engine that advances the manual clock past lease expiry during scrape().
    struct LeaseExpiringEngine {
        clock: Arc<pagesift_cluster::ManualClock>,
    }
    impl crate::Engine for LeaseExpiringEngine {
        fn scrape<'a>(
            &'a self,
            url: &'a str,
            _opts: &'a crate::types::ScrapeOptions,
        ) -> crate::BoxFuture<'a, Result<crate::types::Document, ApiError>> {
            let clock = self.clock.clone();
            let url = url.to_owned();
            Box::pin(async move {
                clock.advance(std::time::Duration::from_secs(20));
                Ok(crate::types::Document {
                    markdown: Some("# ok".into()),
                    metadata: crate::types::Metadata {
                        source_url: Some(url),
                        status_code: 200,
                        ..Default::default()
                    },
                    ..Default::default()
                })
            })
        }
    }

    // Scenario A: transient lease expiry during scrape -> HTTP response surfaces warning + /metrics pending=1.
    let mut keys = HashMap::new();
    keys.insert("sk-p205".to_owned(), KeyLimits::new().with_pages_per_day(2));
    let app = crate::try_router(
        Arc::new(LeaseExpiringEngine {
            clock: clock.clone(),
        }),
        crate::Config {
            key_limits: keys.clone(),
            ledger: Some(ledger.clone()),
            ..Default::default()
        },
    )
    .unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v2/scrape")
                .header("authorization", "Bearer sk-p205")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"url":"https://example.com/a"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(
        body["data"]["metadata"]["pagesift"]["warnings"]
            .as_array()
            .is_some_and(|w| w.iter().any(|x| x
                .as_str()
                .unwrap_or("")
                .contains("quota_settlement_unconfirmed"))),
        "transient settlement failure must surface explicit warning: {body}"
    );
    let metrics_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let metrics_txt = String::from_utf8(
        metrics_resp
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(
        metrics_txt.contains("pagesift_quota_pending_settlements 1"),
        "expected pending=1 in /metrics:\n{metrics_txt}"
    );
    assert!(
        metrics_txt.contains("pagesift_quota_settlement_failures_total 1"),
        "expected failures=1 in /metrics:\n{metrics_txt}"
    );

    // Recover lease: next HTTP scrape reconciles the pending settle (1 page) AND settles its own (2nd page),
    // leaving 0 pages left, and replaying the persisted Journal on a standby Ledger reproduces exact balance 0.
    ledger.extend_lease(clock.now_ms() + 10_000);
    let resp2 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v2/scrape")
                .header("authorization", "Bearer sk-p205")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"url":"https://example.com/b"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    // Recover lease again and run a 3rd scrape against `app` (triggers reconcile of #2, then 402 QuotaExceeded).
    ledger.extend_lease(clock.now_ms() + 10_000);
    let resp3 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v2/scrape")
                .header("authorization", "Bearer sk-p205")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"url":"https://example.com/c"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp3.status(), StatusCode::TOO_MANY_REQUESTS);
    let resp3_body: serde_json::Value =
        serde_json::from_slice(&resp3.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(resp3_body["code"], "QUOTA_EXCEEDED", "{resp3_body}");
    drop(app);

    // Scenario B: follower Ledger produces unrecoverable `SettleOutcome::Failed { recoverable: false }`
    // and increments `dropped` counter instead of queueing a hopeless retry.
    let follower_clock = pagesift_cluster::ManualClock::new();
    let follower_ledger = Arc::new(
        pagesift_cluster::Ledger::follower(pagesift_cluster::LedgerConfig::new(
            follower_clock.clone(),
            false,
        ))
        .unwrap(),
    );
    let lim = Arc::new(KeyLimits::new().with_pages_per_day(5));
    let c = Caller::new(Some("sk-follower"), lim.clone());
    let follower_limiter = Arc::new(Limiter::with_ledger(
        follower_ledger,
        &HashMap::from([(c.account.to_string(), lim)]),
    ));
    let fake_hold = PageHold {
        limiter: follower_limiter.clone(),
        account: c.account.clone(),
        day: 1,
        amount: 1,
        ledger: Some(pagesift_cluster::Hold {
            id: 42,
            account: c.account.clone(),
            day: 1,
            amount: 1,
        }),
        tracked: true,
        settled: false,
    };
    let outcome = fake_hold.commit().await;
    assert!(
        matches!(
            outcome,
            SettleOutcome::Failed {
                recoverable: false,
                ..
            }
        ),
        "follower Ledger settlement must be unrecoverable Failed, got {outcome:?}"
    );
    assert!(outcome.is_unrecoverable());
    assert_eq!(
        follower_limiter.settlement_counters().2,
        1,
        "dropped counter incremented"
    );

    // Scenario C: Journal replay after restart preserves exact settled balance and rolls over cleanly across UTC day.
    drop(ledger);
    let reopened_store = Store::open_dir(dir.path()).unwrap();
    let replay_clock = pagesift_cluster::ManualClock::new();
    let replayed = pagesift_cluster::Ledger::open(
        pagesift_cluster::LedgerConfig::new(replay_clock.clone(), true)
            .with_journal(pagesift_cluster::Journal::new(reopened_store)),
    )
    .unwrap();
    let acct = Caller::new(Some("sk-p205"), Arc::new(KeyLimits::new())).account;
    replayed.set_limits(
        &acct,
        pagesift_cluster::AccountLimits {
            pages_per_day: Some(2),
            requests_per_minute: None,
        },
    );
    assert_eq!(
        replayed.balance(&acct),
        Some(0),
        "replayed ledger must preserve 2 settled pages"
    );
    // Advance across UTC day boundary -> full quota restored and zero stale holds.
    replay_clock.advance(std::time::Duration::from_secs(86_400 + 60));
    assert_eq!(
        replayed.balance(&acct),
        Some(2),
        "UTC day rollover restores full daily quota"
    );
    assert_eq!(replayed.open_hold_count(&acct), 0);

    // Scenario D: local `dirty_local` entries from previous UTC days are pruned automatically
    // instead of sticking `pagesift_quota_pending_settlements >= 1` forever.
    let local_lim = Arc::new(Limiter::default());
    local_lim.mark_dirty_local(Arc::<str>::from("stale-acct"), 0);
    assert_eq!(
        local_lim.pending_settlement_count(),
        0,
        "expired previous-day dirty_local entries must be pruned automatically"
    );
}
