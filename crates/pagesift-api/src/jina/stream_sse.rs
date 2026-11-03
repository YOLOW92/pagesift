//! Jina Reader 的 SSE（`Accept: text/event-stream`）：`progress`、每个非空段落一个 `content`、`done`。
//!
//! [`sse_parts`] 逐个产出事件文本，路由按段写进流式响应体（见 `reader_route`），不拼成一个大字符串；
//! [`stream_markdown_sse`] 保留旧的整块接口，字节和流式拼起来一样。
//! 事件在整页提取完成之后才产生：分帧发送，不是提取过程中的增量输出。

use axum::body::Body;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::error::ApiError;

/// 正文按空行切成段，空段不要。
pub(crate) fn split_paragraphs(text: &str) -> Vec<String> {
    text.split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .map(String::from)
        .collect()
}

/// SSE 的各个事件，按发送顺序：`progress`、每段一个 `content`、`done`（有警告时带 `warnings`）。
/// 每个事件以空行结束。
pub(crate) fn sse_parts<'a>(
    url: &'a str,
    title: Option<&'a str>,
    parts: &'a [String],
    warnings: &'a [String],
) -> impl Iterator<Item = String> + 'a {
    let progress = json!({
        "status": "processing",
        "url": url,
        "title": title.unwrap_or(""),
    });
    std::iter::once(format!("event: progress\ndata: {progress}\n\n"))
        .chain(
            parts
                .iter()
                .map(|p| format!("event: content\ndata: {}\n\n", json!({ "chunk": p }))),
        )
        .chain(std::iter::once_with(move || {
            let mut done = json!({
                "status": "completed",
                "url": url,
            });
            if !warnings.is_empty() {
                done["warnings"] = json!(warnings);
            }
            format!("event: done\ndata: {done}\n\n")
        }))
}

/// 响应头已经发出后抓取失败：用一个 `error` 事件报告，内容和普通请求的错误 JSON 一样。
pub(crate) fn error_event(e: &ApiError) -> String {
    let mut body = json!({
        "success": false,
        "code": e.code.as_str(),
        "error": e.message,
    });
    if let Some(d) = &e.details {
        body["details"] = d.clone();
    }
    format!("event: error\ndata: {body}\n\n")
}

/// 带 SSE 响应头的响应。
pub(crate) fn sse_response(body: Body) -> Response {
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(body)
        .unwrap_or_else(|_| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "SSE stream error",
            )
                .into_response()
        })
}

/// 整块的 SSE 响应（全部事件一次写出）。路由走流式，这个留给库的调用方。
pub fn stream_markdown_sse(url: &str, title: Option<&str>, markdown: &str) -> Response {
    let parts = split_paragraphs(markdown);
    sse_response(Body::from(
        sse_parts(url, title, &parts, &[]).collect::<String>(),
    ))
}
