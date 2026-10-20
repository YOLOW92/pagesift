//! 端到端抓取压测：本地源站轮流返回评测集里的真实页面，完整的 Scraper 并发去抓
//! （HTTP、robots、限速、解码、提取全走一遍）。源站单独一个运行时，不和抓取方抢线程。
//! `cargo run --release -p pagesift --example fetch_bench -- bench/data/inputs 4000 64`

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Html;
use axum::routing::get;
use futures_util::StreamExt;
use pagesift::api::types::ScrapeOptions;
use pagesift::{Scraper, ScraperConfig};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .expect("usage: fetch_bench <dir> [requests] [concurrency]");
    let requests: usize = args.next().map_or(4000, |v| v.parse().unwrap());
    let concurrency: usize = args.next().map_or(64, |v| v.parse().unwrap());
    let pages: Arc<Vec<String>> = Arc::new(
        std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| std::fs::read(e.ok()?.path()).ok())
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .collect(),
    );

    let origin = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let base = origin.block_on(async {
        let app = Router::new()
            .route("/robots.txt", get(|| async { "User-agent: *\nAllow: /\n" }))
            .route(
                "/p/{i}",
                get(
                    |State(p): State<Arc<Vec<String>>>, Path(i): Path<usize>| async move {
                        Html(p[i % p.len()].clone())
                    },
                ),
            )
            .with_state(pages.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    });

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut cfg = ScraperConfig::default();
        cfg.fetcher.ssrf.allow_private_network = true;
        cfg.policy.rate.default_interval = Duration::ZERO;
        cfg.policy.rate.per_host_concurrency = concurrency;
        cfg.policy.rate.global_concurrency = concurrency;
        let scraper = Scraper::new(cfg).unwrap();
        let opts: ScrapeOptions =
            serde_json::from_value(serde_json::json!({ "engine": "http" })).unwrap();
        let errors = AtomicUsize::new(0);
        let bytes = AtomicUsize::new(0);
        let t = Instant::now();
        futures_util::stream::iter(0..requests)
            .for_each_concurrent(concurrency, |i| {
                let (scraper, opts, base, errors, bytes) = (&scraper, &opts, &base, &errors, &bytes);
                async move {
                    match scraper.scrape(&format!("{base}/p/{i}"), opts).await {
                        Ok(d) => {
                            bytes.fetch_add(d.markdown.map_or(0, |m| m.len()), Ordering::Relaxed);
                        }
                        Err(e) => {
                            if errors.fetch_add(1, Ordering::Relaxed) < 3 {
                                eprintln!("{i}: {} {}", e.code.as_str(), e.message);
                            }
                        }
                    }
                }
            })
            .await;
        let s = t.elapsed().as_secs_f64();
        println!(
            "{requests} scrapes, concurrency {concurrency}: {:.0} pages/s, errors {}, markdown {:.1} MB",
            requests as f64 / s,
            errors.load(Ordering::Relaxed),
            bytes.load(Ordering::Relaxed) as f64 / 1e6
        );
    });
}
