//! 故障注入：杀工人、迟到结果、主网关失联温备接管、旧网关写入被栅栏拒绝、站点交接、单主机崩溃恢复。

mod common;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Harness, Mode, connect, free, gateway, task, worker_config};
use pagesift_cluster::{
    Coordinator, Error, Fence, Journal, Ledger, LedgerConfig, LocalCoordinator, ManualClock, Owner,
    Resources, SiteKey, SiteState, SiteStates, Standby, WorkerId,
};
use pagesift_store::Store;
use tokio::sync::{Notify, mpsc};

fn owner_name(h: &Harness, site: u64) -> usize {
    match h.front.owner_of(SiteKey(site)) {
        Owner::Worker(w) => w.as_str()[1..].parse().unwrap(),
        o => panic!("{o:?}"),
    }
}

/// 杀工人（连接断开）：它的站点立即迁走，但在它的租约到期前站点暂停；到期后任务重新分配，
/// 不丢任务、不重复交付；它的旧租约令牌被拒绝。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn killed_worker_lease_expires_and_task_is_reassigned() {
    let mut h = Harness::new(Mode::Cluster(2)).await;
    let site = 5;
    let id = h.front.submit(task(site, "p")).await.unwrap();
    let victim = owner_name(&h, site);
    let survivor = 1 - victim;
    let lease = h.workers[victim].pull(free()).await.unwrap();
    assert_eq!(lease.task, id);

    // 杀掉：丢掉它的协调器与连接
    let victim_id = WorkerId::new(format!("w{victim}"));
    h.gateway.as_ref().unwrap().disconnect(&victim_id);
    h.workers.remove(victim);
    let r = h.remotes.remove(victim);
    drop(r);
    let w = &h.workers[0];
    // 成员表推送到达幸存者之前先做一次往返
    let _ = w.pull(free()).await;
    assert!(!h.ledger.host_map().contains(&victim_id));
    assert!(
        w.pull(free()).await.is_none(),
        "task still leased to the dead worker"
    );
    assert!(h.ledger.paused_until(SiteKey(site)).is_some());

    h.clock.advance(lease.ttl + Duration::from_millis(1));
    let t = h.gateway.as_ref().unwrap().tick();
    assert_eq!(t.expired.len(), 1);
    let again = w.pull(free()).await.expect("reassigned after expiry");
    assert_eq!(again.task, id);
    assert_eq!(again.epoch, lease.epoch + 1);
    w.commit(&again, b"p".to_vec()).await.unwrap();
    // 死去的工人的结果（假设它复活后经别的连接送来）被栅栏拒绝
    let late = h.front.commit(&lease, b"stale".to_vec()).await;
    assert_eq!(
        late,
        Err(Error::Fenced {
            current: again.fence()
        })
    );
    let got = h.collect();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].result, b"p");
    let _ = survivor;
}

/// 失联但连接还在的工人：租约到期且它此后一言不发，就被判失联移出成员表；恢复后自动重新加入。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn silent_worker_is_evicted_on_lease_expiry() {
    let h = Harness::new(Mode::Cluster(2)).await;
    let site = 9;
    h.front.submit(task(site, "s")).await.unwrap();
    let silent = owner_name(&h, site);
    let lease = h.workers[silent].pull(free()).await.unwrap();
    h.clock.advance(lease.ttl + Duration::from_millis(1));
    h.gateway.as_ref().unwrap().tick();
    let id = WorkerId::new(format!("w{silent}"));
    assert!(!h.ledger.host_map().contains(&id));
    let other = &h.workers[1 - silent];
    let _ = other.pull(Default::default()).await;
    let l = other
        .pull(free())
        .await
        .expect("site moved to the live worker");
    other.commit(&l, Vec::new()).await.unwrap();
    // 失联的工人恢复后再说话：重新加入成员表
    let _ = h.workers[silent].pull(Default::default()).await;
    assert!(h.ledger.host_map().contains(&id));
}

/// 迟到结果：租约到期并被重新分配后，原持有者的提交被拒；没被重新分配时仍可续约、提交（两种模式）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_results_are_fenced_only_after_reassignment() {
    for mode in [Mode::Local, Mode::Cluster(1)] {
        let mut h = Harness::new(mode).await;
        let w = h.workers[0].clone();
        h.front.submit(task(1, "x")).await.unwrap();
        let first = w.pull(free()).await.unwrap();
        h.clock.advance(first.ttl + Duration::from_millis(1));
        // 工人还活着（发过帧），只是没续约：不会被判失联
        let _ = w.pull(Resources::default()).await;
        h.tick();
        // 过期但没被重新分配：原持有者续约即重新拿回
        assert!(matches!(
            w.renew(&first).await.unwrap(),
            pagesift_cluster::Renewal::Extended { .. }
        ));
        h.clock.advance(first.ttl + Duration::from_millis(1));
        let _ = w.pull(Resources::default()).await;
        h.tick();
        let second = w.pull(free()).await.unwrap();
        assert_eq!(second.epoch, first.epoch + 1);
        assert_eq!(
            w.commit(&first, b"late".to_vec()).await,
            Err(Error::Fenced {
                current: second.fence()
            }),
            "{mode:?}"
        );
        assert_eq!(
            w.renew(&first).await,
            Err(Error::Fenced {
                current: second.fence()
            })
        );
        w.commit(&second, b"x".to_vec()).await.unwrap();
        let got = h.collect();
        assert_eq!(got.len(), 1, "{mode:?}");
        assert_eq!(got[0].result, b"x");
    }
}

