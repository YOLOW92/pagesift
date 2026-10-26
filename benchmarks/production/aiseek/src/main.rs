//! AiSeek 的 `POST /v1/fetch` 契约的测试适配器（`cargo build --release` 在 benchmarks/production/aiseek 下）：把请求转给进程内的 pagesift Scraper，
//! 用来拿 AiSeek 现成的压测工具（`~/crawl-bench/bench.py`）和同一批网址，跟 Crawl4AI 版的
//! document-worker 做同口径对比（第 4 阶段的可靠性测试）。只给压测用，不是 pagesift 服务端的一部分。
//!
//! 契约见 AiSeek 的 `docs/research/crawl4ai-2026-10-21.md` 第 7 节：
//! 请求 `{url, mode: "crawl4ai"|"http", render, timeoutMs}`，头 `x-internal-service-token`；
//! 响应 `{finalUrl, statusCode, title, markdown, plainText, links, metadata, author, publishedAt, language}`；
//! 错误 400 `URL_NOT_ALLOWED`、403 被拒、429 限流、503 繁忙、504 超时，其余 5xx。
//! 比原版多分出两类：DNS 失败（502 `DNS_FAILURE`）和网络错误（502 `NETWORK_ERROR`）。
//!
//! 环境变量：`PORT`（默认 8100）、`TOKEN`、`PAGES`（浏览器进程上限，不设时自适应）、`ALLOW_HOSTS`（逗号分隔）。
//! 浏览器内核按 PATH 自动探测（Lightpanda → moli → Chrome 系）。

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use pagesift::api::ErrorCode;
use pagesift::api::types::{Document, ScrapeOptions};
use pagesift::{Scraper, ScraperConfig};
use serde::Deserialize;
use serde_json::{Value, json};

const UA: &str = "AiSeekBot/1.0 (+https://aiseek.dev/credits)";
const MAX_LINKS: usize = 500;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FetchRequest {
    url: String,
    #[serde(default = "default_mode")]
    mode: String,
    #[serde(default = "yes")]
    render: bool,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
}

fn default_mode() -> String {
    "crawl4ai".into()
}
fn yes() -> bool {
    true
}
fn default_timeout() -> u64 {
    60_000
}

struct App {
    scraper: Scraper,
    token: String,
}

#[tokio::main]
async fn main() {
    pagesift_sys::tune_allocator();
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.user_agent = UA.into();
    cfg.fetcher.max_redirects = 4;
    cfg.fetcher.max_body_bytes = 15 * 1024 * 1024;
    if let Some(hosts) = env("ALLOW_HOSTS") {
        cfg.fetcher.ssrf.allow_hosts = hosts.split(',').map(str::to_owned).collect();
    }
    // AiSeek 的 Node 侧自己做每站的节奏控制（host_pacing），这里不再额外限速
    cfg.policy.rate.default_interval = Duration::ZERO;
    if let Some(n) = env("PAGES").and_then(|v| v.parse().ok()) {
        cfg.browser.max_browsers = n;
    }
    let app = Arc::new(App {
        scraper: Scraper::new(cfg).expect("scraper"),
        token: env("TOKEN").unwrap_or_else(|| "bench-token-local-0123456789abcdef".into()),
    });
    let port: u16 = env("PORT").and_then(|v| v.parse().ok()).unwrap_or(8100);
    let router = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/v1/fetch", post(fetch))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        })
        .await
        .unwrap();
}

