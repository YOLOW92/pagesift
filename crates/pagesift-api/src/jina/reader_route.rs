//! Jina Reader protocol handler (`GET /r/{*url}` and `POST /r`).
//! Provides direct Markdown extraction and JSON responses for AI Agents.

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::sync::atomic::Ordering;

use super::stream_sse::{error_event, split_paragraphs, sse_parts, sse_response};
use crate::limits::Caller;
use crate::metrics::PageSource;
use crate::server::AppState;
use crate::types::ScrapeOptions;

#[derive(Debug, Deserialize)]
pub struct JinaPostRequest {
    pub url: String,
}

/// Handler for `GET /r/{*target_url}`.
pub(crate) async fn handle_jina_get(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Path(raw_url): Path<String>,
) -> Response {
    let clean_url = parse_target_url(&raw_url);
    process_jina_scrape(s, caller, headers, &clean_url).await
}

/// Handler for `POST /r`.
pub(crate) async fn handle_jina_post(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(payload): Json<JinaPostRequest>,
) -> Response {
    let clean_url = parse_target_url(&payload.url);
    process_jina_scrape(s, caller, headers, &clean_url).await
}

fn parse_target_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else if let Some(idx) = trimmed.find("http:/") {
        if trimmed[idx..].starts_with("http://") {
            trimmed[idx..].to_string()
        } else {
            format!("http://{}", trimmed[idx + 6..].trim_start_matches('/'))
        }
    } else if let Some(idx) = trimmed.find("https:/") {
        if trimmed[idx..].starts_with("https://") {
            trimmed[idx..].to_string()
        } else {
            format!("https://{}", trimmed[idx + 7..].trim_start_matches('/'))
        }
    } else {
        format!("https://{}", trimmed.trim_start_matches('/'))
    }
}

async fn process_jina_scrape(
    s: AppState,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    target_url: &str,
) -> Response {
    if let Err(e) = crate::validate::check_url(target_url) {
        return e.into_response();
    }
    if let Some(Extension(c)) = &caller
        && let Err(e) = s.limiter.check_quota(c)
    {
        return e.into_response();
    }

    let (opts, params) = match super::headers::parse(&headers) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = crate::validate::check_scrape_options(&opts, s.engine.as_ref()) {
        return e.into_response();
    }
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if accept.contains("text/event-stream") {
        return stream_scrape(
            s,
            caller.map(|Extension(c)| c),
            target_url.to_string(),
            opts,
            params,
        );
    }

    match s.engine.scrape(target_url, &opts).await {
        Ok(doc) => {
            record_ok(&s, caller.as_ref().map(|Extension(c)| c)).await;
            let r = match super::respond::render(&doc, target_url, &params) {
                Ok(r) => r,
                Err(e) => return e.into_response(),
            };
            let mut warnings = doc
                .metadata
                .pagesift
                .as_ref()
                .map(|m| m.warnings.clone())
                .unwrap_or_default();
            warnings.extend(r.warnings.iter().cloned());
            if accept.contains("application/json") {
                let mut data = serde_json::json!({
                    "title": doc.metadata.title.as_deref().unwrap_or(""),
                    "description": doc.metadata.description.clone().unwrap_or_default(),
                    "url": target_url,
                    "content": r.body,
                });
                if let Some(chunks) = &r.chunks {
                    data["chunks"] = serde_json::json!(chunks);
                }
                if !warnings.is_empty() {
                    data["warnings"] = serde_json::json!(warnings);
                }
                // [协议] Jina Reader JSON 响应的固定外壳：code 200、status 20000
                Json(serde_json::json!({ "code": 200, "status": 20000, "data": data }))
                    .into_response()
            } else {
                let body = r.text;
                let mut resp = Response::builder().header(header::CONTENT_TYPE, r.content_type);
                // 纯文本响应没有放警告的字段：写进响应头（`x-pagesift-warnings`，多条用 `; ` 隔开）
                if let Some(v) = (!warnings.is_empty())
                    .then(|| warnings.join("; "))
                    .and_then(|w| axum::http::HeaderValue::from_str(&header_safe(&w)).ok())
                {
                    resp = resp.header("x-pagesift-warnings", v);
                }
                resp.body(axum::body::Body::from(body)).unwrap_or_else(|_| {
                    (StatusCode::INTERNAL_SERVER_ERROR, "response builder error").into_response()
                })
            }
        }
        Err(e) => {
            s.metrics.scrape_err.fetch_add(1, Ordering::Relaxed);
            e.into_response()
        }
    }
}

