//! 面向 AI 应用的单页能力的端到端测试：按查询过滤正文（`queryFilter`）和变化检测（`changeTracking`）。
//! 本地起一个内容可以改的页面；json 模式用一个假的 OpenAI 兼容接口。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use pagesift::api::Engine;
use pagesift::api::types::ScrapeOptions;
use pagesift::{Scraper, ScraperConfig};
use serde_json::{Value, json};

const ARTICLE: &str = "<h1>Store help</h1>\
<p>This page collects answers to the questions customers ask us most often about orders.</p>\
<h2>Shipping</h2><p>Orders leave our warehouse within two days and arrive within a week.</p>\
<h2>Returns</h2><p>You can ask for a refund within 30 days; the refund window starts at delivery.</p>\
<h3>Exceptions</h3><p>Gift cards never get a refund.</p>\
<h2>Careers</h2><p>We are hiring engineers and designers in three offices.</p>";

/// 页面状态：(状态码, article 里的 HTML)。
type PageState = Arc<Mutex<(u16, String)>>;

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn site(state: PageState) -> String {
    let app = Router::new().route(
        "/p",
        get(move || {
            let (status, body) = state.lock().unwrap().clone();
            async move {
                (
                    StatusCode::from_u16(status).unwrap(),
                    Html(format!(
                        "<html><head><title>Help</title></head><body><article>{body}</article></body></html>"
                    )),
                )
                    .into_response()
            }
        }),
    );
    serve(app).await
}

/// 假的 LLM：正文里有 "Price: N" 就回答 `{"price": N}`。
async fn llm() -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|body: String| async move {
            let req: Value = serde_json::from_str(&body).unwrap();
            let prompt = req["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|m| m["content"].as_str())
                .collect::<String>();
            let price: u64 = prompt
                .split("Price: ")
                .nth(1)
                .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            let content = json!({ "price": price }).to_string();
            axum::Json(json!({
                "choices": [{ "message": { "role": "assistant", "content": content } }]
            }))
        }),
    );
    format!("{}/v1", serve(app).await)
}

fn scraper(store: Option<&tempfile::TempDir>, llm: Option<String>) -> Scraper {
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    cfg.store = store.map(|d| pagesift_store::Store::open_dir(d.path()).unwrap());
    cfg.llm = llm.map(|base| {
        let mut c = pagesift_llm::LlmConfig::new(base, "m", None);
        c.retry_backoff = Duration::from_millis(5);
        c
    });
    Scraper::new(cfg).unwrap()
}

fn opts(extra: Value) -> ScrapeOptions {
    let mut v = json!({ "engine": "http", "ignoreRobotsTxt": true });
    v.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(v).unwrap()
}

#[tokio::test]
async fn chunking_returns_chunks_with_heading_paths() {
    let base = site(Arc::new(Mutex::new((200, ARTICLE.into())))).await;
    let url = format!("{base}/p");
    let s = scraper(None, None);
    let doc = s
        .scrape(
            &url,
            &opts(json!({ "chunking": { "type": "recursive", "size": 20 } })),
        )
        .await
        .unwrap();
    let md = doc.markdown.unwrap();
    let chunks = doc.chunks.unwrap();
    assert!(chunks.len() >= 3, "{chunks:?}");
    for c in &chunks {
        assert_eq!(&md[c.start..c.end], c.text);
        assert!(c.tokens <= 20, "{c:?}");
    }
    let refund = chunks
        .iter()
        .find(|c| c.text.contains("refund window"))
        .unwrap();
    assert_eq!(refund.heading_path, ["Store help", "Returns"]);
    let headings = s
        .scrape(
            &url,
            &opts(json!({ "chunking": { "type": "headings", "maxLevel": 2 } })),
        )
        .await
        .unwrap()
        .chunks
        .unwrap();
    assert!(
        headings
            .iter()
            .any(|c| c.text.starts_with("## Returns") && c.text.contains("Gift cards"))
    );
    let bad = s
        .scrape(
            &url,
            &opts(json!({ "chunking": { "type": "recursive", "size": 0 } })),
        )
        .await
        .unwrap_err();
    assert_eq!(bad.code.as_str(), "INVALID_REQUEST");
}

