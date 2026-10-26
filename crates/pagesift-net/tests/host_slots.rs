//! 同一源站上，慢请求不能把快请求挡在热连接名额外面（批量抓取里快页面被慢页面拖住）。

mod common;

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{FakeDns, ok, serve};
use pagesift_net::{FetchRequest, Fetcher, FetcherConfig, SsrfPolicy};

#[tokio::test]
async fn slow_requests_do_not_block_fast_ones_on_the_same_origin() {
    // `/slow` 不回应，`/fast` 立刻回应
    let srv = serve(|s| (s.path != "/slow").then(|| ok("fast"))).await;
    let fetcher = Arc::new(
        Fetcher::new(FetcherConfig {
            ssrf: SsrfPolicy {
                allow_ips: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
                ..Default::default()
            },
            dns: FakeDns::new(&[]),
            tail_threshold: Duration::from_millis(500),
            first_byte_timeout: Duration::from_secs(20),
            max_connections_per_host: 16,
            ..Default::default()
        })
        .unwrap(),
    );

    // 热连接目标从 2 起步：7 个慢请求排在前面。按名额一代代释放（每个阈值放 2 个）
    // 快请求要等 4 个阈值；排队方等满一个阈值就放行，与前面有几个慢请求无关
    let mut slow = tokio::task::JoinSet::new();
    for _ in 0..7 {
        let (f, url) = (fetcher.clone(), srv.url("/slow"));
        slow.spawn(async move { f.fetch(FetchRequest::new(url)).await });
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    let t = Instant::now();
    let r = tokio::time::timeout(
        Duration::from_secs(5),
        fetcher.fetch(FetchRequest::new(srv.url("/fast"))),
    )
    .await
    .expect("fast request waited behind slow ones")
    .unwrap();
    assert_eq!(r.status, 200);
    assert!(t.elapsed() < Duration::from_millis(1500), "{:?}", t.elapsed());

    let stats = fetcher.host_connection_stats();
    let s = stats
        .iter()
        .find(|s| s.origin.ends_with(&srv.addr.to_string()))
        .unwrap();
    assert!(s.active <= 2 && s.active + s.parked == 7, "{s:?}");
    slow.abort_all();
}

#[tokio::test]
async fn parked_requests_still_count_toward_the_ceiling() {
    let srv = serve(|s| (s.path != "/slow").then(|| ok("fast"))).await;
    let fetcher = Arc::new(
        Fetcher::new(FetcherConfig {
            ssrf: SsrfPolicy {
                allow_ips: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
                ..Default::default()
            },
            dns: FakeDns::new(&[]),
            tail_threshold: Duration::from_millis(100),
            first_byte_timeout: Duration::from_secs(20),
            max_connections_per_host: 2,
            ..Default::default()
        })
        .unwrap(),
    );
    let mut slow = tokio::task::JoinSet::new();
    for _ in 0..2 {
        let (f, url) = (fetcher.clone(), srv.url("/slow"));
        slow.spawn(async move { f.fetch(FetchRequest::new(url)).await });
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    // 两个都停放了，但上限是 2：第三个只能等
    let blocked = tokio::time::timeout(
        Duration::from_millis(500),
        fetcher.fetch(FetchRequest::new(srv.url("/fast"))),
    )
    .await;
    assert!(blocked.is_err(), "ceiling ignored");
    slow.abort_all();
    while slow.join_next().await.is_some() {}
    let r = tokio::time::timeout(
        Duration::from_secs(5),
        fetcher.fetch(FetchRequest::new(srv.url("/fast"))),
    )
    .await
    .expect("slot not released after cancel")
    .unwrap();
    assert_eq!(r.status, 200);
}
