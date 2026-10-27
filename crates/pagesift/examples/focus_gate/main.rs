//! 聚焦爬取的精度回归线：进程内起 8 所合成"大学"（见 campus.rs）和 2 个别的领域的站点（见 other.rs，
//! 查询也不同），每站从首页 `crawl --query --adaptive --limit 12`，按 URL 真值统计每校抓到的 U（国际生专属）、M（一般招生）、
//! X（无用）页数，报告总体精度 (U+M)/页数、U 占比、零分校数、每校中位数，以及吞吐和 CPU 时间。
//! 口径和《聚焦爬取改进测试规格》一致，只是站点是合成的。
//! 运行：`cargo run --release -p pagesift --example focus_gate [-- --subdomains] [--attachments] [--no-adaptive] [-v]`
//! 门槛（MIN_*）按 0.80.2 的结果定；低于门槛以非零状态退出。

mod campus;
mod other;

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use campus::{Campus, Kind};
use pagesift::api::types::{CrawlRequest, CrawlSummary, Document};
use pagesift::api::{ApiError, BoxFuture, CrawlSink};
use pagesift::{Scraper, ScraperConfig};

const LIMIT: usize = 12;
/// 总体精度 (U+M)/页数 的下限，和零分校数的上限（按 0.80.2 定，0.80.1 基线见 benchmarks/history）。
const MIN_PRECISION: f64 = 0.75;
/// 允许几个站点一页有用的都没抓到（`usize` 让门槛可以放宽）。
const MAX_ZERO_SITES: usize = 0;

struct Collect {
    /// (URL, 轨迹：深度、分数、父页)
    pages: Mutex<Vec<(String, String)>>,
    errors: Mutex<usize>,
    summary: Mutex<Option<CrawlSummary>>,
}

impl CrawlSink for Collect {
    fn page(&self, doc: Document) -> BoxFuture<'_, ()> {
        let url = doc
            .metadata
            .source_url
            .clone()
            .or(doc.metadata.url.clone())
            .unwrap_or_default();
        let trace = doc
            .metadata
            .pagesift
            .as_ref()
            .and_then(|p| p.crawl.as_ref())
            .map(|t| {
                format!(
                    "d{} s{:.2} <- {}",
                    t.depth,
                    t.score.unwrap_or(0.0),
                    t.parent
                        .as_deref()
                        .map(|p| p
                            .rsplit_once(".localhost:")
                            .map_or(p, |(_, r)| r.split_once('/').map_or("", |(_, p)| p)))
                        .unwrap_or("-")
                )
            })
            .unwrap_or_default();
        self.pages.lock().unwrap().push((url, trace));
        Box::pin(async {})
    }
    fn error<'a>(&'a self, _url: &'a str, _err: ApiError) -> BoxFuture<'a, ()> {
        *self.errors.lock().unwrap() += 1;
        Box::pin(async {})
    }
    fn total(&self, _: usize) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn cancelled(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { false })
    }
    fn summary(&self, s: CrawlSummary) -> BoxFuture<'_, ()> {
        *self.summary.lock().unwrap() = Some(s);
        Box::pin(async {})
    }
}

type Shared = Arc<Vec<Campus>>;

fn host_of(headers: &HeaderMap) -> String {
    headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .map(|h| h.split(':').next().unwrap_or(h).to_ascii_lowercase())
        .unwrap_or_default()
}

async fn serve(State(campuses): State<Shared>, headers: HeaderMap, uri: Uri) -> Response {
    let host = host_of(&headers);
    let path = uri.path().to_string();
    if path == "/robots.txt" {
        return "User-agent: *\nAllow: /\n".into_response();
    }
    let campus = campuses
        .iter()
        .find(|c| c.pages.keys().any(|(h, _)| *h == host));
    let Some(campus) = campus else {
        return (StatusCode::NOT_FOUND, "no such campus").into_response();
    };
    if path == "/sitemap.xml" {
        return match &campus.sitemap {
            Some(urls) if host == campus.host => {
                let mut xml = String::from("<?xml version=\"1.0\"?><urlset>");
                for u in urls {
                    xml.push_str(&format!("<url><loc>{u}</loc></url>"));
                }
                xml.push_str("</urlset>");
                ([("content-type", "application/xml")], xml).into_response()
            }
            _ => (StatusCode::NOT_FOUND, "no sitemap").into_response(),
        };
    }
    tokio::time::sleep(Duration::from_millis(3)).await;
    if path.ends_with(".pdf") {
        return match campus.pages.get(&(host, path)) {
            Some(page) => (
                [("content-type", "application/pdf")],
                Campus::pdf(&page.title),
            )
                .into_response(),
            None => (StatusCode::NOT_FOUND, "no such file").into_response(),
        };
    }
    match campus.pages.get(&(host, path)) {
        Some(page) => (
            StatusCode::from_u16(page.status).unwrap_or(StatusCode::OK),
            Html(campus.render(page)),
        )
            .into_response(),
        None => {
            // 真实站点的 404 也是一个带导航的模板页
            let fake = campus::Page {
                title: "Page not found".into(),
                kind: Kind::X,
                status: 404,
                paragraphs: vec!["The page you requested could not be found.".into()],
                links: Vec::new(),
            };
            (StatusCode::NOT_FOUND, Html(campus.render(&fake))).into_response()
        }
    }
}

