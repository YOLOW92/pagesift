//! Jina Reader protocol handler (`GET /r/{*url}` and `POST /r`).
//! Provides direct Markdown extraction and JSON responses for AI Agents.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::sync::Arc;

use super::stream_sse::stream_markdown_sse;
use crate::engine::Engine;
use crate::types::ScrapeOptions;

#[derive(Debug, Deserialize)]
pub struct JinaPostRequest {
    pub url: String,
}

pub struct JinaState {
    pub engine: Arc<dyn Engine>,
}

/// Handler for `GET /r/{*target_url}`.
pub async fn handle_jina_get(
    State(engine): State<Arc<dyn Engine>>,
    headers: HeaderMap,
    Path(raw_url): Path<String>,
) -> Response {
    let clean_url = parse_target_url(&raw_url);
    process_jina_scrape(engine, headers, &clean_url).await
}

/// Handler for `POST /r`.
pub async fn handle_jina_post(
    State(engine): State<Arc<dyn Engine>>,
    headers: HeaderMap,
    Json(payload): Json<JinaPostRequest>,
) -> Response {
    let clean_url = parse_target_url(&payload.url);
    process_jina_scrape(engine, headers, &clean_url).await
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
    engine: Arc<dyn Engine>,
    headers: HeaderMap,
    target_url: &str,
) -> Response {
    if let Err(e) = crate::validate::check_url(target_url) {
        return e.into_response();
    }

    let opts = ScrapeOptions {
        only_main_content: true,
        ..Default::default()
    };

    match engine.scrape(target_url, &opts).await {
        Ok(doc) => {
            let markdown = doc.markdown.unwrap_or_default();
            let title = doc.metadata.title.as_deref();

            let accept = headers
                .get(header::ACCEPT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");

            if accept.contains("text/event-stream") {
                stream_markdown_sse(target_url, title, &markdown)
            } else if accept.contains("application/json") {
                let json_data = serde_json::json!({
                    "code": 200,
                    "status": 20000,
                    "data": {
                        "title": title.unwrap_or(""),
                        "description": doc.metadata.description.unwrap_or_default(),
                        "url": target_url,
                        "content": markdown,
                    }
                });
                Json(json_data).into_response()
            } else {
                // Default: Direct Raw Markdown for LLM Agents
                Response::builder()
                    .header(header::CONTENT_TYPE, "text/markdown; charset=utf-8")
                    .body(axum::body::Body::from(markdown))
                    .unwrap_or_else(|_| {
                        (StatusCode::INTERNAL_SERVER_ERROR, "response builder error")
                            .into_response()
                    })
            }
        }
        Err(e) => e.into_response(),
    }
}
