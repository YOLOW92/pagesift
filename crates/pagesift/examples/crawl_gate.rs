//! 爬取性能回归线：进程内起一个多站点源站（合成页面，确定性生成），用完整的 Scraper 不限速地
//! 爬全部站点，检查吞吐和峰值内存（VmHWM），低于下限或超过上限就以非零状态退出。
//! 限值留了机器负载波动的余量，用来抓明显的退化（比如内存随在途页面无界增长、吞吐掉一个量级）；
//! 和对手的正式比较用 benchmarks/crawl。
//! CI 里跑：`cargo run --release -p pagesift --example crawl_gate`

use std::fmt::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::Path;
use axum::http::HeaderMap;
use axum::response::Html;
use axum::routing::get;
use pagesift::api::types::{CrawlRequest, Document};
use pagesift::api::{ApiError, BoxFuture, CrawlSink};
use pagesift::{Scraper, ScraperConfig};

const SITES: usize = 20;
const PAGES: usize = 60;
/// 吞吐下限（页/秒，越高越好）和峰值内存上限（MB，越低越好）。本机基线约 6000 页/秒、41 MB。
const MIN_PAGES_PER_S: f64 = 1500.0;
const MAX_PEAK_MB: f64 = 120.0;

/// 约 80 KB 的文章页：导航、正文段落、站内链接（第 j 页链接到 3j+1…3j+3）。
fn page(site: usize, j: usize) -> String {
    let mut s = format!(
        "<!doctype html><html><head><title>Site {site} page {j}</title></head><body><nav><ul>"
    );
    for i in 0..30 {
        let _ = write!(s, "<li><a href=\"/\">Menu {i}</a></li>");
    }
    s.push_str("</ul></nav><article><h1>Page heading</h1>");
    for i in 0..220 {
        let _ = write!(
            s,
            "<p>Paragraph {i} of page {j} on site {site} explains the topic in detail, with \
             <strong>emphasis</strong>, <em>asides</em> and enough words to read like prose.</p>"
        );
    }
    for k in [3 * j + 1, 3 * j + 2, 3 * j + 3] {
        if k < PAGES {
            let _ = write!(s, "<p><a href=\"/p/{k}\">Continue with part {k}</a></p>");
        }
    }
    s.push_str("</article><footer>Footer</footer></body></html>");
    s
}

fn site_of(headers: &HeaderMap) -> usize {
    headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.split('.').next()?.strip_prefix('s')?.parse().ok())
        .unwrap_or(0)
}

#[derive(Default)]
struct Count(AtomicUsize, AtomicUsize);

impl CrawlSink for Count {
    fn page(&self, _: Document) -> BoxFuture<'_, ()> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Box::pin(async {})
    }
    fn error<'a>(&'a self, url: &'a str, err: ApiError) -> BoxFuture<'a, ()> {
        eprintln!("{url}: {} {}", err.code.as_str(), err.message);
        self.1.fetch_add(1, Ordering::Relaxed);
        Box::pin(async {})
    }
    fn total(&self, _: usize) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn cancelled(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { false })
    }
}

/// 进程的峰值常驻内存，MB。
fn peak_rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
        })
        .map_or(0.0, |kb| kb / 1024.0)
}

fn main() -> ExitCode {
    pagesift_sys::tune_allocator();
    let origin = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let port = origin.block_on(async {
        let app = Router::new()
            .route("/robots.txt", get(|| async { "User-agent: *\nAllow: /\n" }))
            .route(
                "/",
                get(|h: HeaderMap| async move {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    Html(page(site_of(&h), 0))
                }),
            )
            .route(
                "/p/{j}",
                get(|h: HeaderMap, Path(j): Path<usize>| async move {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    Html(page(site_of(&h), j))
                }),
            );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        port
    });

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (pages, errors, secs) = rt.block_on(async {
        let mut cfg = ScraperConfig::default();
        cfg.fetcher.ssrf.allow_private_network = true;
        cfg.policy.rate.default_interval = Duration::ZERO;
        cfg.policy.rate.per_host_concurrency = 4;
        let scraper = Arc::new(Scraper::new(cfg).unwrap());
        let sink = Arc::new(Count::default());
        let t = Instant::now();
        let crawls = (0..SITES).map(|s| {
            let (scraper, sink) = (scraper.clone(), sink.clone());
            tokio::spawn(async move {
                let req: CrawlRequest = serde_json::from_value(serde_json::json!({
                    "url": format!("http://s{s}.localhost:{port}/"),
                    "limit": PAGES,
                    "sitemap": "skip",
                    "scrapeOptions": { "formats": ["markdown"], "engine": "http" },
                }))
                .unwrap();
                scraper.crawl(&req, sink).await.unwrap();
            })
        });
        futures_util::future::join_all(crawls).await;
        (
            sink.0.load(Ordering::Relaxed),
            sink.1.load(Ordering::Relaxed),
            t.elapsed().as_secs_f64(),
        )
    });
    let rate = pages as f64 / secs;
    let peak = peak_rss_mb();
    let expected = SITES * PAGES;
    let ok = pages == expected && errors == 0 && rate >= MIN_PAGES_PER_S && peak <= MAX_PEAK_MB;
    println!(
        "crawl {SITES}x{PAGES}: {pages}/{expected} pages, {errors} errors, {rate:.0} pages/s \
         (min {MIN_PAGES_PER_S:.0}), peak RSS {peak:.0} MB (max {MAX_PEAK_MB:.0}) {}",
        if ok { "ok" } else { "FAIL" }
    );
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