async fn fetch(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(req): Json<FetchRequest>,
) -> Response {
    if headers
        .get("x-internal-service-token")
        .and_then(|v| v.to_str().ok())
        != Some(&app.token)
    {
        return (StatusCode::UNAUTHORIZED, "bad token").into_response();
    }
    if !(1000..=180_000).contains(&req.timeout_ms) {
        return (StatusCode::BAD_REQUEST, "timeoutMs out of range").into_response();
    }
    let rendered = req.mode == "crawl4ai" && req.render;
    let opts: ScrapeOptions = serde_json::from_value(json!({
        "formats": ["markdown", "links"],
        "timeout": req.timeout_ms,
        "engine": if rendered { "browser" } else { "http" },
        // robots.txt 由 AiSeek 的 Node 侧检查
        "ignoreRobotsTxt": true,
        "storeInCache": false,
    }))
    .unwrap();
    match app.scraper.scrape(&req.url, &opts).await {
        Ok(doc) => Json(response(doc, rendered)).into_response(),
        // 纯 HTTP 模式下，原版把站点的 429 原样放进 statusCode（200 响应），由 Node 侧判成 slow
        Err(e)
            if !rendered && matches!(e.code, ErrorCode::RateLimited | ErrorCode::HostCooldown) =>
        {
            Json(json!({
                "finalUrl": req.url,
                "statusCode": 429,
                "markdown": "",
                "plainText": "",
                "links": [],
                "metadata": { "engine": "pagesift-http", "rendered": false, "extraction": "none" },
            }))
            .into_response()
        }
        Err(e) => {
            let (status, body) = match e.code {
                ErrorCode::SsrfBlocked | ErrorCode::InvalidRequest => (
                    StatusCode::BAD_REQUEST,
                    json!({ "error": { "code": "URL_NOT_ALLOWED", "message": e.message } }),
                ),
                ErrorCode::AccessBarrier | ErrorCode::RobotsDisallowed => (
                    StatusCode::FORBIDDEN,
                    json!({ "error": { "code": "BLOCKED", "message": format!("blocked: {}", e.message) } }),
                ),
                ErrorCode::RateLimited | ErrorCode::HostCooldown => (
                    StatusCode::TOO_MANY_REQUESTS,
                    json!({ "error": { "code": "RATE_LIMITED", "message": format!("rate limited: {}", e.message) } }),
                ),
                ErrorCode::Busy => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({ "error": { "code": "BUSY", "message": e.message } }),
                ),
                ErrorCode::Timeout => (
                    StatusCode::GATEWAY_TIMEOUT,
                    json!({ "error": { "code": "DEADLINE", "message": e.message } }),
                ),
                code @ (ErrorCode::DnsFailure | ErrorCode::NetworkError) => (
                    StatusCode::BAD_GATEWAY,
                    json!({ "error": { "code": code.as_str(), "message": e.message } }),
                ),
                code => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "error": { "code": code.as_str(), "message": e.message } }),
                ),
            };
            (status, Json(body)).into_response()
        }
    }
}

fn response(doc: Document, rendered: bool) -> Value {
    let m = &doc.metadata;
    let md = doc.markdown.clone().unwrap_or_default();
    let links: Vec<&String> = doc
        .links
        .iter()
        .flatten()
        .filter(|l| l.starts_with("http://") || l.starts_with("https://"))
        .take(MAX_LINKS)
        .collect();
    let engine = m
        .pagesift
        .as_ref()
        .and_then(|p| p.browser_engine.clone().or_else(|| p.engine.clone()));
    json!({
        "finalUrl": m.url,
        "statusCode": m.status_code,
        "title": m.title,
        "markdown": md,
        "plainText": plain_text(&md),
        "links": links,
        "metadata": {
            "engine": format!("pagesift-{}", engine.as_deref().unwrap_or("http")),
            "rendered": rendered,
            "extraction": "pagesift",
            "contentType": m.content_type,
        },
        "author": m.author,
        "publishedAt": m.published_time,
        "language": m.language,
    })
}

/// 去掉 Markdown 标记，留下给人看的文字（链接只留文字，图片去掉）。
fn plain_text(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    for line in md.lines() {
        let line = line.trim_start_matches(['#', '>', ' ', '-', '*', '+']);
        let mut s = String::with_capacity(line.len());
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'!' if b.get(i + 1) == Some(&b'[') => {
                    // 图片 ![alt](src)：整段去掉
                    if let Some(end) = line[i..].find(')') {
                        i += end + 1;
                        continue;
                    }
                    i += 1;
                }
                b'[' => {
                    if let Some(close) = line[i..].find("](")
                        && let Some(end) = line[i + close..].find(')')
                    {
                        s.push_str(&line[i + 1..i + close]);
                        i += close + end + 1;
                        continue;
                    }
                    i += 1;
                }
                b'*' | b'_' | b'`' | b'|' => i += 1,
                _ => {
                    let ch = line[i..].chars().next().unwrap();
                    s.push(ch);
                    i += ch.len_utf8();
                }
            }
        }
        let s = s.trim();
        if !s.is_empty() {
            out.push_str(s);
            out.push('\n');
        }
    }
    out
}
