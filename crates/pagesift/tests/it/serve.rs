//! HTTP 服务的重启恢复，端到端：真实的 Scraper 和存储爬一个本地站点，爬到一半丢掉整个运行时
//! （相当于进程被杀），用同一个数据目录重启后任务从断点继续。站点跑在单独的运行时里，不受“崩溃”影响。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::Path;
use axum::http::Request;
use axum::response::Html;
use axum::routing::get;
use http_body_util::BodyExt;
use pagesift::{Scraper, ScraperConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

const PAGES: usize = 40;

type Hits = Arc<Mutex<HashMap<usize, usize>>>;

/// 第 i 页链接到 2i+1、2i+2；每次请求慢一点，好在中途打断。
fn site(rt: &tokio::runtime::Runtime, hits: Hits) -> String {
    let app = Router::new().route(
        "/p/{i}",
        get(move |Path(i): Path<usize>| {
            let hits = hits.clone();
            async move {
                *hits.lock().unwrap().entry(i).or_default() += 1;
                tokio::time::sleep(Duration::from_millis(15)).await;
                let links: String = [2 * i + 1, 2 * i + 2]
                    .iter()
                    .filter(|c| **c < PAGES)
                    .map(|c| format!("<p>Next: <a href=\"/p/{c}\">part {c}</a></p>"))
                    .collect();
                Html(format!(
                    "<html><head><title>Part {i}</title></head><body><article><h1>Part {i}</h1>\
                     <p>Part {i} has enough text to be the main content of the page.</p>{links}</article></body></html>"
                ))
            }
        }),
    );
    let listener = rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    rt.spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn server(dir: &std::path::Path) -> Router {
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    cfg.policy.rate.per_host_concurrency = 4;
    let mut sc = pagesift_store::Config::new(dir);
    sc.background = false;
    pagesift::api::router(
        Arc::new(Scraper::new(cfg).unwrap()),
        pagesift::api::Config {
            store: Some(pagesift_store::Store::open(sc).unwrap()),
            ..Default::default()
        },
    )
}

async fn get_json(app: &Router, uri: &str) -> Value {
    let resp = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

/// 已完成页面的编号（按 sourceURL）。
async fn done_pages(app: &Router, id: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut skip = 0;
    loop {
        let v = get_json(app, &format!("/v2/crawl/{id}?skip={skip}")).await;
        let data = v["data"].as_array().unwrap();
        if data.is_empty() {
            return out;
        }
        for d in data {
            let src = d["metadata"]["sourceURL"].as_str().unwrap();
            out.push(src.rsplit('/').next().unwrap().parse().unwrap());
        }
        skip += data.len();
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn serve_resumes_a_crawl_after_a_restart() {
    let site_rt = runtime();
    let hits: Hits = Arc::default();
    let base = site(&site_rt, hits.clone());
    let dir = tempfile::tempdir().unwrap();

    let rt = runtime();
    let (id, before) = rt.block_on(async {
        let app = server(dir.path());
        let req = Request::post("/v2/crawl")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "url": format!("{base}/p/0"),
                    "limit": PAGES,
                    "sitemap": "skip",
                    "crawlEntireDomain": true,
                    "scrapeOptions": {"engine": "http", "formats": ["markdown"]},
                })
                .to_string(),
            ))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let v: Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let id = v["id"].as_str().unwrap().to_string();
        for i in 0.. {
            let s = get_json(&app, &format!("/v2/crawl/{id}")).await;
            if s["completed"].as_u64().unwrap() >= 10 {
                break;
            }
            assert!(i < 2000, "crawl made no progress: {s}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let before = done_pages(&app, &id).await;
        (id, before)
    });
    drop(rt);
    let before: HashSet<usize> = before.into_iter().collect();
    assert!(before.len() >= 10 && before.len() < PAGES, "{before:?}");

    let rt = runtime();
    rt.block_on(async {
        let app = server(dir.path());
        let mut status = Value::Null;
        for _ in 0..1000 {
            status = get_json(&app, &format!("/v2/crawl/{id}")).await;
            if status["status"] == "completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(status["status"], "completed", "{status}");
        assert_eq!(status["completed"], PAGES);
        let pages = done_pages(&app, &id).await;
        assert_eq!(pages.len(), PAGES, "no duplicates");
        assert_eq!(
            pages.iter().copied().collect::<HashSet<_>>(),
            (0..PAGES).collect(),
            "no page missing"
        );
    });
    drop(rt);

    let hits = hits.lock().unwrap();
    for i in &before {
        assert_eq!(
            hits[i], 1,
            "page {i} was finished before the restart and fetched again"
        );
    }
    let total: usize = hits.values().sum();
    // 中断时正在抓的页面（最多每主机并发数那么多）会重抓一次
    assert!(total <= PAGES + 4, "{total} fetches for {PAGES} pages");
}
