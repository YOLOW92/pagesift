//! 整站爬取在"真实网站毛病"上的表现：跟踪参数和会话 ID 不产生重复抓取，二进制文件不抓，
//! 无穷日历这类陷阱排到正常页面之后，偶发的 503 重试成功，冷却期间排队的页面不直接判失败。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use pagesift::api::types::{CrawlRequest, Document};
use pagesift::api::{ApiError, BoxFuture, CrawlSink};
use pagesift::{Scraper, ScraperConfig};
use serde_json::json;

const STORIES: usize = 12;

#[derive(Default)]
struct Origin {
    hits: Mutex<Vec<String>>,
    flaky_left: AtomicUsize,
}

fn story(n: usize) -> String {
    let topics = [
        "rivers",
        "glaciers",
        "deserts",
        "forests",
        "volcanoes",
        "reefs",
        "tundra",
        "wetlands",
        "canyons",
        "islands",
        "prairies",
        "caves",
    ];
    let mut links = String::new();
    for k in [2 * n + 1, 2 * n + 2] {
        if k < STORIES {
            links.push_str(&format!(
                "<li><a href=\"/story/{k};jsessionid=S{n}X{k}ABCDEF?utm_source=feed&utm_medium=web\">Story {k}</a></li>"
            ));
        }
    }
    format!(
        "<html><head><title>Story {n}</title></head><body><article><h1>Story {n}</h1>\
         <p>This story is about {} and explains at length how they form, change and disappear over time.</p>\
         <ul>{links}</ul><p><a href=\"/events?month=2026-01\">Calendar</a> <a href=\"/media/{n}.jpg\">Photo</a> \
         <a href=\"/story/{n}?print=1\">Print</a></p></article></body></html>",
        topics[n]
    )
}

async fn handle(State(o): State<Arc<Origin>>, uri: Uri) -> Response {
    o.hits.lock().unwrap().push(uri.to_string());
    StatusCode::NOT_FOUND.into_response()
}

async fn origin() -> (String, Arc<Origin>) {
    let o = Arc::new(Origin {
        flaky_left: AtomicUsize::new(1),
        ..Default::default()
    });
    let log = |o: &Arc<Origin>, uri: &Uri| o.hits.lock().unwrap().push(uri.to_string());
    let app = Router::new()
        .route("/robots.txt", get(|| async { "User-agent: *\nAllow: /\n" }))
        .route(
            "/",
            get(move |State(o): State<Arc<Origin>>, uri: Uri| async move {
                log(&o, &uri);
                Html(story(0))
            }),
        )
        .route(
            "/story/{n}",
            get(move |State(o): State<Arc<Origin>>, uri: Uri, Path(n): Path<String>| async move {
                log(&o, &uri);
                let n: usize = n.split(';').next().unwrap().parse().unwrap();
                // 第 5 篇第一次请求返回 503（没有 Retry-After），重试就好
                if n == 5 && o.flaky_left.fetch_sub(1, Ordering::SeqCst) == 1 {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                Html(story(n)).into_response()
            }),
        )
        .route(
            "/events",
            get(move |State(o): State<Arc<Origin>>, uri: Uri, Query(q): Query<HashMap<String, String>>| async move {
                log(&o, &uri);
                let m = q.get("month").cloned().unwrap_or_default();
                let (y, mm) = m.split_once('-').unwrap();
                let t = y.parse::<i32>().unwrap() * 12 + mm.parse::<i32>().unwrap();
                let next = format!("{}-{:02}", t / 12, t % 12 + 1);
                Html(format!(
                    "<html><body><h1>Events in {m}</h1><p>No events scheduled.</p><a href=\"/events?month={next}\">Next month</a></body></html>"
                ))
            }),
        )
        .fallback(handle)
        .with_state(o.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), o)
}

#[derive(Default)]
struct Collect {
    pages: Mutex<Vec<Document>>,
    errors: Mutex<Vec<(String, String)>>,
}

impl CrawlSink for Collect {
    fn page(&self, doc: Document) -> BoxFuture<'_, ()> {
        self.pages.lock().unwrap().push(doc);
        Box::pin(async {})
    }
    fn error<'a>(&'a self, url: &'a str, err: ApiError) -> BoxFuture<'a, ()> {
        self.errors
            .lock()
            .unwrap()
            .push((url.to_string(), err.code.as_str().to_string()));
        Box::pin(async {})
    }
    fn total(&self, _: usize) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn cancelled(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { false })
    }
}

#[tokio::test]
async fn traps_and_noise_do_not_eat_the_budget() {
    let (base, o) = origin().await;
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    cfg.policy.rate.per_host_concurrency = 1;
    let scraper = Scraper::new(cfg).unwrap();
    // 上限比内容页多一点：陷阱降级后，正常页面先抓完，剩下的名额才给日历
    let req: CrawlRequest = serde_json::from_value(json!({
        "url": format!("{base}/"),
        "limit": STORIES + 8,
        "scrapeOptions": { "formats": ["markdown"], "engine": "http" }
    }))
    .unwrap();
    let sink = Arc::new(Collect::default());
    scraper.crawl(&req, sink.clone()).await.unwrap();

    let hits = o.hits.lock().unwrap().clone();
    assert!(
        hits.iter()
            .all(|h| !h.contains("utm_") && !h.contains("jsessionid")),
        "tracking and session parameters are stripped before fetching: {hits:?}"
    );
    assert!(
        !hits.iter().any(|h| h.starts_with("/media/")),
        "binary files are not fetched: {hits:?}"
    );

    let pages = sink.pages.lock().unwrap();
    let mut stories: Vec<String> = pages
        .iter()
        .filter(|d| d.metadata.status_code == 200)
        .filter_map(|d| d.metadata.title.clone())
        .filter(|t| t.starts_with("Story"))
        .collect();
    stories.sort();
    stories.dedup();
    assert_eq!(
        stories.len(),
        STORIES,
        "every story is crawled, including the flaky one: {stories:?}"
    );

    // 所有故事都排在第 4 个之后的日历页之前：日历在抓了几页重复内容后降级
    let order: Vec<&str> = hits.iter().map(String::as_str).collect();
    let last_story = order
        .iter()
        .rposition(|h| h.starts_with("/story/") && !h.contains("print"))
        .unwrap();
    let calendars_before = order[..last_story]
        .iter()
        .filter(|h| h.starts_with("/events"))
        .count();
    assert!(
        calendars_before <= 5,
        "calendar pages fetched before the last story: {calendars_before}\n{order:?}"
    );

    // 打印版和原文全文相同：标出原文
    let dup = pages
        .iter()
        .find(|d| {
            d.metadata
                .url
                .as_deref()
                .is_some_and(|u| u.contains("print=1"))
        })
        .and_then(|d| d.metadata.pagesift.as_ref()?.duplicate_of.clone());
    if let Some(first) = dup {
        assert!(first.contains("/story/") || first.ends_with('/'), "{first}");
    }
    assert!(
        sink.errors.lock().unwrap().is_empty(),
        "{:?}",
        sink.errors.lock().unwrap()
    );
}
