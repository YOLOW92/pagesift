//! 源站并发控制在“延迟随负载变化”的本地源站上的行为：延迟随并发上升时收缩、负载降下来后
//! 恢复；延迟和并发无关时不收缩；冷启动（样本不足 8 个）时“慢”按尾部阈值算。

mod common;

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use common::{FakeDns, ok};
use pagesift_net::{FetchRequest, Fetcher, FetcherConfig, SsrfPolicy};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const BASE_MS: u64 = 20;
const STEP_MS: u64 = 20;

/// 本地源站。`load_dependent` 打开时，每个请求的延迟 = 20 ms + 20 ms ×（到达时已在处理的
/// 其他请求数），即并发越高越慢（被压垮）；关掉时固定 20 ms。路径带 `?ms=N` 时固定 N 毫秒。
struct Origin {
    addr: std::net::SocketAddr,
    load_dependent: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl Origin {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let load_dependent = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (ld, act, pk) = (load_dependent.clone(), active.clone(), peak.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let (ld, act, pk) = (ld.clone(), act.clone(), pk.clone());
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&buf).into_owned();
                    let fixed = head
                        .split_once("ms=")
                        .and_then(|(_, rest)| rest.split(|c: char| !c.is_ascii_digit()).next())
                        .and_then(|n| n.parse::<u64>().ok());
                    let others = act.fetch_add(1, Ordering::SeqCst);
                    pk.fetch_max(others + 1, Ordering::SeqCst);
                    let ms = match fixed {
                        Some(ms) => ms,
                        None if ld.load(Ordering::SeqCst) => BASE_MS + STEP_MS * others as u64,
                        None => BASE_MS,
                    };
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    act.fetch_sub(1, Ordering::SeqCst);
                    let _ = sock.write_all(&ok("page")).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        Self {
            addr,
            load_dependent,
            active,
            peak,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    fn take_peak(&self) -> usize {
        self.peak
            .swap(self.active.load(Ordering::SeqCst), Ordering::SeqCst)
    }
}

fn fetcher(tail: Duration) -> Arc<Fetcher> {
    Arc::new(
        Fetcher::new(FetcherConfig {
            ssrf: SsrfPolicy {
                allow_ips: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
                ..Default::default()
            },
            dns: FakeDns::new(&[]),
            tail_threshold: tail,
            first_byte_timeout: Duration::from_secs(20),
            max_connections_per_host: 16,
            ..Default::default()
        })
        .unwrap(),
    )
}

/// 同时发 `n` 个请求，等全部结束。
async fn burst(f: &Arc<Fetcher>, o: &Origin, n: usize) {
    let mut set = tokio::task::JoinSet::new();
    for i in 0..n {
        let (f, url) = (f.clone(), o.url(&format!("/p{i}")));
        set.spawn(async move { f.fetch(FetchRequest::new(url)).await.unwrap() });
    }
    while let Some(r) = set.join_next().await {
        assert_eq!(r.unwrap().status, 200);
    }
}

fn target(f: &Fetcher, o: &Origin) -> usize {
    f.host_connection_stats()
        .iter()
        .find(|s| s.origin.ends_with(&o.addr.to_string()))
        .map_or(0, |s| s.hot_target)
}

#[tokio::test]
async fn latency_rising_with_load_shrinks_and_recovers() {
    let o = Origin::start().await;
    let f = fetcher(Duration::from_secs(2));
    // 低负载下先建立这个源站的基线延迟（20 ms）
    for i in 0..16 {
        let r = f
            .fetch(FetchRequest::new(o.url(&format!("/w{i}"))))
            .await
            .unwrap();
        assert_eq!(r.status, 200);
    }

    // 延迟随并发上升：并发一上去延迟就超过基线 1.5 倍，目标退回、停放名额收紧，
    // 源站上同时进行的请求数保持在很低的水平
    o.load_dependent.store(true, Ordering::SeqCst);
    o.take_peak();
    burst(&f, &o, 96).await;
    let loaded_peak = o.take_peak();
    let loaded_target = target(&f, &o);
    eprintln!("loaded: target {loaded_target}, origin peak {loaded_peak}");
    assert!(loaded_target <= 2, "target {loaded_target}");
    assert!(loaded_peak <= 3, "peak {loaded_peak}");

    // 负载特性恢复正常（延迟不再随并发上升）：目标重新逐窗口增长
    o.load_dependent.store(false, Ordering::SeqCst);
    burst(&f, &o, 160).await;
    let recovered = target(&f, &o);
    eprintln!("recovered: target {recovered}");
    assert!(recovered >= 4, "target {recovered} after recovery");
    assert!(o.take_peak() >= 4);
}

#[tokio::test]
async fn latency_independent_of_load_does_not_shrink() {
    // 对照：同样的并发，延迟固定 20 ms，目标照常增长
    let o = Origin::start().await;
    let f = fetcher(Duration::from_secs(2));
    for i in 0..16 {
        f.fetch(FetchRequest::new(o.url(&format!("/w{i}"))))
            .await
            .unwrap();
    }
    burst(&f, &o, 96).await;
    let t = target(&f, &o);
    eprintln!(
        "constant latency: target {t}, origin peak {}",
        o.take_peak()
    );
    assert!(t >= 4, "target {t}");
}

#[tokio::test]
async fn cold_start_uses_the_tail_threshold_until_eight_samples() {
    // 还没有样本时不知道这个源站正常要多久：一律按尾部阈值（这里 600 ms）判断“慢”。
    // 若按 250 ms 的下限算，一个页面本来就大、响应本来就慢的源站，开头的请求都会被停放、
    // 额外多开连接。攒够 8 个样本后才按“中位数 × 4，不低于 250 ms”判断
    let o = Origin::start().await;
    let f = fetcher(Duration::from_millis(600));
    let parked = |f: &Fetcher| {
        f.host_connection_stats()
            .iter()
            .find(|s| s.origin.ends_with(&o.addr.to_string()))
            .map_or(0, |s| s.parked)
    };

    // 冷启动：450 ms 的请求过了 350 ms 还占着热连接（没到 600 ms）
    let (f1, url) = (f.clone(), o.url("/slow?ms=450"));
    let slow = tokio::spawn(async move { f1.fetch(FetchRequest::new(url)).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(
        parked(&f),
        0,
        "cold start parks only after the tail threshold"
    );
    assert_eq!(slow.await.unwrap().status, 200);

    // 攒够 8 个 20 ms 的样本：阈值降到下限 250 ms，同样的请求 350 ms 时已停放
    for i in 0..8 {
        f.fetch(FetchRequest::new(o.url(&format!("/w{i}"))))
            .await
            .unwrap();
    }
    let (f2, url) = (f.clone(), o.url("/slow?ms=450"));
    let slow = tokio::spawn(async move { f2.fetch(FetchRequest::new(url)).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(
        parked(&f),
        1,
        "warm origin parks after its own slow threshold"
    );
    assert_eq!(slow.await.unwrap().status, 200);
}
