//! 0.97：意图 → LLM 生成 schema → 缓存命中 → 改版后 locate 自愈 → locate 也找不回时 LLM 重新解析；
//! 只写 describe 的语义字段。LLM 用录制好的回复（本机假服务，OpenAI 兼容接口）。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::response::Html;
use axum::routing::{get, post};
use pagesift::{Scraper, ScraperConfig};
use serde_json::{Value, json};

fn page(v: usize) -> String {
    let nav: String = (0..10)
        .map(|i| format!("<a href='/c{i}'>Category {i}</a>"))
        .collect();
    let items: String = ["Alpha", "Beta", "Gamma"]
        .iter()
        .enumerate()
        .map(|(i, n)| match v {
            1 => format!("<li class='product'><h3 class='name'>{n} kit</h3><span class='price'>${i}0</span></li>"),
            2 => format!("<div class='css-1x8dfa'><h4>{n} kit</h4><b class='sc-q9z2'>${i}0</b></div>"),
            // 3：价格只在属性里、没有文字：指纹对不上，locate 找不回
            _ => format!("<div class='css-1x8dfa'><h4>{n} kit</h4><i data-cost='{i}0'></i></div>"),
        })
        .collect();
    let wrap = if v == 1 {
        format!("<ul>{items}</ul>")
    } else {
        format!("<section>{items}</section>")
    };
    format!(
        "<html><body><nav>{nav}</nav><main>{wrap}</main><footer><p>Footer one</p><p>Footer two</p></footer></body></html>"
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generate_cache_heal_and_resolve() {
    let version = Arc::new(AtomicUsize::new(1));
    let v = version.clone();
    let site = Router::new().route(
        "/shop",
        get(move || {
            let p = page(v.load(Ordering::SeqCst));
            async move { Html(p) }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/shop", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, site).await.unwrap() });

    // 录制的 LLM 回复，按调用顺序
    let replies: Arc<Mutex<VecDeque<Value>>> = Arc::new(Mutex::new(VecDeque::from([
        json!({"baseSelector": "li.product", "fields": [
            {"name": "name", "selector": "h3.name", "type": "text"},
            {"name": "price", "selector": "span.price", "type": "text"}]}),
        json!({"selector": "i", "type": "attribute", "attribute": "data-cost"}),
        json!({"selector": "h3", "type": "text"}),
    ])));
    let calls = Arc::new(AtomicUsize::new(0));
    let (r, c) = (replies.clone(), calls.clone());
    let llm = Router::new()
        .route("/v1/models/{m}", get(|| async { axum::Json(json!({"context_window": 8192, "max_completion_tokens": 1024})) }))
        .route("/v1/chat/completions", post(move || {
            c.fetch_add(1, Ordering::SeqCst);
            let content = r.lock().unwrap().pop_front().unwrap_or(json!({})).to_string();
            async move { axum::Json(json!({"choices": [{"message": {"role": "assistant", "content": content}, "finish_reason": "stop"}]})) }
        }));
    let l2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let llm_url = format!("http://{}/v1", l2.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l2, llm).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.env_proxy = false;
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    cfg.store = Some(pagesift_store::Store::open_dir(dir.path()).unwrap());
    cfg.llm = Some(pagesift_llm::LlmConfig::new(llm_url, "fixture-model", None));
    let s = Scraper::new(cfg).unwrap();
    let opts = |fmt: Value| -> pagesift::api::types::ScrapeOptions {
        serde_json::from_value(json!({"engine": "http", "ignoreRobotsTxt": true, "maxAge": 0, "storeInCache": false, "formats": [fmt]})).unwrap()
    };
    let intent = opts(json!({"type": "extract", "intent": "product names and prices"}));
    let warnings = |d: &pagesift::api::types::Document| {
        d.metadata
            .pagesift
            .as_ref()
            .map(|m| m.warnings.clone())
            .unwrap_or_default()
    };

    // ① 生成：一次 LLM 调用，验证通过，写进缓存
    let d = s.scrape(&url, &intent).await.unwrap();
    assert_eq!(
        d.extract.as_ref().unwrap()[2],
        json!({"name": "Gamma kit", "price": "$20"})
    );
    assert!(
        warnings(&d).contains(&"extract_schema: generated".to_string()),
        "{:?}",
        warnings(&d)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // ② 重复运行：缓存命中，不调 LLM
    let d = s.scrape(&url, &intent).await.unwrap();
    assert!(warnings(&d).contains(&"extract_schema: cached".to_string()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // ③ 改版：locate 自愈，不调 LLM
    version.store(2, Ordering::SeqCst);
    let d = s.scrape(&url, &intent).await.unwrap();
    assert_eq!(
        d.extract.as_ref().unwrap().as_array().unwrap().len(),
        3,
        "{:?}",
        warnings(&d)
    );
    assert!(
        warnings(&d)
            .iter()
            .any(|w| w.starts_with("relocated: baseSelector")),
        "{:?}",
        warnings(&d)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // ④ 再改版：价格的指纹对不上，LLM 重新解析一次
    version.store(3, Ordering::SeqCst);
    let d = s.scrape(&url, &intent).await.unwrap();
    assert!(
        warnings(&d).contains(&"selector_healed: fields.price via llm".to_string()),
        "{:?}",
        warnings(&d)
    );
    assert_eq!(d.extract.as_ref().unwrap()[1]["price"], "10");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // ⑤ 语义字段：只写 describe，第一次用时由 LLM 解析
    version.store(1, Ordering::SeqCst);
    let semantic = opts(
        json!({"type": "extract", "schema": {"baseSelector": "li.product", "fields": [{"name": "title", "describe": "the product name"}]}}),
    );
    let d = s.scrape(&url, &semantic).await.unwrap();
    assert_eq!(
        d.extract.as_ref().unwrap()[0],
        json!({"title": "Alpha kit"})
    );
    assert!(warnings(&d).contains(&"selector_resolved: fields.title via llm".to_string()));
    s.shutdown().await;
}
