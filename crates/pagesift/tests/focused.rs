//! 带查询的整站爬取的端到端测试：本地起一个帮助中心式的小站点，和退款相关的页面藏在首页链接列表的最后。
//! 相关性优先时它们先被爬到；自适应爬取在信息够了（或者不再有新信息）时提前停止。
//! 并发设成 1，抓取顺序就是调度顺序。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::response::Html;
use axum::routing::get;
use pagesift::api::CrawlResume;
use pagesift::api::types::{CrawlRequest, CrawlSummary, Document};
use pagesift::api::{ApiError, BoxFuture, CrawlSink};
use pagesift::{Scraper, ScraperConfig};
use serde_json::json;

const FILLER: &str = "describes our company, the people behind it and the things we build every day for customers around the world";

fn page(title: &str, paragraphs: &[&str], links: &[(&str, &str)]) -> Html<String> {
    let mut body = format!("<h1>{title}</h1><p>{title} {FILLER}.</p>");
    for p in paragraphs {
        body.push_str(&format!("<p>{p}</p>"));
    }
    for (href, text) in links {
        body.push_str(&format!("<p>Next: <a href=\"{href}\">{text}</a></p>"));
    }
    Html(format!(
        "<html><head><title>{title}</title></head><body><article>{body}</article></body></html>"
    ))
}

fn plain(title: &'static str, links: &'static [(&'static str, &'static str)]) -> Html<String> {
    page(title, &[], links)
}

