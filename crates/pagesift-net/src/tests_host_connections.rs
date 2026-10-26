//! 源站自适应连接控制器的单元测试。

use super::*;

#[tokio::test]
async fn healthy_saturated_host_grows_without_scanning() {
    let control = HostConnectionControl::new(true, 8);
    for _ in 0..8 {
        let a = control.acquire().await;
        let b = control.acquire().await;
        a.finish(HostOutcome::Success);
        b.finish(HostOutcome::Success);
    }
    let stats = control.stats("http://fast.test".into());
    assert!(stats.hot_target > 2, "healthy saturated host should grow");
    assert_eq!(stats.failed, 0);
}

#[tokio::test]
async fn overload_shrinks_only_its_own_host() {
    let slow = HostConnectionControl::new(true, 8);
    let fast = HostConnectionControl::new(true, 8);
    for _ in 0..16 {
        let permit = fast.acquire().await;
        permit.finish(HostOutcome::Success);
    }
    let permit = slow.acquire().await;
    permit.finish(HostOutcome::Overload);
    assert_eq!(slow.stats("slow".into()).hot_target, 1);
    assert_eq!(fast.stats("fast".into()).hot_target, 2);
}

#[tokio::test]
async fn one_overload_episode_causes_only_one_backoff() {
    let control = HostConnectionControl::new(true, 8);
    while control.stats("host".into()).hot_target < 5 {
        let target = control.stats("host".into()).hot_target;
        for _ in 0..(WINDOW as usize / target).max(1) {
            let mut permits = Vec::new();
            for _ in 0..target {
                permits.push(control.acquire().await);
            }
            for permit in permits {
                permit.finish(HostOutcome::Success);
            }
        }
    }
    let mut overshoot = Vec::new();
    for _ in 0..5 {
        overshoot.push(control.acquire().await);
    }
    for permit in overshoot {
        permit.finish(HostOutcome::Overload);
    }
    assert_eq!(control.stats("host".into()).hot_target, 4);
}

#[tokio::test]
async fn cancelled_acquire_is_released_and_counted() {
    let control = HostConnectionControl::new(true, 2);
    let permit = control.acquire().await;
    drop(permit);
    let stats = control.stats("cancelled".into());
    assert_eq!(stats.active, 0);
    assert_eq!(stats.cancelled, 1);
    assert_eq!(stats.failed, 1);
}

#[tokio::test]
async fn parking_frees_a_hot_slot_but_not_the_ceiling() {
    let control = HostConnectionControl::new(true, 3);
    let wait = |c: &Arc<HostConnectionControl>| {
        let c = c.clone();
        async move {
            tokio::time::timeout(Duration::from_millis(20), c.acquire())
                .await
                .ok()
        }
    };
    let mut a = control.acquire().await;
    let mut b = control.acquire().await;
    assert!(wait(&control).await.is_none(), "target is 2");
    a.park();
    a.park();
    let c = wait(&control).await.expect("parked slot is free");
    b.park();
    assert!(wait(&control).await.is_none(), "1 active + 2 parked = max");
    a.finish(HostOutcome::Success);
    let d = wait(&control)
        .await
        .expect("finished parked request frees the ceiling");
    let stats = control.stats("h".into());
    assert_eq!((stats.active, stats.parked), (2, 1));
    drop((b, c, d));
    let stats = control.stats("h".into());
    assert_eq!((stats.active, stats.parked), (0, 0));
}

#[tokio::test]
async fn impatient_waiters_are_admitted_as_parked_within_the_ceiling() {
    let control = HostConnectionControl::new(true, 4);
    let patience = Duration::from_millis(50);
    let _a = control.acquire().await;
    let _b = control.acquire().await;
    // 前面两个一直不结束：等满耐心后以停放身份放行，两个一起，不分代
    let t = Instant::now();
    let (c, d) = tokio::join!(
        control.acquire_within(patience),
        control.acquire_within(patience)
    );
    assert!(t.elapsed() < Duration::from_millis(500));
    assert!(c.parked && d.parked);
    // 已到上限 4：再等多久也不放行
    assert!(
        tokio::time::timeout(Duration::from_millis(200), control.acquire_within(patience))
            .await
            .is_err()
    );
    drop((c, d));
    let stats = control.stats("h".into());
    assert_eq!((stats.active, stats.parked), (2, 0));
}

