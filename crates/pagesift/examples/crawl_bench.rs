//! 整站爬取压测：同时爬 bench/crawl 源站上的全部站点，每站从首页广度优先爬到上限。
//! 按真实 Scraper 的礼貌规则走（robots、每主机间隔、每主机并发），只把间隔和并发做成参数。
//! `cargo run --release -p pagesift --example crawl_bench -- <port> <hosts> <pages> <interval-ms> [per-host]`

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use pagesift::api::types::{CrawlRequest, Document};
use pagesift::api::{ApiError, BoxFuture, CrawlSink};
use pagesift::{Scraper, ScraperConfig};

#[derive(Default)]
struct Count {
    pages: AtomicUsize,
    errors: AtomicUsize,
    markdown: AtomicUsize,
}

impl CrawlSink for Count {
    fn page(&self, doc: Document) -> BoxFuture<'_, ()> {
        self.pages.fetch_add(1, Ordering::Relaxed);
        self.markdown
            .fetch_add(doc.markdown.map_or(0, |m| m.len()), Ordering::Relaxed);
        Box::pin(async {})
    }
    fn error<'a>(&'a self, url: &'a str, err: ApiError) -> BoxFuture<'a, ()> {
        if self.errors.fetch_add(1, Ordering::Relaxed) < 50 {
            eprintln!("{url}: {} {}", err.code.as_str(), err.message);
        }
        Box::pin(async {})
    }
    fn total(&self, _: usize) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn cancelled(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { false })
    }
}

