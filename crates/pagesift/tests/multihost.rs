//! 允许子域名的爬取：一个主机排了很多页时，其他主机不被它挡住（按主机轮流出队）。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::http::{HeaderMap, header};
use axum::response::Html;
use axum::routing::get;
use pagesift::api::types::{CrawlRequest, Document};
use pagesift::api::{ApiError, BoxFuture, CrawlSink};
use pagesift::{Scraper, ScraperConfig};
use serde_json::json;

#[derive(Default)]
struct Collect {
    pages: Mutex<Vec<(Instant, String)>>,
}

impl CrawlSink for Collect {
    fn page(&self, doc: Document) -> BoxFuture<'_, ()> {
        self.pages
            .lock()
            .unwrap()
            .push((Instant::now(), doc.metadata.url.unwrap_or_default()));
        Box::pin(async {})
    }
    fn error<'a>(&'a self, _: &'a str, _: ApiError) -> BoxFuture<'a, ()> {
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
async fn a_long_queue_on_one_host_does_not_block_the_others() {
    // `slow.localhost` 的首页链接 30 个慢页面；其他子域名各一个页面
    let app = Router::new().fallback(get(|headers: HeaderMap, uri: axum::http::Uri| async move {
        let host = headers[header::HOST].to_str().unwrap().split(':').next().unwrap().to_string();
        let port = headers[header::HOST].to_str().unwrap().split(':').nth(1).unwrap().to_string();
        if uri.path() == "/robots.txt" {
            return Html("User-agent: *\nAllow: /\n".to_string());
        }
        let mut links = String::new();
        if host == "root.localhost" && uri.path() == "/" {
            for i in 0..30 {
                links.push_str(&format!("<a href=\"/slow/{i}\">slow {i}</a> "));
            }
            for s in ["a", "b", "c", "d"] {
                links.push_str(&format!("<a href=\"http://{s}.root.localhost:{port}/\">{s}</a> "));
            }
        }
        if uri.path().starts_with("/slow/") {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Html(format!(
            "<html><body><article><h1>{host}{}</h1><p>Some words on {host} so there is a body.</p>{links}</article></body></html>",
            uri.path()
        ))
    }));
    let listener = tokio::net::TcpListener::bind("[::1]:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    cfg.policy.rate.per_host_concurrency = 1;
    let scraper = Scraper::new(cfg).unwrap();
    let req: CrawlRequest = serde_json::from_value(json!({
        "url": format!("http://root.localhost:{port}/"),
        "allowSubdomains": true,
        "limit": 100,
        "scrapeOptions": { "formats": ["markdown"], "engine": "http" }
    }))
    .unwrap();
    let sink = Arc::new(Collect::default());
    let t0 = Instant::now();
    scraper.crawl(&req, sink.clone()).await.unwrap();
    let pages = sink.pages.lock().unwrap();
    assert_eq!(
        pages.len(),
        35,
        "{:?}",
        pages.iter().map(|p| &p.1).collect::<Vec<_>>()
    );
    // 子域名的页面在慢页面排队期间就抓完了，不用等 30 个慢页面
    for s in ["a", "b", "c", "d"] {
        let (t, _) = pages
            .iter()
            .find(|(_, u)| u.contains(&format!("//{s}.root.localhost")))
            .unwrap();
        assert!(
            t.duration_since(t0) < Duration::from_millis(600),
            "{s} done after {:?}",
            t.duration_since(t0)
        );
    }
}
