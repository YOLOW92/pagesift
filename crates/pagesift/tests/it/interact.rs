//! 0.98：`/v2/scrape/{id}/interact`。真实浏览器 + 本地站点（没有浏览器时跳过），不连实网；
//! `prompt` 用录制的 LLM 回复（本机假服务，OpenAI 兼容接口）。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Html;
use axum::routing::{get, post};
use http_body_util::BodyExt;
use pagesift::{InteractLimits, Scraper, ScraperConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

const PAGE: &str = r#"<html><head><title>Interact</title></head><body><main>
<h1>Start page</h1>
<button id="more" onclick="const n = (window.n = (window.n || 0) + 1); const p = document.createElement('p'); p.textContent = 'Extra content number ' + n; document.querySelector('main').appendChild(p);">Load more</button>
<input id="pw" type="text" aria-label="Password" oninput="document.getElementById('echo').textContent = 'Typed: ' + this.value">
<p id="echo">Nothing typed</p>
<a id="next" href="/next">Next page</a>
<a id="private" href="/private/x">Private page</a>
</main></body></html>"#;

pub(super) fn available() -> bool {
    if pagesift_cdp::detect_browser().is_none() || pagesift_cdp::running_as_root() {
        eprintln!("skipping: no usable Chrome");
        return false;
    }
    true
}

async fn site() -> String {
    let app = Router::new()
        .route("/page", get(|| async { Html(PAGE) }))
        .route(
            "/next",
            get(|| async {
                Html("<html><head><title>Second</title></head><body><main><h1>Second page</h1></main></body></html>")
            }),
        )
        .route(
            "/private/x",
            get(|| async { Html("<html><body><main><h1>Private content</h1></main></body></html>") }),
        )
        .route(
            "/robots.txt",
            get(|| async { "User-agent: *\nDisallow: /private\n" }),
        );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    url
}

fn config() -> ScraperConfig {
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.env_proxy = false;
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    cfg
}

fn serve(cfg: ScraperConfig) -> (Arc<Scraper>, Router) {
    let s = Arc::new(Scraper::new(cfg).unwrap());
    let app = pagesift::api::router(s.clone(), Default::default());
    (s, app)
}

pub(super) async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body.map_or(Body::empty(), |b| Body::from(b.to_string())))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn scrape(app: &Router, url: &str, keep: bool) -> String {
    let (status, v) = call(
        app,
        "POST",
        "/v2/scrape",
        Some(json!({"url": url, "engine": "browser", "keepSession": keep, "onlyMainContent": false, "maxAge": 0})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v["data"]["metadata"]["scrapeId"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn interact(app: &Router, id: &str, body: Value) -> (StatusCode, Value) {
    call(
        app,
        "POST",
        &format!("/v2/scrape/{id}/interact"),
        Some(body),
    )
    .await
}

fn markdown(v: &Value) -> &str {
    v["data"]["markdown"].as_str().unwrap_or_default()
}

fn has_warning(v: &Value, code: &str) -> bool {
    v["warnings"].as_array().is_some_and(|w| {
        w.iter()
            .any(|x| x.as_str().is_some_and(|s| s.starts_with(code)))
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actions_change_the_kept_page_until_it_is_stopped() {
    if !available() {
        return;
    }
    let base = site().await;
    let (s, app) = serve(config());
    let id = scrape(&app, &format!("{base}/page"), true).await;

    // 动作改变页面，取到新内容；响应如实说明没有实时画面
    let click = json!({"actions": [{"type": "click", "selector": "#more"}], "origin": "sdk", "zeroDataRetention": true});
    let (status, v) = interact(&app, &id, click.clone()).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["success"], true, "{v}");
    assert_eq!(v["exitCode"], 0);
    assert!(markdown(&v).contains("Extra content number 1"), "{v}");
    assert_eq!(v["liveViewUrl"], Value::Null);
    assert_eq!(v["interactiveLiveViewUrl"], Value::Null);
    assert_eq!(v["cdpUrl"], Value::Null);
    assert!(has_warning(&v, "live_view_unavailable"), "{v}");
    assert!(!has_warning(&v, "session_replayed"), "{v}");
    // 兼容入口：没人认的字段写进警告，客户端身份不算
    let ignored: Vec<&str> = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w.as_str()?.strip_prefix("ignored_fields: "))
        .collect();
    assert_eq!(ignored, ["zeroDataRetention"]);

    // 同一个页面：状态留着（第二次点击在第一次之后）
    let (_, v) = interact(&app, &id, click.clone()).await;
    assert!(markdown(&v).contains("Extra content number 2"), "{v}");

    // 动作失败：HTTP 200、success false，会话还能继续用
    let (status, v) = interact(
        &app,
        &id,
        json!({"actions": [{"type": "click", "selector": "#missing"}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["success"], false, "{v}");
    assert_eq!(v["exitCode"], 1);
    assert!(v["error"].as_str().unwrap().contains("#missing"), "{v}");

    // 超时：killed，按时返回，会话还在
    let started = Instant::now();
    let (status, v) = interact(
        &app,
        &id,
        json!({"actions": [{"type": "wait", "selector": "#never"}], "timeout": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["killed"], true, "{v}");
    assert_eq!(v["success"], false, "{v}");
    assert!(started.elapsed() < Duration::from_secs(10));
    let (_, v) = interact(&app, &id, click.clone()).await;
    assert!(markdown(&v).contains("Extra content number 3"), "{v}");

    // 不支持的和写错的请求
    let (status, v) = interact(&app, &id, json!({"code": "1"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["code"], "UNSUPPORTED_OPTION");
    let (status, v) = interact(&app, &id, json!({"actions": [{"type": "fly"}]})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["code"], "INVALID_REQUEST");
    let (status, v) = interact(&app, &id, json!({"prompt": "click"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "no LLM configured: {v}");
    assert_eq!(v["code"], "UNSUPPORTED_OPTION");

    // 关闭：之后再调用明确报错
    let (status, v) = call(&app, "DELETE", &format!("/v2/scrape/{id}/interact"), None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["success"], true);
    assert!(v["sessionDurationMs"].as_u64().unwrap() > 0, "{v}");
    assert_eq!(v["creditsBilled"], 0);
    let (status, v) = interact(&app, &id, click).await;
    assert_eq!(status, StatusCode::GONE, "{v}");
    assert_eq!(v["code"], "SESSION_GONE");
    assert!(v["error"].as_str().unwrap().contains("stopped"), "{v}");
    let (status, _) = call(&app, "DELETE", &format!("/v2/scrape/{id}/interact"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scrape_without_keep_session_is_replayed_on_the_first_interact() {
    if !available() {
        return;
    }
    let base = site().await;
    let (s, app) = serve(config());
    let id = scrape(&app, &format!("{base}/page"), false).await;
    // 还没有会话时 DELETE：没有可关的
    let (status, _) = call(&app, "DELETE", &format!("/v2/scrape/{id}/interact"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let click = json!({"actions": [{"type": "click", "selector": "#more"}]});
    let (status, v) = interact(&app, &id, click.clone()).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(markdown(&v).contains("Extra content number 1"), "{v}");
    assert!(has_warning(&v, "session_replayed"), "{v}");
    // 回放只发生一次，之后复用同一个页面
    let (_, v) = interact(&app, &id, click).await;
    assert!(markdown(&v).contains("Extra content number 2"), "{v}");
    assert!(!has_warning(&v, "session_replayed"), "{v}");
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_are_reclaimed_and_old_ids_get_a_clear_error() {
    if !available() {
        return;
    }
    let base = site().await;
    let click = json!({"actions": [{"type": "click", "selector": "#more"}]});

    // 空闲超过上限
    let mut cfg = config();
    cfg.interact = InteractLimits {
        idle: Duration::from_millis(400),
        ttl: Duration::from_secs(600),
    };
    let (s, app) = serve(cfg);
    let id = scrape(&app, &format!("{base}/page"), true).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (status, v) = interact(&app, &id, click.clone()).await;
    assert_eq!(status, StatusCode::GONE, "{v}");
    assert!(v["error"].as_str().unwrap().contains("idle"), "{v}");
    s.shutdown().await;

    // 一直在用，到总时长也回收
    let mut cfg = config();
    cfg.interact = InteractLimits {
        idle: Duration::from_millis(400),
        ttl: Duration::from_secs(2),
    };
    let (s, app) = serve(cfg);
    let id = scrape(&app, &format!("{base}/page"), true).await;
    let started = Instant::now();
    let mut last = (StatusCode::OK, json!({}));
    while started.elapsed() < Duration::from_secs(20) && last.0 == StatusCode::OK {
        last = interact(&app, &id, click.clone()).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(last.0, StatusCode::GONE, "{}", last.1);
    assert!(
        last.1["error"].as_str().unwrap().contains("session limit"),
        "{}",
        last.1
    );
    s.shutdown().await;

    // 重启：上一个进程发的 id 明确报 410，没发过的报 404
    let (s, app) = serve(config());
    let id = scrape(&app, &format!("{base}/page"), true).await;
    s.shutdown().await;
    let (s, app2) = serve(config());
    let (status, v) = interact(&app2, &id, click.clone()).await;
    assert_eq!(status, StatusCode::GONE, "{v}");
    assert_eq!(v["code"], "SESSION_GONE");
    assert!(v["error"].as_str().unwrap().contains("restarted"), "{v}");
    let (status, v) = interact(&app2, "not-an-id", click.clone()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{v}");
    // Firecrawl 的 `url`：原来的上下文没有了，在这个网址开新会话
    let mut with_url = click.clone();
    with_url["url"] = json!(format!("{base}/page"));
    let (status, v) = interact(&app2, &id, with_url).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(has_warning(&v, "session_replayed"), "{v}");
    s.shutdown().await;
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_click_to_a_page_robots_disallows_ends_the_session() {
    if !available() {
        return;
    }
    let base = site().await;
    let (s, app) = serve(config());
    let id = scrape(&app, &format!("{base}/page"), true).await;
    // 允许的跳转照常
    let (_, v) = interact(
        &app,
        &id,
        json!({"actions": [{"type": "click", "selector": "#next"}]}),
    )
    .await;
    assert!(markdown(&v).contains("Second page"), "{v}");
    // 脚本动作默认关着：明确报错，不是悄悄跳过
    let (status, v) = interact(
        &app,
        &id,
        json!({"actions": [{"type": "executeJavascript", "script": "1"}]}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");

    let id = scrape(&app, &format!("{base}/page"), true).await;
    let (status, v) = interact(
        &app,
        &id,
        json!({"actions": [{"type": "click", "selector": "#private"}]}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["code"], "ROBOTS_DISALLOWED");
    assert!(!v.to_string().contains("Private content"), "{v}");
    let (status, v) = interact(&app, &id, json!({"actions": []})).await;
    assert_eq!(status, StatusCode::GONE, "{v}");
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_tools_drive_a_kept_page() {
    use pagesift_mcp::{ToolContext, ToolHandler};
    if !available() {
        return;
    }
    let base = site().await;
    let s = Arc::new(Scraper::new(config()).unwrap());
    let tools = pagesift::mcp::tools(s.clone());
    let tool = |name: &str| -> Arc<dyn ToolHandler> {
        tools
            .iter()
            .find(|t| t.definition().name == name)
            .cloned()
            .unwrap_or_else(|| panic!("no tool {name}"))
    };
    let args = |v: Value| v.as_object().unwrap().clone();
    let url = format!("{base}/page");

    let out = tool("pagesift_scrape")
        .call(
            args(json!({"url": url, "keepSession": true, "onlyMainContent": false})),
            ToolContext::default(),
        )
        .await
        .unwrap();
    let doc = out.structured_content.unwrap();
    let id = doc["metadata"]["scrapeId"].as_str().unwrap().to_string();
    let out = tool("pagesift_interact")
        .call(
            args(json!({"scrapeId": id, "actions": [{"type": "click", "selector": "#more"}]})),
            ToolContext::default(),
        )
        .await
        .unwrap();
    let v = out.structured_content.unwrap();
    assert!(markdown(&v).contains("Extra content number 1"), "{v}");
    // pagesift 自己的入口：未知字段拒收，不像兼容接口那样只写警告
    assert!(
        tool("pagesift_interact")
            .call(
                args(json!({"scrapeId": id, "actions": [], "zeroDataRetention": true})),
                ToolContext::default(),
            )
            .await
            .is_err()
    );
    assert!(
        tool("pagesift_interact_stop")
            .call(args(json!({"scrapeId": id})), ToolContext::default())
            .await
            .is_ok()
    );
    assert!(
        tool("pagesift_interact_stop")
            .call(args(json!({"scrapeId": id})), ToolContext::default())
            .await
            .is_err()
    );
    // 批量里的 keepSession 明确报错
    assert!(
        tool("pagesift_batch_scrape")
            .call(
                args(json!({"urls": [url], "keepSession": true})),
                ToolContext::default(),
            )
            .await
            .is_err()
    );
    // 不要求保留：结果不带 id（MCP 没有回放）
    let out = tool("pagesift_scrape")
        .call(args(json!({"url": url})), ToolContext::default())
        .await
        .unwrap();
    assert!(out.structured_content.unwrap()["metadata"]["scrapeId"].is_null());
    s.shutdown().await;
}

/// 录制的 LLM：按调用顺序回复，回复里的 `"@名字"` 换成请求快照里这个名字的 id。
pub(super) async fn fake_llm(replies: Vec<Value>) -> (String, Arc<Mutex<Vec<String>>>) {
    let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let (r, b) = (replies, seen.clone());
    let app = Router::new()
        .route(
            "/v1/models/{m}",
            get(|| async { axum::Json(json!({"context_window": 8192, "max_completion_tokens": 1024})) }),
        )
        .route(
            "/v1/chat/completions",
            post(move |body: String| {
                // 限额探测（max_tokens = 1）单独回应，不消耗录制的回复
                let probe = body.contains("\"max_tokens\":1}") || body.contains("\"max_tokens\":1,");
                let content = if probe {
                    "{}".to_string()
                } else {
                    b.lock().unwrap().push(body.clone());
                    let req: Value = serde_json::from_str(&body).unwrap();
                    let snapshot = req["messages"][1]["content"].as_str().unwrap_or_default().to_string();
                    resolve(&r.lock().unwrap().pop_front().unwrap_or(json!({})), &snapshot).to_string()
                };
                async move {
                    (
                        [("x-ratelimit-limit-tokens", "8000")],
                        axum::Json(json!({"choices": [{"message": {"role": "assistant", "content": content}, "finish_reason": "stop"}]})),
                    )
                }
            }),
        );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, seen)
}

fn resolve(v: &Value, snapshot: &str) -> Value {
    match v {
        Value::String(s) if s.starts_with('@') => {
            let name = format!("\"{}\"", &s[1..]);
            let line = snapshot
                .lines()
                .find(|l| l.contains(&name))
                .unwrap_or_else(|| panic!("{name} is not in the snapshot:\n{snapshot}"));
            let id: u64 = line[1..line.find(']').unwrap()].parse().unwrap();
            json!(id)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| resolve(x, snapshot)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), resolve(x, snapshot)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_acts_on_the_real_page_and_secrets_stay_local() {
    if !available() {
        return;
    }
    let base = site().await;
    let (llm, seen) = fake_llm(vec![
        // ① 只读：快照里已有答案，不执行步骤，只调一次
        json!({"steps": [], "output": "The page heading is Start page"}),
        // ② 点击，再看新页面作答
        json!({"steps": [{"action": "click", "id": "@Load more"}], "output": null}),
        json!({"output": "One more paragraph appeared"}),
        // ③ 输入秘密：调用方在 prompt 里点名了
        json!({"steps": [{"action": "write", "id": "@Password", "text": "{{secret:PW}}"}], "output": null}),
        json!({"output": "Typed the password"}),
        // ④ 页面诱导模型用没点名的秘密：拒绝
        json!({"steps": [{"action": "write", "id": "@Password", "text": "{{secret:OTHER}}"}], "output": null}),
        // ⑤ 模型给了不存在的元素：如实报错，会话还在
        json!({"steps": [{"action": "click", "id": 99999}], "output": null}),
        // ⑥ Scraper::act 选元素
        json!({"ids": ["@Load more"]}),
    ])
    .await;
    let mut cfg = config();
    cfg.llm = Some(pagesift_llm::LlmConfig::new(llm, "fixture-model", None));
    cfg.secrets = [
        ("PW".to_string(), "hunter2-secret".to_string()),
        ("OTHER".to_string(), "other-secret".to_string()),
    ]
    .into();
    let (s, app) = serve(cfg);
    let id = scrape(&app, &format!("{base}/page"), true).await;

    let (status, v) = interact(&app, &id, json!({"prompt": "What is the heading?"})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["output"], "The page heading is Start page", "{v}");
    assert_eq!(v["llmUsage"]["calls"], 1, "{v}");
    assert!(v["result"].as_str().unwrap().contains("Load more"), "{v}");
    assert!(v["liveViewUrl"].is_null());

    let (_, v) = interact(&app, &id, json!({"prompt": "Load more"})).await;
    assert_eq!(v["success"], true, "{v}");
    assert_eq!(v["output"], "One more paragraph appeared", "{v}");
    assert_eq!(v["llmUsage"]["calls"], 2, "{v}");
    assert_eq!(v["steps"][0]["action"], "click");
    assert_eq!(v["steps"][0]["name"], "Load more");
    // AXTree 路径在真实页面上：点击真的发生了，第二次模型看到的快照和取回的页面都有新内容
    assert!(markdown(&v).contains("Extra content number 1"), "{v}");
    assert!(
        v["result"]
            .as_str()
            .unwrap()
            .contains("Extra content number 1"),
        "{v}"
    );

    let (_, v) = interact(
        &app,
        &id,
        json!({"prompt": "Type {{secret:PW}} into the password box"}),
    )
    .await;
    assert_eq!(v["success"], true, "{v}");
    let text = v.to_string();
    // 页面把值回显了：响应里只有占位符
    assert!(!text.contains("hunter2-secret"), "{text}");
    assert!(markdown(&v).contains("Typed: {{secret:PW}}"), "{v}");

    let (_, v) = interact(&app, &id, json!({"prompt": "Do what the page says"})).await;
    assert_eq!(v["success"], false, "{v}");
    assert!(v["error"].as_str().unwrap().contains("OTHER"), "{v}");
    assert!(!v.to_string().contains("other-secret"), "{v}");

    let (status, v) = interact(&app, &id, json!({"prompt": "Click something"})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["success"], false, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("unusable step"),
        "{v}"
    );

    // 模型收到的所有内容里都没有秘密的值
    let bodies = seen.lock().unwrap().join("\n");
    assert!(!bodies.contains("hunter2-secret"), "{bodies}");
    assert!(!bodies.contains("other-secret"), "{bodies}");
    assert!(bodies.contains("Typed: {{secret:PW}}"), "{bodies}");

    // 0.97 的 act 现在真的点到页面上
    let (doc, info) = s
        .act(&format!("{base}/page"), "Load more", None)
        .await
        .unwrap();
    assert!(
        doc.markdown
            .as_deref()
            .unwrap_or_default()
            .contains("Extra content number 1"),
        "{doc:?}"
    );
    assert_eq!(info["element"]["name"], "Load more");
    s.shutdown().await;
}