/// 统计用的分配器：当前堆大小、峰值、大块分配次数（只在压测里用）。
mod track {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    pub static LIVE: AtomicUsize = AtomicUsize::new(0);
    pub static PEAK: AtomicUsize = AtomicUsize::new(0);
    /// 按大小分桶：<64K、64K-1M、1M-8M、>=8M 的分配次数
    pub static BIG: [AtomicUsize; 4] = [const { AtomicUsize::new(0) }; 4];
    /// 默认关闭：计数用的原子变量在线程间争用，会让分配慢一截。HEAP_TRACK=1 打开。
    pub static ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    pub struct Track;
    unsafe impl GlobalAlloc for Track {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            if !ON.load(Relaxed) {
                return unsafe { System.alloc(l) };
            }
            let n = LIVE.fetch_add(l.size(), Relaxed) + l.size();
            PEAK.fetch_max(n, Relaxed);
            let b = match l.size() {
                0..65536 => 0,
                65536..1048576 => 1,
                1048576..8388608 => 2,
                _ => 3,
            };
            BIG[b].fetch_add(1, Relaxed);
            unsafe { System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            if !ON.load(Relaxed) {
                return unsafe { System.dealloc(p, l) };
            }
            LIVE.fetch_sub(l.size(), Relaxed);
            unsafe { System.dealloc(p, l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
            if !ON.load(Relaxed) {
                return unsafe { System.realloc(p, l, new) };
            }
            let n = if new > l.size() {
                LIVE.fetch_add(new - l.size(), Relaxed) + new - l.size()
            } else {
                LIVE.fetch_sub(l.size() - new, Relaxed) - (l.size() - new)
            };
            PEAK.fetch_max(n, Relaxed);
            let b = match new {
                0..65536 => 0,
                65536..1048576 => 1,
                1048576..8388608 => 2,
                _ => 3,
            };
            BIG[b].fetch_add(1, Relaxed);
            unsafe { System.realloc(p, l, new) }
        }
    }
}

#[global_allocator]
static ALLOC: track::Track = track::Track;

#[tokio::main]
async fn main() {
    if std::env::var_os("PAGESIFT_LOG").is_some() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_env("PAGESIFT_LOG"))
            .with_writer(std::io::stderr)
            .init();
    }
    track::ON.store(std::env::var_os("HEAP_TRACK").is_some(), Ordering::Relaxed);
    if std::env::var_os("NO_TUNE").is_none() {
        pagesift_sys::tune_allocator();
    }
    let mut args = std::env::args().skip(1);
    let usage = "usage: crawl_bench <port> <hosts> <pages> <interval-ms> [per-host-concurrency]";
    let port: u16 = args.next().expect(usage).parse().unwrap();
    let hosts: usize = args.next().expect(usage).parse().unwrap();
    let pages: usize = args.next().expect(usage).parse().unwrap();
    let interval = Duration::from_millis(args.next().expect(usage).parse().unwrap());
    let per_host: usize = args.next().map_or(2, |v| v.parse().unwrap());

    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    if let Some(n) = std::env::var("POOL_IDLE").ok().and_then(|v| v.parse().ok()) {
        cfg.fetcher.pool_max_idle_per_host = n;
    }
    cfg.policy.rate.default_interval = interval;
    cfg.policy.rate.per_host_concurrency = per_host;
    if let Some(g) = std::env::var("GLOBAL").ok().and_then(|v| v.parse().ok()) {
        cfg.policy.rate.global_concurrency = g;
    }
    // 浏览器内核（ENGINE=browser 时用）：BROWSER_EXE、BROWSER_ARGS（可用 {port}、{user_data_dir}）、
    // BROWSER_MAX、BROWSER_TABS、BROWSER_RECYCLE（每个进程处理多少页后换新进程）
    if let Ok(exe) = std::env::var("BROWSER_EXE") {
        cfg.browser.source = pagesift_cdp::BrowserSource::Executable(exe.into());
    }
    if let Ok(a) = std::env::var("BROWSER_ARGS") {
        cfg.browser.args_override = Some(a.split_whitespace().map(str::to_owned).collect());
    }
    if let Ok(a) = std::env::var("BROWSER_EXTRA") {
        cfg.browser.extra_args = a.split("||").map(str::to_owned).collect();
    }
    let env_num = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<usize>().ok());
    if let Some(n) = env_num("BROWSER_MAX") {
        cfg.browser.max_browsers = n;
    }
    if let Some(n) = env_num("BROWSER_TABS") {
        cfg.browser.max_tabs_per_browser = n;
    }
    if std::env::var("BROWSER_LOOPBACK").is_ok_and(|v| v == "1") {
        cfg.browser.loopback_only = true;
    }
    if std::env::var("BROWSER_ENGINE_GUARD").is_ok_and(|v| v == "1") {
        cfg.browser.engine_network_guard = true;
    }
    if let Some(n) = env_num("BROWSER_RECYCLE") {
        cfg.browser.recycle_after_pages = n;
    }
    let scraper = Arc::new(Scraper::new(cfg).unwrap());
    let sink = Arc::new(Count::default());
    let t = Instant::now();
    let crawls = (0..hosts).map(|h| {
        let (scraper, sink) = (scraper.clone(), sink.clone());
        tokio::spawn(async move {
            let req: CrawlRequest = serde_json::from_value(serde_json::json!({
                "url": format!("http://s{h}.localhost:{port}/"),
                "limit": pages,
                "sitemap": "skip",
                "maxConcurrency": std::env::var("CRAWL_CONC").ok().and_then(|v| v.parse::<usize>().ok()),
                "scrapeOptions": { "formats": ["markdown"], "engine": std::env::var("ENGINE").unwrap_or_else(|_| "http".into()) },
            }))
            .unwrap();
            if let Err(e) = scraper.crawl(&req, sink).await {
                eprintln!("s{h}: {} {}", e.code.as_str(), e.message);
            }
        })
    });
    futures_util::future::join_all(crawls).await;
    let s = t.elapsed().as_secs_f64();
    let n = sink.pages.load(Ordering::Relaxed);
    println!(
        "pagesift: {n} pages from {hosts} sites in {s:.2} s = {:.0} pages/s, errors {}, markdown {:.1} MB",
        n as f64 / s,
        sink.errors.load(Ordering::Relaxed),
        sink.markdown.load(Ordering::Relaxed) as f64 / 1e6
    );
    if !track::ON.load(Ordering::Relaxed) {
        return;
    }
    eprintln!(
        "heap peak {:.1} MB, allocations <64K {} 64K-1M {} 1-8M {} >=8M {}",
        track::PEAK.load(Ordering::Relaxed) as f64 / 1e6,
        track::BIG[0].load(Ordering::Relaxed),
        track::BIG[1].load(Ordering::Relaxed),
        track::BIG[2].load(Ordering::Relaxed),
        track::BIG[3].load(Ordering::Relaxed)
    );
}