fn store(dir: &tempfile::TempDir) -> Journal {
    Journal::new(Store::open_dir(dir.path()).unwrap())
}

/// 主网关失联，温备接管（term+1）：已复制的租约仍可提交，未完成的任务在新网关上完成且只交付一次；
/// 旧网关在接管后的写入（新租约的结果、日志记录）都被栅栏拒绝；工人不再接受旧任期的网关。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn standby_takes_over_and_old_gateway_is_fenced() {
    let clock = ManualClock::new();
    let (d1, d2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (primary, _old_results) = gateway(clock.clone(), Some(store(&d1)));
    let w0 = connect(&primary, &clock, worker_config("w0", 0, None))
        .await
        .unwrap();
    let mut ids = BTreeSet::new();
    for i in 0..6u64 {
        ids.insert(primary.ledger().submit(task(i, &format!("t{i}"))).unwrap());
    }
    let in_flight = Coordinator::<Vec<u8>>::pull(&*w0, free()).await.unwrap();
    let done_before = Coordinator::<Vec<u8>>::pull(&*w0, free()).await.unwrap();
    Coordinator::<Vec<u8>>::commit(&*w0, &done_before, b"before".to_vec())
        .await
        .unwrap();

    // 温备跟上主网关
    let mut cfg = LedgerConfig::new(clock.clone(), false);
    cfg.shards = 2; // 分片数与主网关不同也能回放
    let standby = Arc::new(Standby::new(Arc::new(
        Ledger::follower(cfg.with_journal(store(&d2))).unwrap(),
    )));
    primary
        .tokens()
        .add(WorkerId::new("standby"), &common::token("standby"));
    let (a, b) = tokio::io::duplex(1 << 16);
    let p = primary.clone();
    let serve = tokio::spawn(async move { p.serve(b).await });
    let s = standby.clone();
    let follow = tokio::spawn(async move { s.follow(a, &common::dial("standby")).await });
    let len = primary.ledger().journal().unwrap().len();
    standby.caught_up(len).await;
    assert!(!standby.primary_lost());

    // 主网关与温备之间断网
    serve.abort();
    let _ = follow.await;
    clock.advance(Duration::from_secs(3));
    assert!(standby.primary_lost());
    let term = standby.take_over().unwrap();
    assert_eq!(term, primary.ledger().term() + 1);

    let (tx, mut results) = mpsc::unbounded_channel();
    let new_gw = pagesift_cluster::GatewayCoordinator::new(
        standby.ledger().clone(),
        Arc::new(tx),
        pagesift_cluster::TokenStore::new(),
        Default::default(),
    );
    let w0b = connect(&new_gw, &clock, worker_config("w0", w0.term(), None))
        .await
        .unwrap();
    assert_eq!(w0b.term(), term);
    // 接管前发出、已复制的租约仍然有效
    Coordinator::<Vec<u8>>::commit(&*w0b, &in_flight, b"in-flight".to_vec())
        .await
        .unwrap();
    // 接管前已完成的再提交是幂等的，不再交付
    Coordinator::<Vec<u8>>::commit(&*w0b, &done_before, b"before".to_vec())
        .await
        .unwrap();

    // 旧网关还活着：它给仍连着它的工人发新租约，结果送到新网关被拒
    let stale = Coordinator::<Vec<u8>>::pull(&*w0, free()).await.unwrap();
    assert_eq!(stale.term, term - 1);
    assert!(matches!(
        Coordinator::<Vec<u8>>::commit(&*w0b, &stale, b"x".to_vec()).await,
        Err(Error::Fenced { .. })
    ));
    // 旧网关此后写的日志记录在新网关回放时被拒
    let old_j = primary.ledger().journal().unwrap();
    let last = old_j.len() - 1;
    let raw = old_j.read(last, usize::MAX).unwrap().remove(0).1;
    assert_eq!(
        standby
            .ledger()
            .apply_shipped(standby.ledger().shipped_len(), &raw),
        Err(Error::Fenced {
            current: Fence { term, epoch: 0 }
        })
    );
    // 见过新任期的工人不再接受旧网关
    let back = connect(&primary, &clock, worker_config("w9", term, None)).await;
    assert!(matches!(back, Err(Error::Fenced { .. })));

    // 新网关上做完其余任务：每个任务恰好交付一次（接管前已完成的那个除外）
    let mut seen = BTreeSet::new();
    while let Some(l) = Coordinator::<Vec<u8>>::pull(&*w0b, free()).await {
        Coordinator::<Vec<u8>>::commit(&*w0b, &l, l.work.body.clone())
            .await
            .unwrap();
    }
    while let Ok(c) = results.try_recv() {
        assert!(seen.insert(c.task), "delivered twice");
    }
    seen.insert(done_before.task);
    assert_eq!(seen, ids);
}

