//! 0.97 的“解析一次 → 缓存”：同一类页面、同一句话重复运行不再问模型；页面变了先走 locate，
//! 再不行才问模型。真实 Chrome + 本地站点（没有浏览器时跳过），LLM 用录制回复，数的是实际收到的调用次数。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::get;
use pagesift::{Scraper, ScraperConfig};
use serde_json::{Value, json};

use super::interact::{available, call, fake_llm};

fn nav() -> String {
    (0..12)
        .map(|i| format!("<a href='/c{i}'>Category number {i}</a>"))
        .collect()
}

const HANDLER: &str = "const p = document.createElement('p'); p.textContent = 'Clicked: ' + this.textContent; document.querySelector('main').appendChild(p);";

/// 同一家店的几个版本。
fn page(version: usize) -> String {
    let body = match version {
        1 => format!("<button id='buy' class='cta primary' onclick=\"{HANDLER}\">Buy now</button>"),
        // 改版：换了 id，多包一层，文字和类名没变。locate 找得回来
        2 => format!(
            "<div class='row'><button id='purchase-77' class='cta primary' onclick=\"{HANDLER}\">Buy now</button></div>"
        ),
        // 同一个选择器现在是另一个东西：不能照点，要问模型
        _ => format!(
            "<button id='purchase-77' class='cta primary' onclick=\"{HANDLER}\">Delete my account</button>\
             <a id='order' href='#' onclick=\"{HANDLER}\">Place an order</a>"
        ),
    };
    format!(
        "<html><head><title>Shop</title></head><body><nav>{}</nav><main><h1>Shop</h1>\
         <p>Some words about the shop, long enough to count as a page of its own.</p>{body}\
         <input id='pw' type='password' aria-label='Password'></main></body></html>",
        nav()
    )
}

struct Fx {
    base: String,
    version: Arc<AtomicUsize>,
    store: pagesift_store::Store,
    llm: String,
    seen: Arc<Mutex<Vec<String>>>,
    _dir: tempfile::TempDir,
}