async fn record_ok(s: &AppState, caller: Option<&Caller>) {
    s.metrics.scrape_ok.fetch_add(1, Ordering::Relaxed);
    s.metrics.page(PageSource::Scrape);
    if let Some(c) = caller {
        s.limiter.consume_pages(c, 1).await;
    }
}

/// SSE：响应头立刻发出，抓取在流里进行；整页抓完、提取完之后才逐个写出 progress、各段 content 和 done
/// （按段分帧写出，不拼成整块），抓取期间按心跳间隔写注释保活。不边提取边推：正文识别要看完整个 DOM
/// 才知道哪些块是正文（打分、去模板），提取本身只占请求耗时的一小部分（0.91 发布说明里有本机实测），
/// 第一个 `content` 的时刻等于整页提取完成的时刻（0.90 的发布说明写成了"一产出就写"，不准确）。
/// 客户端中途断开就停止抓取。响应头发出后才失败的，用 `error` 事件报告。
fn stream_scrape(
    s: AppState,
    caller: Option<Caller>,
    url: String,
    opts: ScrapeOptions,
    params: super::headers::JinaParams,
) -> Response {
    stream_scrape_with(
        s,
        caller,
        url,
        opts,
        params,
        crate::firecrawl::stream::KEEPALIVE,
    )
}

/// 同 [`stream_scrape`]，`keepalive` 是抓取期间写心跳注释（`: keepalive`）的间隔：渲染、慢站点
/// 可能几十秒才有结果，期间不写字节，代理和客户端会按空闲超时断开（0.90 如此）。SSE 客户端忽略注释行，
/// 事件本身的字节不变。
fn stream_scrape_with(
    s: AppState,
    caller: Option<Caller>,
    url: String,
    opts: ScrapeOptions,
    params: super::headers::JinaParams,
    keepalive: std::time::Duration,
) -> Response {
    let (tx, body) = crate::stream_body::channel();
    tokio::spawn(async move {
        let scrape = s.engine.scrape(&url, &opts);
        tokio::pin!(scrape);
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + keepalive, keepalive);
        let r = loop {
            tokio::select! {
                r = &mut scrape => break r,
                () = tx.closed() => return,
                _ = tick.tick() => {
                    if tx.send(axum::body::Bytes::from_static(b": keepalive\n\n")).await.is_err() {
                        return;
                    }
                }
            }
        };
        match r {
            Ok(doc) => {
                record_ok(&s, caller.as_ref()).await;
                // 和普通响应走同一份渲染：respond-with、保留方式、上限、分块都生效
                let r = match super::respond::render(&doc, &url, &params) {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = tx.send(error_event(&e).into()).await;
                        return;
                    }
                };
                let mut warnings = doc
                    .metadata
                    .pagesift
                    .as_ref()
                    .map(|m| m.warnings.clone())
                    .unwrap_or_default();
                warnings.extend(r.warnings.iter().cloned());
                let parts: Vec<String> = match r.chunks {
                    Some(c) => c,
                    None => split_paragraphs(&r.body),
                };
                let events = sse_parts(&url, doc.metadata.title.as_deref(), &parts, &warnings);
                for event in events {
                    if tx.send(event.into()).await.is_err() {
                        return;
                    }
                }
            }
            Err(e) => {
                s.metrics.scrape_err.fetch_add(1, Ordering::Relaxed);
                let _ = tx.send(error_event(&e).into()).await;
            }
        }
    });
    sse_response(body)
}

/// 响应头只能放可见 ASCII：其余字节（比如警告里的中文文件名）写成百分号编码，不让整个头丢掉。
fn header_safe(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if (b' '..=b'~').contains(&b) && b != b'%' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