/// 14 个页面。
async fn site() -> String {
    let app = Router::new()
        .route(
            "/kb/",
            get(|| async {
                plain(
                    "Help center",
                    &[
                        ("/kb/about", "About us"),
                        ("/kb/careers", "Careers"),
                        ("/kb/blog", "Blog"),
                        ("/kb/team", "Our team"),
                        ("/kb/press", "Press kit"),
                        ("/kb/billing/refund-policy", "Refund policy"),
                    ],
                )
            }),
        )
        .route(
            "/kb/about",
            get(|| async {
                plain(
                    "About",
                    &[("/kb/about/history", "History"), ("/kb/about/office", "Office")],
                )
            }),
        )
        .route(
            "/kb/careers",
            get(|| async { plain("Careers", &[("/kb/careers/jobs", "Open jobs")]) }),
        )
        .route(
            "/kb/blog",
            get(|| async {
                plain(
                    "Blog",
                    &[("/kb/blog/post-1", "First post"), ("/kb/blog/post-2", "Second post")],
                )
            }),
        )
        .route("/kb/team", get(|| async { plain("Team", &[]) }))
        .route("/kb/press", get(|| async { plain("Press", &[]) }))
        .route("/kb/about/history", get(|| async { plain("History", &[]) }))
        .route("/kb/about/office", get(|| async { plain("Office", &[]) }))
        .route("/kb/careers/jobs", get(|| async { plain("Jobs", &[]) }))
        .route("/kb/blog/post-1", get(|| async { plain("Post one", &[]) }))
        .route("/kb/blog/post-2", get(|| async { plain("Post two", &[]) }))
        .route(
            "/kb/billing/refund-policy",
            get(|| async {
                page(
                    "Refund policy",
                    &[
                        "Our refund policy lets you return items within 30 days of purchase.",
                        "Refunds are paid to the original card, and the policy covers every plan.",
                        "If a refund is late, contact billing; this policy was last updated in 2026.",
                    ],
                    &[
                        ("/kb/billing/refund-timeline", "How long a refund takes"),
                        ("/kb/billing/refund-exceptions", "Refund exceptions"),
                    ],
                )
            }),
        )
        .route(
            "/kb/billing/refund-timeline",
            get(|| async {
                page(
                    "Refund timeline",
                    &["A refund reaches your bank within five working days after approval."],
                    &[("/kb/team", "Meet the team")],
                )
            }),
        )
        .route(
            "/kb/billing/refund-exceptions",
            get(|| async {
                page(
                    "Refund exceptions",
                    &["Gift cards and custom orders cannot get a refund once they ship."],
                    &[("/kb/press", "Press kit")],
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn scraper() -> Scraper {
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    Scraper::new(cfg).unwrap()
}

#[derive(Default)]
struct Collect {
    pages: Mutex<Vec<Document>>,
    summary: Mutex<Option<CrawlSummary>>,
    /// 设了就模拟持久化的任务：从这里继续，并记下入队的 URL。
    resume: Mutex<Option<CrawlResume>>,
    track: bool,
    enqueued: Mutex<Vec<(String, usize)>>,
}

impl CrawlSink for Collect {
    fn page(&self, doc: Document) -> BoxFuture<'_, ()> {
        self.pages.lock().unwrap().push(doc);
        Box::pin(async {})
    }
    fn error<'a>(&'a self, url: &'a str, err: ApiError) -> BoxFuture<'a, ()> {
        panic!("{url}: {err:?}")
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
    fn resume(&self) -> Option<CrawlResume> {
        self.resume.lock().unwrap().take()
    }
    fn tracks_frontier(&self) -> bool {
        self.track
    }
    fn enqueued(&self, urls: Vec<(String, usize)>) -> BoxFuture<'_, ()> {
        self.enqueued.lock().unwrap().extend(urls);
        Box::pin(async {})
    }
}

fn request(base: &str, extra: serde_json::Value) -> CrawlRequest {
    let mut v = json!({
        "url": format!("{base}/kb/"),
        "sitemap": "skip",
        "maxConcurrency": 1,
        "scrapeOptions": { "formats": ["markdown"], "engine": "http" },
    });
    v.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(v).unwrap()
}

async fn crawl(base: &str, extra: serde_json::Value) -> Arc<Collect> {
    let sink = Arc::new(Collect::default());
    scraper()
        .crawl(&request(base, extra), sink.clone())
        .await
        .unwrap();
    sink
}

/// 按抓取顺序排列的标题。
fn order(c: &Collect) -> Vec<String> {
    c.pages
        .lock()
        .unwrap()
        .iter()
        .map(|d| d.metadata.title.clone().unwrap_or_default())
        .collect()
}

#[tokio::test]
async fn best_first_crawls_relevant_pages_first() {
    let base = site().await;

    let bfs = crawl(&base, json!({})).await;
    let bfs_order = order(&bfs);
    assert_eq!(bfs_order.len(), 14, "{bfs_order:?}");
    let pos = bfs_order.iter().position(|t| t == "Refund policy").unwrap();
    assert!(
        pos >= 6,
        "breadth-first reaches it after the other links: {bfs_order:?}"
    );
    assert!(
        bfs.summary.lock().unwrap().is_none(),
        "no summary without a query"
    );
    assert!(
        bfs.pages.lock().unwrap()[0]
            .metadata
            .pagesift
            .as_ref()
            .unwrap()
            .relevance
            .is_none()
    );

    let best = crawl(&base, json!({ "query": "refund policy" })).await;
    let best_order = order(&best);
    assert_eq!(
        best_order.len(),
        14,
        "same pages, different order: {best_order:?}"
    );
    assert_eq!(best_order[0], "Help center");
    assert_eq!(best_order[1], "Refund policy");
    let mut next: Vec<&str> = best_order[2..4].iter().map(String::as_str).collect();
    next.sort();
    assert_eq!(
        next,
        ["Refund exceptions", "Refund timeline"],
        "{best_order:?}"
    );

    let pages = best.pages.lock().unwrap();
    let rel = |title: &str| {
        pages
            .iter()
            .find(|d| d.metadata.title.as_deref() == Some(title))
            .and_then(|d| d.metadata.pagesift.as_ref()?.relevance)
            .unwrap()
    };
    assert!(
        rel("Refund policy") > rel("Team"),
        "relevance is reported per page"
    );
    assert!(
        pages.iter().all(|d| d.links.is_none()),
        "links not requested"
    );
    let summary = best.summary.lock().unwrap().clone().unwrap();
    assert_eq!(summary.stop_reason, "exhausted");
    assert_eq!((summary.pages, summary.coverage), (14, None));
}

#[tokio::test]
async fn best_first_respects_limit_and_reports_it() {
    let base = site().await;
    let c = crawl(&base, json!({ "query": ["refund", "policy"], "limit": 3 })).await;
    assert_eq!(
        order(&c),
        ["Help center", "Refund policy", order(&c)[2].as_str()]
    );
    assert!(order(&c)[2].starts_with("Refund"));
    assert_eq!(
        c.summary.lock().unwrap().clone().unwrap().stop_reason,
        "limit_reached"
    );
}

#[tokio::test]
async fn adaptive_crawl_stops_once_the_query_is_covered() {
    let base = site().await;
    let c = crawl(
        &base,
        json!({
            "query": "refund policy",
            "adaptive": { "evidencePerTerm": 3, "coverageThreshold": 1.0 },
        }),
    )
    .await;
    assert_eq!(order(&c), ["Help center", "Refund policy"]);
    let s = c.summary.lock().unwrap().clone().unwrap();
    assert_eq!(s.stop_reason, "coverage_reached");
    assert_eq!(s.coverage, Some(1.0));
    assert_eq!(s.pages, 2);
    assert!(
        s.term_evidence["refund"] >= 3 && s.term_evidence["policy"] >= 3,
        "{s:?}"
    );
}

#[tokio::test]
async fn adaptive_crawl_stops_when_new_pages_add_nothing() {
    let base = site().await;
    // "warranty" 在站点上从来没出现：覆盖度到不了。抓过几页以后 "refund" 每页都有、不再算证据，
    // 只命中它的页面也不再算相关，连续 3 页没有新信息就停
    let c = crawl(
        &base,
        json!({
            "query": "refund warranty",
            "adaptive": { "patience": 3, "evidencePerTerm": 100 },
        }),
    )
    .await;
    let o = order(&c);
    assert_eq!(o.len(), 5, "{o:?}");
    assert!(o[1..4].iter().all(|t| t.starts_with("Refund")), "{o:?}");
    let s = c.summary.lock().unwrap().clone().unwrap();
    assert_eq!(s.stop_reason, "saturated");
    assert!(s.coverage.unwrap() < 0.5);
    assert_eq!(s.term_evidence["warranty"], 0);
}

#[tokio::test]
async fn adaptive_needs_a_query_and_valid_options() {
    let base = site().await;
    let s = scraper();
    let req = |v: serde_json::Value| -> CrawlRequest {
        let mut r = json!({ "url": format!("{base}/kb/") });
        r.as_object_mut()
            .unwrap()
            .extend(v.as_object().unwrap().clone());
        serde_json::from_value(r).unwrap()
    };
    for bad in [
        json!({ "adaptive": true }),
        json!({ "query": "x", "adaptive": { "coverageThreshold": 0 } }),
        json!({ "query": "x", "adaptive": { "minGain": 2 } }),
    ] {
        let err = s
            .crawl(&req(bad.clone()), Arc::new(Collect::default()))
            .await
            .unwrap_err();
        assert_eq!(err.code.as_str(), "INVALID_REQUEST", "{bad}");
    }
}

#[tokio::test]
async fn focused_crawl_records_its_frontier_and_resumes_from_it() {
    let base = site().await;
    let req = request(&base, json!({ "query": "refund policy", "limit": 4 }));
    let sink = Arc::new(Collect {
        track: true,
        ..Default::default()
    });
    scraper().crawl(&req, sink.clone()).await.unwrap();
    let enqueued = sink.enqueued.lock().unwrap().clone();
    assert_eq!(enqueued[0], (format!("{base}/kb/"), 0));
    assert!(enqueued.contains(&(format!("{base}/kb/billing/refund-policy"), 1)));

    // 首页已经抓完，退款页还在队里：从断点继续时不再抓首页，limit 里扣掉已完成的一页
    let home = format!("{base}/kb/");
    let refund = format!("{base}/kb/billing/refund-policy");
    let about = format!("{base}/kb/about");
    let resumed = Arc::new(Collect {
        track: true,
        resume: Mutex::new(Some(CrawlResume {
            enqueued: vec![home.clone(), about.clone(), refund.clone()],
            pending: vec![(about, 1), (refund, 1)],
            done: 1,
        })),
        ..Default::default()
    });
    scraper().crawl(&req, resumed.clone()).await.unwrap();
    let o = order(&resumed);
    assert_eq!(o.len(), 3, "{o:?}");
    assert_eq!(o[0], "Refund policy", "re-scored by URL path: {o:?}");
    assert!(!o.contains(&"Help center".to_string()), "{o:?}");
    let e = resumed.enqueued.lock().unwrap().clone();
    assert!(
        !e.iter().any(|(u, _)| u == &home),
        "pending URLs are not re-reported"
    );
    assert_eq!(resumed.summary.lock().unwrap().clone().unwrap().pages, 4);
}
