//! `extract` 格式端到端：按 CSS schema 抽取；配了存储时页面改版后自动重定位、写回新选择器。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::response::Html;
use axum::routing::get;
use pagesift::{Scraper, ScraperConfig};
use serde_json::json;

const NAMES: [&str; 5] = [
    "Alpha widget",
    "Beta gadget",
    "Gamma tool",
    "Delta kit",
    "Epsilon set",
];

/// 一个像样的商品列表页：导航、列表、页脚；`v2` 是改版后的（类名哈希化、换标签、换包裹）。
fn page(v2: bool) -> String {
    let nav: String = (0..12)
        .map(|i| format!("<a href='/c{i}'>Category {i}</a>"))
        .collect();
    let foot: String = (0..12)
        .map(|i| format!("<p>Footer note number {i} about shipping</p>"))
        .collect();
    let items: String = NAMES
        .iter()
        .enumerate()
        .map(|(i, n)| {
            if v2 {
                format!("<div class='css-1x8dfa'><h4 class='sc-a3b9'>{n}</h4><b class='sc-q9z2'>${i}</b></div>")
            } else {
                format!("<li class='product'><h3 class='name'>{n}</h3><span class='price'>${i}</span></li>")
            }
        })
        .collect();
    let list = if v2 {
        format!("<div class='grid'>{items}</div>")
    } else {
        format!("<ul>{items}</ul>")
    };
    format!(
        "<html><head><title>Shop</title></head><body><header><nav>{nav}</nav></header>\
         <main>{list}</main><footer>{foot}</footer></body></html>"
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extract_relocates_after_redesign_and_remembers() {
    let version = Arc::new(AtomicUsize::new(1));
    let v = version.clone();
    let app = Router::new().route(
        "/shop",
        get(move || {
            let body = page(v.load(Ordering::SeqCst) != 1);
            async move { Html(body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/shop", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.env_proxy = false;
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    cfg.store = Some(pagesift_store::Store::open_dir(dir.path()).unwrap());
    let s = Scraper::new(cfg).unwrap();
    let opts: pagesift::api::types::ScrapeOptions = serde_json::from_value(json!({
        "engine": "http", "ignoreRobotsTxt": true, "maxAge": 0, "storeInCache": false,
        "formats": [{"type": "extract", "schema": {
            "baseSelector": "li.product",
            "fields": [
                {"name": "name", "selector": "h3.name", "type": "text"},
                {"name": "price", "selector": "span.price", "type": "text"}
            ]
        }}]
    }))
    .unwrap();

    let first = s.scrape(&url, &opts).await.unwrap();
    assert_eq!(first.extract.as_ref().unwrap().as_array().unwrap().len(), 5);
    assert!(first.markdown.is_none() && first.raw_html.is_none());

    version.store(2, Ordering::SeqCst);
    let second = s.scrape(&url, &opts).await.unwrap();
    let items = second.extract.unwrap();
    let want: Vec<serde_json::Value> = NAMES
        .iter()
        .enumerate()
        .map(|(i, n)| json!({"name": n, "price": format!("${i}")}))
        .collect();
    assert_eq!(items, serde_json::Value::Array(want));
    let warnings = second.metadata.pagesift.unwrap().warnings;
    assert!(
        warnings
            .iter()
            .any(|w| w.starts_with("relocated: baseSelector li.product -> ")),
        "{warnings:?}"
    );

    // 不合法的 schema 在抓取前就报 INVALID_REQUEST
    let bad: pagesift::api::types::ScrapeOptions = serde_json::from_value(json!({
        "engine": "http", "formats": [{"type": "extract", "schema": {"fields": []}}]
    }))
    .unwrap();
    let err = s.scrape(&url, &bad).await.unwrap_err();
    assert_eq!(err.code, pagesift::api::ErrorCode::InvalidRequest);
    s.shutdown().await;
}
