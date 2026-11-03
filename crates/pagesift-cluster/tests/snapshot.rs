//! 日志检查点：快照后截断、从快照回放、温备跟着截断与从快照重新同步；多线程下的账户公平调度。

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use common::{free, gateway, task};
use pagesift_cluster::journal::{Entry, Record};
use pagesift_cluster::{
    AccountLimits, Journal, Ledger, LedgerConfig, ManualClock, Refusal, Standby, StatKind,
    StatsDelta,
};
use pagesift_stat::beta::BetaCounts;
use pagesift_stat::table::{Key, MetricId, Mode, Scope, StatTable};
use pagesift_store::Store;

fn journal(d: &tempfile::TempDir) -> Journal {
    Journal::new(Store::open_dir(d.path()).unwrap())
}

fn open(clock: &Arc<ManualClock>, d: &tempfile::TempDir) -> Ledger {
    let mut cfg = LedgerConfig::new(clock.clone(), true).with_journal(journal(d));
    cfg.shards = 4;
    let l = Ledger::open(cfg).unwrap();
    l.set_limits(
        "a",
        AccountLimits {
            pages_per_day: Some(1_000),
            requests_per_minute: Some(5),
        },
    );
    l
}

fn delta(seq: u64) -> StatsDelta {
    let mut t: StatTable<BetaCounts> = StatTable::new(Mode::Live);
    let key = Key {
        scope: Scope::Global,
        metric: MetricId(1),
    };
    t.update(key, |c| c.observe(true));
    StatsDelta {
        kind: StatKind(1),
        seq,
        table: t.encode(),
    }
}

/// 长期运行时日志被快照截断，存活记录有界；重开后从快照恢复出同样的状态。
#[test]
fn journal_is_compacted_and_replays_from_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new();
    let l = open(&clock, &dir);
    let origin = pagesift_cluster::Origin {
        worker: pagesift_cluster::WorkerId::new("w"),
        boot: 1,
    };
    for i in 0..300u64 {
        l.submit_as(task(i % 11, &format!("t{i}")), Some("a"))
            .unwrap();
    }
    let mut leased = Vec::new();
    for i in 0..120 {
        let x = l.pull(None, &free(), Duration::ZERO).unwrap().unwrap();
        if i % 2 == 0 {
            l.commit(x.task, x.fence(), None, None, |_| {}).unwrap();
        } else {
            leased.push(x);
        }
    }
    l.register_stat::<BetaCounts>(StatKind(1));
    assert!(l.merge_stats(&origin, delta(0)).unwrap());
    for _ in 0..5 {
        l.check_rate("a").unwrap();
    }
    let j = l.journal().unwrap();
    assert!(j.head() > 0, "head truncated by a snapshot");
    // 存活的日志 = 最近一次快照 + 其后的增量，增量不超过快照本身（否则已经又做了一次快照）
    let (mut snap, mut after, mut in_snap) = (0, 0, true);
    for (_, raw) in j.read(0, usize::MAX).unwrap() {
        let rec = Entry::decode(&raw).unwrap().rec;
        if in_snap {
            snap += raw.len();
            in_snap = !matches!(rec, Record::SnapEnd {});
        } else {
            after += raw.len();
        }
    }
    assert!(after <= snap, "{after} bytes after a {snap}-byte snapshot");
    let balance = l.balance("a");
    drop(l);

    let l = open(&clock, &dir);
    l.register_stat::<BetaCounts>(StatKind(1));
    assert_eq!(l.balance("a"), balance, "60 settled + 60 frozen");
    assert!(matches!(l.check_rate("a"), Err(Refusal::Rate { .. })));
    // 重复的统计增量仍被认出；回放早于登记时也已合并
    assert!(!l.merge_stats(&origin, delta(0)).unwrap());
    assert!(!l.priors(Scope::Global).tables[0].1.is_empty());
    // 快照前的租约仍可提交；剩下的任务一个不少
    for x in &leased {
        l.commit(x.task, x.fence(), None, None, |_| {}).unwrap();
    }
    let mut rest = 0;
    while let Some(x) = l.pull(None, &free(), Duration::ZERO).unwrap() {
        l.commit(x.task, x.fence(), None, None, |_| {}).unwrap();
        rest += 1;
    }
    assert_eq!(rest, 300 - 120);
}