struct SiteResult {
    name: &'static str,
    pattern: &'static str,
    u: usize,
    m: usize,
    x: usize,
    pages: usize,
    errors: usize,
    stop: String,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let subdomains = args.iter().any(|a| a == "--subdomains");
    let attachments = args.iter().any(|a| a == "--attachments");
    let adaptive = !args.iter().any(|a| a == "--no-adaptive");
    let verbose = args.iter().any(|a| a == "-v");

    let origin = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (port, campuses) = origin.block_on(async {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let campuses: Shared = Arc::new(Campus::all(port));
        let app = Router::new()
            .route("/", get(serve))
            .fallback(get(serve))
            .with_state(campuses.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (port, campuses)
    });

    let rt = tokio::runtime::Runtime::new().unwrap();
    let t = Instant::now();
    let cpu0 = cpu_seconds();
    let results: Vec<SiteResult> = rt.block_on(async {
        let mut cfg = ScraperConfig::default();
        cfg.fetcher.ssrf.allow_private_network = true;
        cfg.policy.rate.default_interval = Duration::ZERO;
        cfg.policy.rate.per_host_concurrency = 4;
        let scraper = Arc::new(Scraper::new(cfg).unwrap());
        let mut out = Vec::new();
        for campus in campuses.iter() {
            let sink = Arc::new(Collect {
                pages: Mutex::new(Vec::new()),
                errors: Mutex::new(0),
                summary: Mutex::new(None),
            });
            let mut req = serde_json::json!({
                "url": campus.url("/"),
                "limit": LIMIT,
                "query": campus.query,
                "adaptive": adaptive,
                "scrapeOptions": { "formats": ["markdown"], "engine": "http" },
            });
            if subdomains {
                req["allowSubdomains"] = serde_json::json!(true);
            }
            if attachments {
                req["attachments"] = serde_json::json!(true);
            }
            let req: CrawlRequest = serde_json::from_value(req).unwrap();
            let _ = port;
            scraper.crawl(&req, sink.clone()).await.unwrap();
            let pages = sink.pages.lock().unwrap().clone();
            let mut counts: BTreeMap<Kind, usize> = BTreeMap::new();
            let docs = pages.iter().filter(|(u, _)| u.ends_with(".pdf")).count();
            if docs > 0 {
                eprintln!("  {}: {docs} attachment(s)", campus.name);
            }
            for (url, trace) in &pages {
                let kind = campus.kind_of(url).unwrap_or(Kind::X);
                *counts.entry(kind).or_default() += 1;
                if verbose {
                    eprintln!("  {kind:?}  {url}  [{trace}]");
                }
            }
            out.push(SiteResult {
                name: campus.name,
                pattern: campus.pattern,
                u: counts.get(&Kind::U).copied().unwrap_or(0),
                m: counts.get(&Kind::M).copied().unwrap_or(0),
                x: counts.get(&Kind::X).copied().unwrap_or(0),
                pages: pages.len(),
                errors: *sink.errors.lock().unwrap(),
                stop: sink
                    .summary
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|s| s.stop_reason.clone())
                    .unwrap_or_default(),
            });
        }
        out
    });
    let secs = t.elapsed().as_secs_f64();
    let cpu = cpu_seconds() - cpu0;

    println!("site      pattern                              U   M   X  pages err  stop");
    let (mut u_all, mut m_all, mut pages_all, mut zero) = (0, 0, 0, 0);
    let mut scores = Vec::new();
    for r in &results {
        println!(
            "{:<9} {:<35} {:>3} {:>3} {:>3} {:>5} {:>3}  {}",
            r.name, r.pattern, r.u, r.m, r.x, r.pages, r.errors, r.stop
        );
        u_all += r.u;
        m_all += r.m;
        pages_all += r.pages;
        if r.u + r.m == 0 {
            zero += 1;
        }
        scores.push(r.u + r.m);
    }
    scores.sort();
    let median = scores[scores.len() / 2];
    let precision = (u_all + m_all) as f64 / pages_all.max(1) as f64;
    let u_share = u_all as f64 / pages_all.max(1) as f64;
    #[allow(clippy::absurd_extreme_comparisons)]
    let ok = precision >= MIN_PRECISION && zero <= MAX_ZERO_SITES;
    println!(
        "focus {}x{LIMIT}: U+M {}/{pages_all} = {:.1}% (min {:.1}%), U {:.1}%, median {median}/{LIMIT}, \
         zero-score sites {zero} (max {MAX_ZERO_SITES}), {:.1} pages/s, cpu {cpu:.2} s {}",
        results.len(),
        u_all + m_all,
        precision * 100.0,
        MIN_PRECISION * 100.0,
        u_share * 100.0,
        pages_all as f64 / secs,
        if ok { "ok" } else { "FAIL" }
    );
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// 进程的用户态 CPU 时间（秒）。
fn cpu_seconds() -> f64 {
    std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| {
            let fields: Vec<&str> = s.rsplit(')').next()?.split_whitespace().collect();
            let utime: f64 = fields.get(11)?.parse().ok()?;
            let stime: f64 = fields.get(12)?.parse().ok()?;
            Some((utime + stime) / 100.0)
        })
        .unwrap_or(0.0)
}