#[tokio::test]
async fn query_filter_keeps_relevant_sections_only() {
    let base = site(Arc::new(Mutex::new((200, ARTICLE.into())))).await;
    let url = format!("{base}/p");
    let s = scraper(None, None);

    let full = s.scrape(&url, &opts(json!({}))).await.unwrap();
    let full_md = full.markdown.unwrap();
    assert!(full_md.contains("hiring engineers"));
    assert!(full.metadata.pagesift.unwrap().query_filter.is_none());

    let doc = s
        .scrape(&url, &opts(json!({ "queryFilter": "refund window" })))
        .await
        .unwrap();
    let md = doc.markdown.unwrap();
    assert!(md.contains("## Returns"), "{md}");
    assert!(md.contains("refund window starts at delivery"), "{md}");
    assert!(!md.contains("hiring engineers"), "{md}");
    assert!(!md.contains("warehouse"), "{md}");
    assert!(md.len() < full_md.len() / 2, "{md}");
    let stats = doc.metadata.pagesift.unwrap().query_filter.unwrap();
    assert!(stats.kept_blocks >= 1 && stats.kept_blocks < stats.total_blocks);
    assert!(stats.tokens_after < stats.tokens_before);

    // 标题命中整节保留，并带上上级标题
    let doc = s
        .scrape(
            &url,
            &opts(json!({ "queryFilter": { "query": "exceptions", "threshold": 0.1 } })),
        )
        .await
        .unwrap();
    let md = doc.markdown.unwrap();
    assert_eq!(
        md,
        "# Store help\n\n## Returns\n\n### Exceptions\n\nGift cards never get a refund.\n"
    );

    let none = s
        .scrape(&url, &opts(json!({ "queryFilter": "kubernetes" })))
        .await
        .unwrap();
    assert_eq!(none.markdown.as_deref(), Some(""));
    assert!(
        none.metadata
            .pagesift
            .unwrap()
            .warnings
            .contains(&"query_filter_no_match".to_string())
    );
}

#[tokio::test]
async fn change_tracking_reports_new_same_changed_and_removed() {
    let state: PageState = Arc::new(Mutex::new((
        200,
        "<h1>Plan</h1><p>The pro plan costs ten dollars a month and includes support.</p>".into(),
    )));
    let base = site(state.clone()).await;
    let url = format!("{base}/p");
    let dir = tempfile::tempdir().unwrap();
    let s = scraper(Some(&dir), None);
    assert!(Engine::supported_formats(&s).contains(&"changeTracking"));
    let o = opts(json!({
        "formats": ["markdown", { "type": "changeTracking", "modes": ["git-diff"] }],
    }));

    let first = s.scrape(&url, &o).await.unwrap();
    let meta = serde_json::to_value(&first.metadata).unwrap();
    for name in ["contentHash", "rawHash", "textHash"] {
        let hash = meta["pagesift"][name]
            .as_str()
            .expect("tracking fingerprint");
        assert_eq!(hash.len(), 64);
        assert!(hash.bytes().all(|c| c.is_ascii_hexdigit()));
    }
    let ct = first.change_tracking.unwrap();
    assert_eq!(
        (ct.change_status.as_str(), ct.visibility.as_str()),
        ("new", "visible")
    );
    assert_eq!(ct.previous_scrape_at, None);
    assert!(first.markdown.is_some());

    // 缓存开着，但变化检测总是重新抓
    let same = s.scrape(&url, &o).await.unwrap();
    assert_eq!(same.metadata.cache_state.as_deref(), Some("miss"));
    let ct = same.change_tracking.unwrap();
    assert_eq!(ct.change_status, "same");
    assert!(ct.previous_scrape_at.unwrap().ends_with('Z'));
    assert!(ct.diff.is_none());

    state.lock().unwrap().1 =
        "<h1>Plan</h1><p>The pro plan costs twelve dollars a month and includes support.</p>"
            .into();
    let changed = s.scrape(&url, &o).await.unwrap();
    let v = serde_json::to_value(&changed).unwrap();
    let ct = &v["changeTracking"];
    assert_eq!(ct["changeStatus"], "changed");
    let text = ct["diff"]["text"].as_str().unwrap();
    assert!(text.starts_with("@@ -"), "{text}");
    assert!(text.contains("\n-The pro plan costs ten dollars"), "{text}");
    assert!(
        text.contains("\n+The pro plan costs twelve dollars"),
        "{text}"
    );
    let changes = &ct["diff"]["json"]["files"][0]["chunks"][0]["changes"];
    assert!(
        changes
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["type"] == "add")
    );
    assert_eq!(ct["pagesift"]["substantive"], true);

    // 只要 changeTracking 不要 markdown 时照样能比较，输出里不带 markdown
    let only = s
        .scrape(&url, &opts(json!({ "formats": ["changeTracking"] })))
        .await
        .unwrap();
    assert!(only.markdown.is_none());
    assert_eq!(only.change_tracking.unwrap().change_status, "same");

    state.lock().unwrap().0 = 404;
    let removed = s.scrape(&url, &o).await.unwrap();
    assert_eq!(removed.change_tracking.unwrap().change_status, "removed");

    // 不同 tag 各有各的历史
    state.lock().unwrap().0 = 200;
    let tagged = s
        .scrape(
            &url,
            &opts(json!({ "formats": [{ "type": "changeTracking", "tag": "weekly" }] })),
        )
        .await
        .unwrap();
    assert_eq!(tagged.change_tracking.unwrap().change_status, "new");
}