#[derive(Default)]
struct States {
    map: Mutex<HashMap<SiteKey, SiteState>>,
    exported: Notify,
}

impl SiteStates for States {
    fn sites(&self) -> Vec<SiteKey> {
        self.map.lock().unwrap().keys().copied().collect()
    }
    fn export(&self, site: SiteKey) -> Option<SiteState> {
        let s = self.map.lock().unwrap().remove(&site);
        self.exported.notify_one();
        s
    }
    fn import(&self, state: SiteState) {
        self.map.lock().unwrap().insert(state.site, state);
    }
}

/// 站点换负责人：旧负责人把礼貌状态经网关交给新负责人，交接期内站点暂停，新负责人的租约带着状态。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn politeness_state_moves_with_the_site() {
    let clock = ManualClock::new();
    let (gw, _rx) = gateway(clock.clone(), None);
    let s0 = Arc::new(States::default());
    let w0 = connect(&gw, &clock, worker_config("w0", 0, Some(s0.clone())))
        .await
        .unwrap();
    // 找一个加入 w1 后会迁走的站点
    let next = pagesift_cluster::HostMap::new(
        u64::MAX,
        [(WorkerId::new("w0"), 1.0), (WorkerId::new("w1"), 1.0)],
    );
    let site = (0..)
        .map(SiteKey)
        .find(|s| next.owner(*s).unwrap().as_str() == "w1")
        .unwrap();
    let wait = Duration::from_millis(1500);
    s0.import(SiteState {
        site,
        wait,
        state: b"next-allowed".to_vec(),
    });
    gw.ledger().submit(task(site.0, "h")).unwrap();

    let s1 = Arc::new(States::default());
    let w1 = connect(&gw, &clock, worker_config("w1", 0, Some(s1.clone())))
        .await
        .unwrap();
    s0.exported.notified().await;
    // w0 的交接帧在这次往返之前已发出，网关已处理
    let _ = Coordinator::<Vec<u8>>::pull(&*w0, Default::default()).await;
    assert!(gw.ledger().paused_until(site).is_some());
    assert!(
        Coordinator::<Vec<u8>>::pull(&*w1, free()).await.is_none(),
        "paused during handoff"
    );
    clock.advance(wait);
    let l = Coordinator::<Vec<u8>>::pull(&*w1, free())
        .await
        .expect("resumes after the wait");
    assert_eq!(l.handoff.as_ref().unwrap().state, b"next-allowed");
    assert!(s1.sites().contains(&site), "new owner imported the state");
    assert!(s0.sites().is_empty());
}

/// 单主机崩溃恢复：重开后任期加一，排队的任务还在，崩溃前的租约仍可提交，已完成的不再交付。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_restart_recovers_from_journal() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new();
    let open = |c: Arc<ManualClock>| {
        Arc::new(Ledger::open(LedgerConfig::new(c, true).with_journal(store(&dir))).unwrap())
    };
    let l1 = open(clock.clone());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let c1 = LocalCoordinator::<Vec<u8>>::new(l1.clone(), Arc::new(tx), 1);
    for i in 0..3 {
        c1.submit(task(i, "r")).await.unwrap();
    }
    let a = c1.pull(free()).await.unwrap();
    let b = c1.pull(free()).await.unwrap();
    c1.commit(&a, b"a".to_vec()).await.unwrap();
    assert!(rx.try_recv().is_ok());
    let term1 = l1.term();
    drop(c1);
    drop(l1);

    let l2 = open(clock.clone());
    assert_eq!(l2.term(), term1 + 1);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let c2 = LocalCoordinator::<Vec<u8>>::new(l2, Arc::new(tx), 2);
    c2.commit(&a, b"a".to_vec()).await.unwrap();
    c2.commit(&b, b"b".to_vec()).await.unwrap();
    let c = c2.pull(free()).await.unwrap();
    assert_eq!(c.term, term1 + 1);
    c2.commit(&c, b"c".to_vec()).await.unwrap();
    assert!(c2.pull(free()).await.is_none());
    let got: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
        .map(|c| c.result)
        .collect();
    assert_eq!(got, vec![b"b".to_vec(), b"c".to_vec()]);
}

/// 令牌不对的工人连不上。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_token_is_rejected() {
    let clock = ManualClock::new();
    let (gw, _rx) = gateway(clock.clone(), None);
    gw.tokens().add(WorkerId::new("w0"), "right");
    let (a, b) = tokio::io::duplex(1 << 16);
    let g = gw.clone();
    tokio::spawn(async move { g.serve(b).await });
    let mut cfg = worker_config("w0", 0, None);
    cfg.dial.token = "wrong".into();
    let r = pagesift_cluster::RemoteCoordinator::<Vec<u8>>::connect(a, cfg, clock).await;
    assert!(matches!(r, Err(Error::Rejected(m)) if m.contains("unauthorized")));
}
