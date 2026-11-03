//! `exportImages`：网页图片走同一条礼貌抓取路径（robots、SSRF 放行本机），按内容哈希去重；
//! 不打开时不下载任何图片。只用本机的测试站点，不联网。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use pagesift::api::types::ScrapeOptions;
use pagesift::{Scraper, ScraperConfig};
use serde_json::json;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR-test-image";

async fn site(hits: Arc<AtomicUsize>) -> String {
    let png = move || {
        let hits = hits.clone();
        async move {
            hits.fetch_add(1, Ordering::Relaxed);
            ([("content-type", "image/png")], PNG).into_response()
        }
    };
    let app = Router::new()
        .route(
            "/page",
            get(|| async {
                Html(
                    r#"<html><head><title>Pictures</title></head><body><article><h1>Pictures</h1>
                    <p>A page with a few pictures and enough words to count as the main content of it.</p>
                    <img src="/a.png" alt="a"><img src="/b.png" alt="b"><img src="/private/c.png" alt="c">
                    <p>More text after the pictures so the article is long enough to be kept as content.</p>
                    </article></body></html>"#,
                )
            }),
        )
        .route("/robots.txt", get(|| async { "User-agent: *\nDisallow: /private/\n" }))
        .route("/a.png", get(png.clone()))
        .route("/b.png", get(png.clone()))
        .route("/private/c.png", get(png));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn scraper() -> Scraper {
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.env_proxy = false; // 不让机器上的代理改道本地测试
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::ZERO;
    Scraper::new(cfg).unwrap()
}

#[tokio::test]
async fn web_images_are_exported_politely_and_deduplicated() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = site(hits.clone()).await;
    let s = scraper();
    let url = format!("{base}/page");

    // 默认不导出：一张图片都不下载
    let plain: ScrapeOptions = serde_json::from_value(json!({ "engine": "http" })).unwrap();
    let doc = s.scrape(&url, &plain).await.unwrap();
    assert!(doc.attachments.is_empty());
    assert_eq!(hits.load(Ordering::Relaxed), 0);

    let export: ScrapeOptions =
        serde_json::from_value(json!({ "engine": "http", "exportImages": true })).unwrap();
    let doc = s.scrape(&url, &export).await.unwrap();
    // a.png 和 b.png 内容相同，只留一份；/private/ 被 robots.txt 禁止，不下载
    assert_eq!(doc.attachments.len(), 1, "{:?}", doc.attachments);
    let a = &doc.attachments[0];
    assert_eq!(a.format, "png");
    assert_eq!(a.data.as_deref(), Some(PNG));
    assert_eq!(a.source.as_deref(), Some(format!("{base}/a.png").as_str()));
    assert_eq!(
        hits.load(Ordering::Relaxed),
        2,
        "robots-disallowed image was fetched"
    );
    assert!(doc.images.is_none(), "images format was not requested");
    let warnings = &doc.metadata.pagesift.as_ref().unwrap().warnings;
    assert!(
        warnings
            .iter()
            .any(|w| w.starts_with("images_not_exported: 1 of 3")),
        "{warnings:?}"
    );
    // JSON 里只有元数据
    let v = serde_json::to_value(&doc).unwrap();
    assert!(v["attachments"][0].get("data").is_none());
    assert_eq!(v["attachments"][0]["id"], a.id.as_str());
}