#[tokio::test]
async fn slow_threshold_follows_the_origins_service_time() {
    let control = HostConnectionControl::new(true, 8);
    let cap = Duration::from_secs(2);
    // 样本不够时用上限
    let probe = control.acquire_within(cap).await;
    assert!(probe.slow_at() >= tokio::time::Instant::now() + Duration::from_millis(1900));
    probe.finish(HostOutcome::Success);
    for _ in 0..MIN_SAMPLES {
        control.acquire_within(cap).await.finish(HostOutcome::Success);
    }
    // 快源站：阈值降到下限
    let a = control.acquire_within(cap).await;
    let b = control.acquire_within(cap).await;
    assert!(a.slow_at() <= tokio::time::Instant::now() + SLOW_FLOOR);
    // a、b 一直不结束：排队的请求过一个阈值就以停放身份放行，而不是等满上限
    let t = Instant::now();
    let c = control.acquire_within(cap).await;
    assert!(c.parked);
    let waited = t.elapsed();
    assert!(waited >= Duration::from_millis(200) && waited < Duration::from_millis(1000), "{waited:?}");
    drop((a, b, c));
}

#[tokio::test]
async fn a_long_queue_on_a_responsive_origin_is_not_admitted_as_parked() {
    // 源站一直有请求结束（只是排得长）：排队再久也不算“前面都是慢请求”，并发仍由热连接目标决定
    let control = HostConnectionControl::new(true, 32);
    let cap = Duration::from_millis(300);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    for _ in 0..12 {
        let (c, tx) = (control.clone(), tx.clone());
        tokio::spawn(async move {
            let _ = tx.send(c.acquire_within(cap).await);
        });
    }
    let mut held = Vec::new();
    for _ in 0..10 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        while let Ok(p) = rx.try_recv() {
            held.push(p);
        }
        let stats = control.stats("h".into());
        assert_eq!((stats.active, stats.parked), (2, 0));
        held.remove(0).finish(HostOutcome::Success);
    }
}

async fn run_window(control: &Arc<HostConnectionControl>, cap: Duration, each: Duration) {
    for _ in 0..WINDOW {
        let p = control.acquire_within(cap).await;
        tokio::time::sleep(each).await;
        p.finish(HostOutcome::Success);
    }
}

#[tokio::test]
async fn a_constantly_slow_origin_keeps_its_concurrency() {
    // 一直很慢、但延迟和负载无关：不算过载，目标不收缩，慢请求照常停放
    let control = HostConnectionControl::new(true, 32);
    let cap = Duration::from_millis(20);
    run_window(&control, cap, Duration::from_millis(30)).await;
    run_window(&control, cap, Duration::from_millis(30)).await;
    assert_eq!(control.stats("h".into()).hot_target, 2);
    let mut a = control.acquire_within(cap).await;
    let mut b = control.acquire_within(cap).await;
    a.park();
    b.park();
    assert!(a.parked && b.parked);
    drop((a, b));
}

#[tokio::test]
async fn latency_rising_with_load_is_treated_as_overloaded() {
    // 延迟相对这个源站的基线明显上升：目标减一，停放名额收紧到热连接目标
    let control = HostConnectionControl::new(true, 32);
    let cap = Duration::from_millis(20);
    run_window(&control, cap, Duration::from_millis(5)).await;
    run_window(&control, cap, Duration::from_millis(30)).await;
    assert_eq!(control.stats("h".into()).hot_target, 1);
    let mut a = control.acquire_within(cap).await;
    a.park();
    let mut b = control.acquire_within(cap).await;
    b.park();
    assert!(a.parked && !b.parked, "停放名额已满，b 继续占着热连接");
    let stats = control.stats("h".into());
    assert_eq!((stats.active, stats.parked), (1, 1));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), control.acquire_within(cap))
            .await
            .is_err()
    );
    drop((a, b));
}

#[tokio::test]
async fn slow_completions_do_not_reset_the_queue_patience() {
    // 慢请求结束不算进展：一直很慢的源站上，排队的请求照样在一个阈值后以停放身份放行
    let control = HostConnectionControl::new(true, 32);
    let cap = Duration::from_millis(200);
    let t = Instant::now();
    let a = control.acquire_within(cap).await;
    let _b = control.acquire_within(cap).await;
    tokio::time::sleep(Duration::from_millis(210)).await;
    let (c1, c2) = (control.clone(), control.clone());
    let w1 = tokio::spawn(async move { c1.acquire_within(cap).await });
    let w2 = tokio::spawn(async move { c2.acquire_within(cap).await });
    tokio::time::sleep(Duration::from_millis(90)).await;
    a.finish(HostOutcome::Success); // 300 ms：慢请求结束，空出一个热连接
    let (p1, p2) = (w1.await.unwrap(), w2.await.unwrap());
    // 排队从 210 ms 起算，410 ms 前后放行；若慢请求结束也重新计时要等到 500 ms
    assert!(t.elapsed() < Duration::from_millis(470), "{:?}", t.elapsed());
    assert!(p1.parked != p2.parked, "一个拿到空出的热连接，一个以停放身份放行");
}
