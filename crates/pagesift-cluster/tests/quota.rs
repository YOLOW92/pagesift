//! 账本化配额、令牌桶与账户间加权公平调度。

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use common::{free, gateway, task};
use pagesift_cluster::{
    AccountLimits, Journal, Ledger, LedgerConfig, ManualClock, Refusal, Standby,
};
use pagesift_store::Store;

fn local(clock: &Arc<ManualClock>, dir: Option<&tempfile::TempDir>) -> Ledger {
    let mut cfg = LedgerConfig::new(clock.clone(), true);
    cfg.shards = 4;
    if let Some(d) = dir {
        cfg = cfg.with_journal(Journal::new(Store::open_dir(d.path()).unwrap()));
    }
    Ledger::open(cfg).unwrap()
}

fn pages(n: u64) -> AccountLimits {
    AccountLimits {
        pages_per_day: Some(n),
        requests_per_minute: None,
    }
}

/// 余额由冻结、结算、释放记录求和；重放后一致；冻结不够时拒绝并给出到 UTC 零点的等待。
#[test]
fn balance_is_replayed_from_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new();
    let l = local(&clock, Some(&dir));
    l.set_limits("a", pages(10));
    let h1 = l.reserve("a", 3).unwrap();
    l.settle(&h1, 2).unwrap();
    let _open = l.reserve("a", 4).unwrap();
    let h3 = l.reserve("a", 1).unwrap();
    l.release(&h3).unwrap();
    assert_eq!(l.balance("a"), Some(10 - 2 - 4));
    assert!(matches!(
        l.reserve("a", 5),
        Err(Refusal::Quota { limit: 10, retry_after }) if retry_after == Duration::from_secs(86_400)
    ));
    assert_eq!(l.balance("nobody"), None, "no limit configured");
    assert_eq!(l.reserve("nobody", 9).unwrap().id, 0);
    drop(l);

    let l = local(&clock, Some(&dir));
    l.set_limits("a", pages(10));
    assert_eq!(l.balance("a"), Some(4));
    // 新的 UTC 日从满额开始
    clock.advance(Duration::from_secs(86_400));
    assert_eq!(l.balance("a"), Some(10));
}

/// 随分配冻结的一页：提交时结算，租约过期或取消时释放。
#[test]
fn task_holds_follow_the_lease() {
    let clock = ManualClock::new();
    let l = local(&clock, None);
    l.set_limits("a", pages(5));
    let t1 = l.submit_as(task(1, "x"), Some("a")).unwrap();
    let lease = l.pull(None, &free(), Duration::ZERO).unwrap().unwrap();
    assert_eq!((lease.task, l.balance("a")), (t1, Some(4)));
    clock.advance(lease.ttl + Duration::from_millis(1));
    assert_eq!(l.tick().expired.len(), 1);
    assert_eq!(l.balance("a"), Some(5), "released on expiry");

    let again = l.pull(None, &free(), Duration::ZERO).unwrap().unwrap();
    assert_eq!(l.balance("a"), Some(4));
    assert!(l.cancel(again.task).unwrap());
    assert_eq!(l.balance("a"), Some(5), "released on cancel");

    l.submit_as(task(2, "y"), Some("a")).unwrap();
    let c = l.pull(None, &free(), Duration::ZERO).unwrap().unwrap();
    l.commit(c.task, c.fence(), None, None, |_| {}).unwrap();
    assert_eq!(l.balance("a"), Some(4), "settled on commit");

    // 额度用完：该账户的任务不再分配
    l.set_limits("a", pages(1));
    l.submit_as(task(3, "z"), Some("a")).unwrap();
    assert!(l.pull(None, &free(), Duration::ZERO).unwrap().is_none());
}

/// 温备接管后，令牌桶由日志里最近一分钟放行的请求重建。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rate_bucket_is_rebuilt_after_takeover() {
    let clock = ManualClock::new();
    let (d1, d2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (primary, _rx) = gateway(
        clock.clone(),
        Some(Journal::new(Store::open_dir(d1.path()).unwrap())),
    );
    let rpm = AccountLimits {
        pages_per_day: None,
        requests_per_minute: Some(3),
    };
    primary.ledger().set_limits("k", rpm);
    for _ in 0..3 {
        primary.ledger().check_rate("k").unwrap();
    }
    assert!(matches!(
        primary.ledger().check_rate("k"),
        Err(Refusal::Rate { limit: 3, .. })
    ));

    let cfg = LedgerConfig::new(clock.clone(), false)
        .with_journal(Journal::new(Store::open_dir(d2.path()).unwrap()));
    let standby = Arc::new(Standby::new(Arc::new(Ledger::follower(cfg).unwrap())));
    primary.tokens().add(
        pagesift_cluster::WorkerId::new("standby"),
        &common::token("standby"),
    );
    let (a, b) = tokio::io::duplex(1 << 16);
    let p = primary.clone();
    let serve = tokio::spawn(async move { p.serve(b).await });
    let s = standby.clone();
    let follow = tokio::spawn(async move { s.follow(a, &common::dial("standby")).await });
    standby
        .caught_up(primary.ledger().journal().unwrap().len())
        .await;
    serve.abort();
    let _ = follow.await;
    standby.take_over().unwrap();

    let l = standby.ledger();
    l.set_limits("k", rpm);
    assert!(
        matches!(l.check_rate("k"), Err(Refusal::Rate { .. })),
        "three requests in the last minute"
    );
    clock.advance(Duration::from_secs(20));
    l.check_rate("k").unwrap();
    assert!(l.check_rate("k").is_err());
}

/// 两个账户配额 1:3、都积压时发放比例收敛到 1:3；一个空闲时另一个用满。
#[test]
fn drr_shares_follow_quota_weights() {
    let clock = ManualClock::new();
    let l = local(&clock, None);
    l.set_limits("a", pages(1_000));
    l.set_limits("b", pages(3_000));
    let mut owner = HashMap::new();
    for i in 0..400u64 {
        owner.insert(l.submit_as(task(i % 13, "a"), Some("a")).unwrap(), "a");
        owner.insert(l.submit_as(task(i % 17, "b"), Some("b")).unwrap(), "b");
    }
    let mut n: HashMap<&str, u32> = HashMap::new();
    for _ in 0..400 {
        let x = l.pull(None, &free(), Duration::ZERO).unwrap().unwrap();
        *n.entry(owner[&x.task]).or_default() += 1;
        l.commit(x.task, x.fence(), None, None, |_| {}).unwrap();
    }
    let (a, b) = (i64::from(n["a"]), i64::from(n["b"]));
    assert!((3 * a - b).abs() <= 3, "a={a} b={b}");
    // b 的 400 个先做完；之后 a 独占
    let mut rest = 0;
    while let Some(x) = l.pull(None, &free(), Duration::ZERO).unwrap() {
        if owner[&x.task] == "a" {
            rest += 1;
        }
        l.commit(x.task, x.fence(), None, None, |_| {}).unwrap();
    }
    assert_eq!(a + rest, 400);
    // 不带账户的任务照常分配
    l.submit(task(1, "plain")).unwrap();
    assert!(l.pull(None, &free(), Duration::ZERO).unwrap().is_some());
}
