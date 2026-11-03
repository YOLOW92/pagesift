//! 两种模式行为一致：同一组测试分别跑单主机与"网关 + 3 个工人"（真实帧协议，内存管道）。

mod common;

use std::collections::BTreeMap;

use common::{Harness, Mode, free, task};
use pagesift_cluster::{
    Error, Owner, Renewal, Resource, Resources, Scope, SiteKey, StatKind, StatsDelta,
};
use pagesift_stat::Stat;
use pagesift_stat::beta::BetaCounts;
use pagesift_stat::table::{Key, MetricId, Mode as TableMode, StatTable};

const MODES: [Mode; 2] = [Mode::Local, Mode::Cluster(3)];

/// 所有任务恰好交付一次，结果与任务一一对应。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_task_delivered_exactly_once() {
    for mode in MODES {
        let mut h = Harness::new(mode).await;
        let mut want = BTreeMap::new();
        for i in 0..40u64 {
            let body = format!("page-{i}");
            let id = h.front.submit(task(i % 7, &body)).await.unwrap();
            want.insert(id, body.into_bytes());
        }
        assert_eq!(h.drain().await, 40, "{mode:?}");
        let got: BTreeMap<_, _> = h
            .collect()
            .into_iter()
            .map(|c| (c.task, c.result))
            .collect();
        assert_eq!(got, want, "{mode:?}");
    }
}

/// 同一租约重复提交是幂等的：都返回 Ok，结果只交付一次。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_commit_is_idempotent() {
    for mode in MODES {
        let mut h = Harness::new(mode).await;
        h.front.submit(task(1, "a")).await.unwrap();
        let (w, l) = pull_any(&h).await;
        w.commit(&l, b"a".to_vec()).await.unwrap();
        w.commit(&l, b"a".to_vec()).await.unwrap();
        assert_eq!(h.collect().len(), 1, "{mode:?}");
        // 已完成任务的续约让持有者停下
        assert_eq!(w.renew(&l).await.unwrap(), Renewal::Cancel, "{mode:?}");
    }
}

/// 取消：排队中的不再分配；执行中的在下一次续约时得知，之后的提交被拒绝。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_queued_and_running() {
    for mode in MODES {
        let mut h = Harness::new(mode).await;
        let queued = h.front.submit(task(2, "q")).await.unwrap();
        assert!(h.front.cancel(queued).await.unwrap());
        assert_eq!(h.drain().await, 0, "{mode:?}");

        let running = h.front.submit(task(3, "r")).await.unwrap();
        let (w, l) = pull_any(&h).await;
        assert_eq!(l.task, running);
        assert!(matches!(
            w.renew(&l).await.unwrap(),
            Renewal::Extended { .. }
        ));
        assert!(h.front.cancel(running).await.unwrap());
        assert_eq!(w.renew(&l).await.unwrap(), Renewal::Cancel, "{mode:?}");
        assert_eq!(
            w.commit(&l, b"r".to_vec()).await,
            Err(Error::Cancelled),
            "{mode:?}"
        );
        assert!(h.collect().is_empty(), "{mode:?}");
    }
}

/// 资源不够的任务不分配，够了再分配。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pull_respects_free_resources() {
    for mode in MODES {
        let h = Harness::new(mode).await;
        let mut t = task(4, "big");
        t.need = Resources::default().with(Resource::Cpu, 3.0);
        h.front.submit(t).await.unwrap();
        let small = Resources::default().with(Resource::Cpu, 2.0);
        for w in &h.workers {
            assert!(w.pull(small).await.is_none(), "{mode:?}");
        }
        assert!(pull_any(&h).await.1.work.body == b"big");
    }
}

/// 站点归属：单主机全归本进程；多服务器下网关与所有工人算出的负责人一致，且每个工人只拿到自己站点的任务。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn site_ownership_is_consistent() {
    for mode in MODES {
        let h = Harness::new(mode).await;
        for s in 0..50u64 {
            let o = h.front.owner_of(SiteKey(s));
            for w in &h.workers {
                assert_eq!(w.owner_of(SiteKey(s)), o, "{mode:?}");
            }
            match mode {
                Mode::Local => assert_eq!(o, Owner::Local),
                Mode::Cluster(_) => assert!(matches!(o, Owner::Worker(_))),
            }
        }
        if let Mode::Cluster(_) = mode {
            for s in 0..20u64 {
                h.front.submit(task(s, "x")).await.unwrap();
            }
            for (i, w) in h.workers.iter().enumerate() {
                let me = Owner::Worker(pagesift_cluster::WorkerId::new(format!("w{i}")));
                while let Some(l) = w.pull(free()).await {
                    assert_eq!(h.front.owner_of(l.work.site), me);
                    w.commit(&l, Vec::new()).await.unwrap();
                }
            }
        }
    }
}

fn delta(seq: u64, ok: f64) -> StatsDelta {
    let mut t: StatTable<BetaCounts> = StatTable::new(TableMode::Live);
    let key = Key {
        scope: Scope::Global,
        metric: MetricId(1),
    };
    t.update(key, |c| c.observe_weighted(ok, 1.0));
    StatsDelta {
        kind: StatKind(7),
        seq,
        table: t.encode(),
    }
}

/// 统计增量乱序、重复发送时合并结果不变，先验能读回。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stats_out_of_order_and_duplicate() {
    for mode in MODES {
        let h = Harness::new(mode).await;
        h.ledger.register_stat::<BetaCounts>(StatKind(7));
        let w = &h.workers[0];
        for seq in [3, 0, 2, 0, 1, 3, 2] {
            w.publish_stats(delta(seq, seq as f64 + 1.0));
        }
        // 同一连接上的帧按序处理：一次往返之后，前面的统计帧都已合并
        let _ = w.pull(Resources::default()).await;
        let p = h.front.priors(Scope::Global).unwrap();
        let t: StatTable<BetaCounts> = StatTable::decode(&p.tables[0].1, TableMode::Live).unwrap();
        let mut c = BetaCounts::default();
        for (_, v) in t.iter() {
            c.merge(v);
        }
        assert_eq!(
            (c.success, c.failure),
            (1.0 + 2.0 + 3.0 + 4.0, 4.0),
            "{mode:?}"
        );
    }
}

async fn pull_any(h: &Harness) -> (common::Coord, pagesift_cluster::Lease) {
    for w in &h.workers {
        if let Some(l) = w.pull(free()).await {
            return (w.clone(), l);
        }
    }
    panic!("nothing to pull");
}
