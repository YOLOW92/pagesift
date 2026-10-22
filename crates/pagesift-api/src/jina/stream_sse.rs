//! Server-Sent Events (SSE) streaming adapter for Jina Reader protocol.
//! Streams progressive fetching status and Markdown content chunks over `text/event-stream`.

use axum::http::header;
use axum::response::{IntoResponse, Response};

/// Construct an SSE streaming response for scraped markdown content.
pub fn stream_markdown_sse(url: &str, title: Option<&str>, markdown: &str) -> Response {
    let mut sse_text = String::new();

    // 1. Progress Event: Fetching / Processing
    let progress_payload = serde_json::json!({
        "status": "processing",
        "url": url,
        "title": title.unwrap_or(""),
    });
    sse_text.push_str(&format!("event: progress\ndata: {}\n\n", progress_payload));

    // 2. Content Data Event: Stream Markdown in logical paragraph blocks
    let paragraphs: Vec<&str> = markdown.split("\n\n").collect();
    for p in paragraphs {
        if !p.trim().is_empty() {
            let data_payload = serde_json::json!({
                "chunk": p,
            });
            sse_text.push_str(&format!("event: content\ndata: {}\n\n", data_payload));
        }
    }

    // 3. Completion Event
    let done_payload = serde_json::json!({
        "status": "completed",
        "url": url,
    });
    sse_text.push_str(&format!("event: done\ndata: {}\n\n", done_payload));

    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(axum::body::Body::from(sse_text))
        .unwrap_or_else(|_| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "SSE stream error").into_response())
}
