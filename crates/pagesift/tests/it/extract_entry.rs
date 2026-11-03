//! 批次 E 的对外入口：CLI extract 的选择器记忆、语义字段缓存、LLM 兜底、`/v2/observe`、`/v2/similar`。
//! LLM 用录制好的回复（本机假服务，OpenAI 兼容接口）。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::response::Html;
use axum::routing::{get, post};
use pagesift::{Scraper, ScraperConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

/// 模型声明的输出上限：请求的 `max_tokens` 不许超过它（超了真服务回 400）。
const MODEL_MAX_OUT: u64 = 1024;

struct Llm {
    url: String,
    calls: Arc<AtomicUsize>,
    max_tokens: Arc<AtomicUsize>,
}

async fn listen(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

/// 按调用顺序给录制的回复；限额探测（`max_tokens` = 1）不计数、不消耗回复。
async fn llm(replies: Vec<Value>) -> Llm {
    let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
    let calls = Arc::new(AtomicUsize::new(0));
    let max_tokens = Arc::new(AtomicUsize::new(0));
    let (c, m) = (calls.clone(), max_tokens.clone());
    let app = Router::new()
        .route(
            "/v1/models/{m}",
            get(|| async {
                axum::Json(json!({"context_window": 8192, "max_completion_tokens": MODEL_MAX_OUT}))
            }),
        )
        .route(
            "/v1/chat/completions",
            post(move |body: String| {
                let v: Value = serde_json::from_str(&body).unwrap();
                let asked = v["max_tokens"].as_u64().unwrap_or(0) as usize;
                let probe = asked == 1;
                if !probe {
                    c.fetch_add(1, Ordering::SeqCst);
                    m.fetch_max(asked, Ordering::SeqCst);
                }
                let content = if probe {
                    "{}".to_string()
                } else {
                    replies
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or(json!({}))
                        .to_string()
                };
                async move {
                    axum::Json(json!({"choices": [{"message": {"role": "assistant", "content": content}, "finish_reason": "stop"}]}))
                }
            }),
        );
    Llm {
        url: format!("{}/v1", listen(app).await),
        calls,
        max_tokens,
    }
}

fn scraper(store: Option<&std::path::Path>, llm_url: Option<&str>) -> Arc<Scraper> {
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.env_proxy = false;
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    cfg.store = store.map(|d| pagesift_store::Store::open_dir(d).unwrap());
    cfg.llm = llm_url.map(|u| pagesift_llm::LlmConfig::new(u.to_string(), "fixture-model", None));
    Arc::new(Scraper::new(cfg).unwrap())
}

fn opts(format: Value) -> pagesift::api::types::ScrapeOptions {
    serde_json::from_value(json!({"engine": "http", "ignoreRobotsTxt": true, "maxAge": 0, "storeInCache": false, "formats": [format]})).unwrap()
}

fn warnings(d: &pagesift::api::types::Document) -> Vec<String> {
    d.metadata
        .pagesift
        .as_ref()
        .map(|m| m.warnings.clone())
        .unwrap_or_default()
}

const LIST: &str = "<html><body><main><ul>\
    <li class='product'><h3 class='name'>Alpha kit</h3></li>\
    <li class='product'><h3 class='name'>Beta kit</h3></li>\
    <li class='product'><h3 class='name'>Gamma kit</h3></li>\
    </ul></main></body></html>";

async fn site(body: &'static str) -> String {
    let app = Router::new().route("/shop", get(move || async move { Html(body) }));
    format!("{}/shop", listen(app).await)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_fields_call_the_model_once_within_its_output_limit() {
    let url = site(LIST).await;
    let m = llm(vec![json!({"selector": "h3", "type": "text"})]).await;
    let dir = tempfile::tempdir().unwrap();
    let s = scraper(Some(dir.path()), Some(&m.url));
    let semantic = opts(
        json!({"type": "extract", "schema": {"baseSelector": "li.product", "fields": [{"name": "title", "describe": "the product name"}]}}),
    );
    let first = s.scrape(&url, &semantic).await.unwrap();
    assert_eq!(first.extract.as_ref().unwrap()[1]["title"], "Beta kit");
    assert_eq!(m.calls.load(Ordering::SeqCst), 1);
    assert!(m.max_tokens.load(Ordering::SeqCst) as u64 <= MODEL_MAX_OUT);
    let second = s.scrape(&url, &semantic).await.unwrap();
    assert_eq!(second.extract, first.extract);
    assert_eq!(m.calls.load(Ordering::SeqCst), 1, "{:?}", warnings(&second));
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_base_selector_reaches_the_model_without_memory() {
    let url = site(LIST).await;
    let m = llm(vec![
        json!({"selector": "li.product"}),
        json!({"selector": "h3", "type": "text"}),
    ])
    .await;
    // 没有存储，也就没有记忆
    let s = scraper(None, Some(&m.url));
    let stale = opts(
        json!({"type": "extract", "schema": {"baseSelector": "div.card", "fields": [{"name": "title", "selector": "h2.t", "type": "text"}]}}),
    );
    let d = s.scrape(&url, &stale).await.unwrap();
    assert_eq!(d.extract.as_ref().unwrap()[0]["title"], "Alpha kit");
    let w = warnings(&d);
    assert!(
        w.contains(&"selector_healed: baseSelector via llm".to_string()),
        "{w:?}"
    );
    assert!(
        w.contains(&"selector_healed: fields.title via llm".to_string()),
        "{w:?}"
    );
    assert_eq!(m.calls.load(Ordering::SeqCst), 2);
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_selector_that_does_not_compile_is_not_applied() {
    let url = site(LIST).await;
    let m = llm(vec![json!({"selector": "((("})]).await;
    let s = scraper(None, Some(&m.url));
    let stale = opts(
        json!({"type": "extract", "schema": {"baseSelector": "div.card", "fields": [{"name": "title", "selector": "h3", "type": "text"}]}}),
    );
    let d = s.scrape(&url, &stale).await.unwrap();
    let w = warnings(&d);
    assert!(
        w.contains(&"extract_missing: baseSelector".to_string()),
        "{w:?}"
    );
    assert!(!w.iter().any(|x| x.starts_with("selector_healed")), "{w:?}");
    s.shutdown().await;
}

async fn post_json(app: &Router, path: &str, body: Value) -> (u16, Value) {
    let req = axum::http::Request::post(path)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_similar_and_observe() {
    let url = site(LIST).await;
    let m = llm(vec![json!({"ids": [2]})]).await;
    let s = scraper(None, Some(&m.url));
    let app = pagesift::api::router(s.clone(), Default::default());

    // 例子是卡片里的标题：往上一层才有结构相同的兄弟
    let (code, v) = post_json(
        &app,
        "/v2/similar",
        json!({"url": url, "selector": "li:first-child h3", "engine": "http", "bogus": 1}),
    )
    .await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["data"]["items"].as_array().unwrap().len(), 3, "{v}");
    assert_eq!(v["data"]["items"][2]["text"], "Gamma kit");
    assert_eq!(v["data"]["depth"], 1);
    assert_eq!(v["warnings"][0], "ignored_fields: bogus");
    let (code, _) = post_json(
        &app,
        "/v2/similar",
        json!({"url": url, "selector": "table.none", "engine": "http"}),
    )
    .await;
    assert_eq!(code, 400);

    let (code, v) = post_json(
        &app,
        "/v2/observe",
        json!({"url": url, "instruction": "the second product"}),
    )
    .await;
    assert_eq!(code, 200, "{v}");
    assert!(v["data"][0]["selector"].is_string(), "{v}");
    assert!(v["data"][0]["role"].is_string(), "{v}");
    assert!(
        v["warnings"][0]
            .as_str()
            .unwrap()
            .starts_with("llm_usage: 1 calls"),
        "{v}"
    );
    assert_eq!(m.calls.load(Ordering::SeqCst), 1);
    let (code, _) = post_json(&app, "/v2/observe", json!({"url": url, "instruction": " "})).await;
    assert_eq!(code, 400);
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observe_without_llm_is_unsupported() {
    let url = site(LIST).await;
    let s = scraper(None, None);
    let app = pagesift::api::router(s.clone(), Default::default());
    let (code, v) = post_json(&app, "/v2/observe", json!({"url": url, "instruction": "x"})).await;
    assert_ne!(code, 200, "{v}");
    s.shutdown().await;
}

/// CLI 进程之间选择器记忆也要保留：第一次记下，页面改版后第二次重定位。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_extract_keeps_selector_memory_between_runs() {
    let version = Arc::new(AtomicUsize::new(1));
    let v = version.clone();
    let app = Router::new().route(
        "/shop",
        get(move || {
            let redesigned = v.load(Ordering::SeqCst) != 1;
            async move { Html(if redesigned { redesign() } else { original() }) }
        }),
    );
    let url = format!("{}/shop", listen(app).await);
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        "[fetch]\nallow_private_network = true\nenv_proxy = false\n[policy]\ninterval_ms = 0\n",
    )
    .unwrap();
    let schema = dir.path().join("schema.json");
    std::fs::write(
        &schema,
        json!({"baseSelector": "li.product", "fields": [
            {"name": "name", "selector": "h3.name", "type": "text"},
            {"name": "price", "selector": "span.price", "type": "text"}]})
        .to_string(),
    )
    .unwrap();
    let run = || {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_pagesift"))
            .env("PAGESIFT_CONFIG", &config)
            .env("PAGESIFT_DATA_DIR", dir.path().join("data"))
            .args(["extract", "--schema"])
            .arg(&schema)
            .args(["--engine", "http", "--ignore-robots", &url])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()
    };
    let first = tokio::task::block_in_place(run);
    assert_eq!(first["extract"].as_array().unwrap().len(), 5, "{first}");
    version.store(2, Ordering::SeqCst);
    let second = tokio::task::block_in_place(run);
    assert_eq!(second["extract"].as_array().unwrap().len(), 5, "{second}");
    assert!(
        second["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().starts_with("relocated: baseSelector")),
        "{second}"
    );
}

const NAMES: [&str; 5] = [
    "Alpha widget",
    "Beta gadget",
    "Gamma tool",
    "Delta kit",
    "Epsilon set",
];

fn chrome(list: String) -> String {
    let nav: String = (0..12)
        .map(|i| format!("<a href='/c{i}'>Category {i}</a>"))
        .collect();
    let foot: String = (0..12)
        .map(|i| format!("<p>Footer note number {i} about shipping</p>"))
        .collect();
    format!(
        "<html><body><header><nav>{nav}</nav></header><main>{list}</main><footer>{foot}</footer></body></html>"
    )
}

fn original() -> String {
    let items: String = NAMES
        .iter()
        .enumerate()
        .map(|(i, n)| {
            format!(
                "<li class='product'><h3 class='name'>{n}</h3><span class='price'>${i}</span></li>"
            )
        })
        .collect();
    chrome(format!("<ul>{items}</ul>"))
}

fn redesign() -> String {
    let items: String = NAMES
        .iter()
        .enumerate()
        .map(|(i, n)| format!("<div class='css-1x8dfa'><h4 class='sc-a3b9'>{n}</h4><b class='sc-q9z2'>${i}</b></div>"))
        .collect();
    chrome(format!("<div class='grid'>{items}</div>"))
}
