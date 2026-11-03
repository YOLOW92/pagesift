//! 不变量：HTTP 抓到的一页只建一棵 DOM，提取和 `engine: auto` 的渲染判断都读这一棵。
//! 计数器是进程级的，这个测试文件只放这一个测试，免得别的测试同时建树。

use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::Router;
use axum::response::Html;
use axum::routing::get;
use pagesift::api::types::ScrapeOptions;
use pagesift::{Scraper, ScraperConfig};
use pagesift_html::DOM_BUILDS;

const ARTICLE: &str = r#"<html><head><title>Long read</title><meta property="og:type" content="article"></head>
<body><nav><a href="/">Home</a></nav><article><h1>Long read</h1>
<p>A paragraph that is long enough to be the body of a real article about something worth reading today.</p>
<p>Another paragraph continues the story with more detail so the extracted text is clearly the main content.</p>
</article></body></html>"#;

/// 正文很少、带脚本：`auto` 会走"要不要渲染"的判断（没有浏览器，照常返回 HTTP 结果）。
const THIN_APP: &str = r#"<html><head><title>App</title></head>
<body><div id="root">Loading</div><script src="/app.js"></script><button onclick="go()">Go</button></body></html>"#;

#[tokio::test]
async fn each_http_page_builds_exactly_one_dom() {
    let app = Router::new()
        .route("/article", get(|| async { Html(ARTICLE) }))
        .route("/app", get(|| async { Html(THIN_APP) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::ZERO;
    // 只验证 HTTP 路径：内核链只有一个连不上的远程浏览器，`auto` 判断要渲染时照常返回 HTTP 结果。
    // 浏览器快照另算（每次快照一棵），不在这里数。
    cfg.browser.source = pagesift_cdp::BrowserSource::Remote("http://127.0.0.1:9".into());
    let scraper = Scraper::new(cfg).unwrap();
    for (path, engine) in [("article", "http"), ("article", "auto"), ("app", "auto")] {
        let opts: ScrapeOptions = serde_json::from_value(
            serde_json::json!({ "engine": engine, "formats": ["markdown", "links"] }),
        )
        .unwrap();
        let before = DOM_BUILDS.load(Ordering::SeqCst);
        let url = format!("http://{addr}/{path}");
        let doc = scraper.scrape(&url, &opts).await.unwrap();
        let built = DOM_BUILDS.load(Ordering::SeqCst) - before;
        assert_eq!(built, 1, "{path} with engine {engine} built {built} DOMs");
        assert!(doc.markdown.is_some());
    }
}