#[tokio::test]
async fn change_tracking_json_mode_compares_extracted_fields() {
    let state: PageState = Arc::new(Mutex::new((
        200,
        "<h1>Widget</h1><p>The widget is a small useful tool for the kitchen. Price: 10 dollars.</p>"
            .into(),
    )));
    let base = site(state.clone()).await;
    let url = format!("{base}/p");
    let dir = tempfile::tempdir().unwrap();
    let s = scraper(Some(&dir), Some(llm().await));
    let o = opts(json!({
        "formats": [{
            "type": "changeTracking",
            "modes": ["json"],
            "schema": { "type": "object", "properties": { "price": { "type": "number" } } },
        }],
    }));
    let first = s.scrape(&url, &o).await.unwrap().change_tracking.unwrap();
    assert_eq!(first.change_status, "new");
    assert!(first.json.is_none());
    state.lock().unwrap().1 =
        "<h1>Widget</h1><p>The widget is a small useful tool for the kitchen. Price: 12 dollars.</p>"
            .into();
    let ct = s.scrape(&url, &o).await.unwrap().change_tracking.unwrap();
    assert_eq!(ct.change_status, "changed");
    assert_eq!(
        ct.json.unwrap(),
        json!({ "price": { "previous": 10, "current": 12 } })
    );
}

#[tokio::test]
async fn change_tracking_needs_a_store_and_json_mode_needs_an_llm() {
    let base = site(Arc::new(Mutex::new((200, ARTICLE.into())))).await;
    let url = format!("{base}/p");
    let no_store = scraper(None, None);
    assert!(!Engine::supported_formats(&no_store).contains(&"changeTracking"));
    let err = no_store
        .scrape(&url, &opts(json!({ "formats": ["changeTracking"] })))
        .await
        .unwrap_err();
    assert_eq!(err.code.as_str(), "UNSUPPORTED_OPTION");

    let dir = tempfile::tempdir().unwrap();
    let no_llm = scraper(Some(&dir), None);
    let err = no_llm
        .scrape(
            &url,
            &opts(json!({ "formats": [{ "type": "changeTracking", "modes": ["json"] }] })),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code.as_str(), "UNSUPPORTED_OPTION");
    let err = no_llm
        .scrape(
            &url,
            &opts(json!({ "formats": [{ "type": "changeTracking", "modes": ["diff"] }] })),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code.as_str(), "INVALID_REQUEST");
}