/// 温备跟着主网关的快照截断；落后到已截断的部分时从快照整体重新同步，接管后状态一致。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn standby_follows_snapshots_and_resyncs() {
    let clock = ManualClock::new();
    let (d1, d2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (primary, _rx) = gateway(clock.clone(), Some(journal(&d1)));
    let p = primary.ledger().clone();
    for i in 0..50u64 {
        p.submit(task(i, "x")).unwrap();
    }
    // 温备晚到：主网关的头部已被截掉
    assert!(p.journal().unwrap().head() > 0);
    let cfg = LedgerConfig::new(clock.clone(), false).with_journal(journal(&d2));
    let standby = Arc::new(Standby::new(Arc::new(Ledger::follower(cfg).unwrap())));
    primary.tokens().add(
        pagesift_cluster::WorkerId::new("standby"),
        &common::token("standby"),
    );
    let (a, b) = tokio::io::duplex(1 << 16);
    let g = primary.clone();
    let serve = tokio::spawn(async move { g.serve(b).await });
    let s = standby.clone();
    let follow = tokio::spawn(async move { s.follow(a, &common::dial("standby")).await });
    standby.caught_up(p.journal().unwrap().len()).await;
    // 继续写，主网关再做快照，温备跟着截断
    for i in 50..400u64 {
        p.submit(task(i, "y")).unwrap();
    }
    standby.caught_up(p.journal().unwrap().len()).await;
    let sj = standby.ledger().journal().unwrap();
    assert_eq!(sj.head(), p.journal().unwrap().head());
    serve.abort();
    let _ = follow.await;

    standby.take_over().unwrap();
    let l = standby.ledger();
    let w = pagesift_cluster::WorkerId::new("w");
    l.join(&w, 1.0, Duration::ZERO);
    let mut n = 0;
    while let Some(x) = l.pull(Some(&w), &free(), Duration::ZERO).unwrap() {
        l.commit(x.task, x.fence(), None, None, |_| {}).unwrap();
        n += 1;
    }
    assert_eq!(n, 400);
}

/// 多个线程同时拉取：每个线程各自轮转，合起来的比例仍是配额比 1:3。
#[test]
fn fair_shares_hold_across_threads() {
    let clock = ManualClock::new();
    let mut cfg = LedgerConfig::new(clock, true);
    cfg.shards = 8;
    let l = Arc::new(Ledger::open(cfg).unwrap());
    for (a, p) in [("a", 10_000), ("b", 30_000)] {
        l.set_limits(
            a,
            AccountLimits {
                pages_per_day: Some(p),
                requests_per_minute: None,
            },
        );
    }
    let mut owner = HashMap::new();
    for i in 0..2_000u64 {
        owner.insert(l.submit_as(task(i, "a"), Some("a")).unwrap(), 'a');
        owner.insert(l.submit_as(task(i, "b"), Some("b")).unwrap(), 'b');
    }
    let owner = Arc::new(owner);
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let (l, owner) = (l.clone(), owner.clone());
            std::thread::spawn(move || {
                let mut n = [0i64; 2];
                for _ in 0..400 {
                    let x = l.pull(None, &free(), Duration::ZERO).unwrap().unwrap();
                    n[usize::from(owner[&x.task] == 'b')] += 1;
                    l.commit(x.task, x.fence(), None, None, |_| {}).unwrap();
                }
                n
            })
        })
        .collect();
    let (mut a, mut b) = (0, 0);
    for t in threads {
        let [x, y] = t.join().unwrap();
        a += x;
        b += y;
    }
    assert!((3 * a - b).abs() <= 4 * 3, "a={a} b={b}");
}