impl Fx {
    async fn new(replies: Vec<Value>) -> Self {
        let version = Arc::new(AtomicUsize::new(1));
        let v = version.clone();
        let app = Router::new().route(
            "/shop/{item}",
            get(move |Path(_): Path<u32>| {
                let p = page(v.load(Ordering::SeqCst));
                async move { Html(p) }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let (llm, seen) = fake_llm(replies).await;
        let dir = tempfile::tempdir().unwrap();
        let store = pagesift_store::Store::open_dir(dir.path()).unwrap();
        Self {
            base,
            version,
            store,
            llm,
            seen,
            _dir: dir,
        }
    }

    fn scraper(&self) -> Arc<Scraper> {
        let mut cfg = ScraperConfig::default();
        cfg.fetcher.env_proxy = false;
        cfg.fetcher.ssrf.allow_private_network = true;
        cfg.policy.rate.default_interval = Duration::from_millis(1);
        cfg.store = Some(self.store.clone());
        cfg.llm = Some(pagesift_llm::LlmConfig::new(
            self.llm.clone(),
            "fixture-model",
            None,
        ));
        cfg.secrets = [("PW".to_string(), "hunter2-secret".to_string())].into();
        Arc::new(Scraper::new(cfg).unwrap())
    }

    fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

fn warnings(d: &pagesift::api::types::Document) -> Vec<String> {
    d.metadata
        .pagesift
        .as_ref()
        .map(|m| m.warnings.clone())
        .unwrap_or_default()
}

fn says(w: &[String], text: &str) -> bool {
    w.iter().any(|x| x == text)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn act_asks_the_model_once_and_again_only_when_the_page_really_changed() {
    if !available() {
        return;
    }
    let fx = Fx::new(vec![
        json!({"ids": ["@Buy now"]}),
        // v3：按钮的位置现在是“删除账号”，模型选下单
        json!({"ids": ["@Place an order"]}),
    ])
    .await;
    let s = fx.scraper();
    let url = |n: u32| format!("{}/shop/{n}", fx.base);
    let text = |d: &pagesift::api::types::Document| d.markdown.clone().unwrap_or_default();

    // ① 第一次：问一次模型，点到了
    let (d, info) = s.act(&url(1), "Buy the item", None).await.unwrap();
    assert_eq!(fx.calls(), 1);
    assert!(text(&d).contains("Clicked: Buy now"), "{}", text(&d));
    assert!(
        says(&warnings(&d), "act_plan: generated"),
        "{:?}",
        warnings(&d)
    );
    assert_eq!(info["llmCalls"], 1);

    // ② 重复：不问模型，照样点到；换一个同形状的网址、指令大小写不同也算同一条
    for (u, instruction) in [(url(1), "Buy the item"), (url(2), "buy the ITEM ")] {
        let (d, info) = s.act(&u, instruction, None).await.unwrap();
        assert_eq!(fx.calls(), 1, "{u}: repeated runs must not call the model");
        assert_eq!(info["llmCalls"], 0);
        assert!(text(&d).contains("Clicked: Buy now"), "{}", text(&d));
        assert!(
            says(&warnings(&d), "act_plan: cached"),
            "{:?}",
            warnings(&d)
        );
    }

    // ③ 重启（新实例、同一个存储）：缓存还在
    s.shutdown().await;
    let s = fx.scraper();
    let (d, _) = s.act(&url(1), "Buy the item", None).await.unwrap();
    assert_eq!(fx.calls(), 1, "the plan survives a restart");
    assert!(text(&d).contains("Clicked: Buy now"));

    // ④ 改版：locate 找回来，不问模型，新选择器写回
    fx.version.store(2, Ordering::SeqCst);
    let (d, _) = s.act(&url(1), "Buy the item", None).await.unwrap();
    assert_eq!(fx.calls(), 1, "locate must heal this without the model");
    assert!(text(&d).contains("Clicked: Buy now"), "{}", text(&d));
    let w = warnings(&d);
    assert!(says(&w, "selector_healed: steps[0] via locate"), "{w:?}");
    assert!(says(&w, "act_plan: cached"), "{w:?}");
    // 写回之后是精确命中
    let (d, _) = s.act(&url(1), "Buy the item", None).await.unwrap();
    assert_eq!(fx.calls(), 1);
    assert!(
        !warnings(&d)
            .iter()
            .any(|w| w.starts_with("selector_healed")),
        "{:?}",
        warnings(&d)
    );

    // ⑤ 同一个选择器现在是“删除账号”：不能照点，问模型，点到模型选的
    fx.version.store(3, Ordering::SeqCst);
    let (d, _) = s.act(&url(1), "Buy the item", None).await.unwrap();
    assert_eq!(fx.calls(), 2);
    assert!(!text(&d).contains("Clicked: Delete"), "{}", text(&d));
    assert!(text(&d).contains("Clicked: Place an order"), "{}", text(&d));
    let w = warnings(&d);
    assert!(says(&w, "selector_healed: steps via llm"), "{w:?}");
    assert!(says(&w, "act_plan: generated"), "{w:?}");
    // 新计划也缓存了
    let (d, _) = s.act(&url(1), "Buy the item", None).await.unwrap();
    assert_eq!(fx.calls(), 2);
    assert!(text(&d).contains("Clicked: Place an order"), "{}", text(&d));
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_plans_are_cached_and_answers_are_not() {
    if !available() {
        return;
    }
    let fx = Fx::new(vec![
        // 只是做动作：一次规划，不用作答
        json!({"steps": [{"action": "click", "id": "@Buy now"}], "output": null, "answer": false}),
        // 做完还要回答：规划一次、作答一次
        json!({"steps": [{"action": "click", "id": "@Buy now"}], "output": null, "answer": true}),
        json!({"output": "first answer"}),
        // 重复运行时规划不再问，作答每次都问（答案取决于当时的页面）
        json!({"output": "second answer"}),
        // 只读：没有步骤就没有可缓存的，每次都问
        json!({"steps": [], "output": "read one"}),
        json!({"steps": [], "output": "read two"}),
    ])
    .await;
    let s = fx.scraper();
    let app = pagesift::api::router(s.clone(), Default::default());
    let url = format!("{}/shop/1", fx.base);
    let open = async || -> String {
        let (st, v) = call(
            &app,
            "POST",
            "/v2/scrape",
            Some(json!({"url": url, "keepSession": true, "onlyMainContent": false})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        v["data"]["metadata"]["scrapeId"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let run = async |id: &str, prompt: &str| -> Value {
        let (st, v) = call(
            &app,
            "POST",
            &format!("/v2/scrape/{id}/interact"),
            Some(json!({ "prompt": prompt })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["success"], true, "{v}");
        v
    };
    let warned = |v: &Value, w: &str| {
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x.as_str() == Some(w))
    };

    // 只是做动作
    let v = run(&open().await, "Buy it").await;
    assert_eq!(fx.calls(), 1);
    assert!(
        v["data"]["markdown"]
            .as_str()
            .unwrap()
            .contains("Clicked: Buy now"),
        "{v}"
    );
    assert!(v.get("output").is_none(), "{v}");
    assert!(
        v["result"].as_str().unwrap().contains("Clicked: Buy now"),
        "{v}"
    );
    assert!(warned(&v, "act_plan: generated"), "{v}");
    assert_eq!(v["llmUsage"]["calls"], 1);
    let v = run(&open().await, "Buy it").await;
    assert_eq!(fx.calls(), 1, "a repeated action makes no model call");
    assert!(
        v["data"]["markdown"]
            .as_str()
            .unwrap()
            .contains("Clicked: Buy now"),
        "{v}"
    );
    assert!(warned(&v, "act_plan: cached"), "{v}");
    assert_eq!(v["llmUsage"]["calls"], 0);

    // 还要回答
    let q = "Buy it and tell me what happened";
    let v = run(&open().await, q).await;
    assert_eq!(fx.calls(), 3);
    assert_eq!(v["output"], "first answer", "{v}");
    let v = run(&open().await, q).await;
    assert_eq!(fx.calls(), 4, "the plan is cached, the answer is not");
    assert_eq!(v["output"], "second answer", "{v}");
    assert!(warned(&v, "act_plan: cached"), "{v}");

    // 只读
    let id = open().await;
    assert_eq!(run(&id, "What is the heading?").await["output"], "read one");
    assert_eq!(run(&id, "What is the heading?").await["output"], "read two");
    assert_eq!(fx.calls(), 6);
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn passwords_typed_literally_are_not_cached_and_placeholders_are() {
    if !available() {
        return;
    }
    let write = |text: &str| json!({"steps": [{"action": "write", "id": "@Password", "text": text}], "output": null, "answer": false});
    let fx = Fx::new(vec![
        write("plain-password"),
        write("plain-password"),
        write("{{secret:PW}}"),
    ])
    .await;
    let s = fx.scraper();
    let app = pagesift::api::router(s.clone(), Default::default());
    let url = format!("{}/shop/1", fx.base);
    let run = async |prompt: &str| {
        let (_, v) = call(
            &app,
            "POST",
            "/v2/scrape",
            Some(json!({"url": url, "keepSession": true})),
        )
        .await;
        let id = v["data"]["metadata"]["scrapeId"].as_str().unwrap();
        let (st, v) = call(
            &app,
            "POST",
            &format!("/v2/scrape/{id}/interact"),
            Some(json!({ "prompt": prompt })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["success"], true, "{v}");
    };
    // 明文密码：不落盘，所以每次都要问模型
    run("Type plain-password into the password box").await;
    run("Type plain-password into the password box").await;
    assert_eq!(fx.calls(), 2);
    // 占位符：可以缓存
    run("Type {{secret:PW}} into the password box").await;
    run("Type {{secret:PW}} into the password box").await;
    assert_eq!(fx.calls(), 3);
    s.shutdown().await;
}
