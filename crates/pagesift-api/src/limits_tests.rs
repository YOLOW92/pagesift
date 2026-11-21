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
