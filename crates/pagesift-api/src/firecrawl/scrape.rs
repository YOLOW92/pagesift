//! Firecrawl v1 and v2 `/scrape` endpoint handler.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use std::sync::atomic::Ordering;

use crate::error::ApiError;
use crate::limits::Caller;
use crate::metrics::PageSource;
use crate::server::{AppState, lenient_body};
use crate::types::{ScrapeRequest, ScrapeResponse};
use crate::validate::{check_scrape_options, check_url};

pub async fn scrape(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    req: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<ScrapeResponse>, ApiError> {
    run(s, caller, req, false, None).await
}

/// `/v2/scrape`：引擎支持 `/interact` 时结果带 `metadata.scrapeId`（v1 没有这个接口，响应不变）。
pub async fn scrape_v2(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    req: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<ScrapeResponse>, ApiError> {
    run(s, caller, req, true, None).await
}

/// Stream the extracted Markdown before optional image downloads finish.
pub async fn scrape_stream(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    req: Result<Json<serde_json::Value>, JsonRejection>,
) -> Response {
    let (progress, mut updates) = tokio::sync::broadcast::channel(1);
    let (tx, body) = crate::stream_body::channel();
    let mut worker = tokio::spawn(run(
        s,
        caller,
        req,
        true,
        Some(crate::types::MarkdownProgress(progress)),
    ));
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                Ok(preview) = updates.recv() => {
                    let data = serde_json::json!({"markdown": preview.markdown, "truncated": preview.truncated});
                    if tx.send(Bytes::from(format!("event: markdown\ndata: {data}\n\n"))).await.is_err() {
                        worker.abort();
                        return;
                    }
                }
                result = &mut worker => {
                    let (event, data) = match result {
                        Ok(Ok(Json(body))) => ("done", serde_json::to_value(body).unwrap_or_default()),
                        Ok(Err(e)) => ("error", serde_json::json!({"success":false, "code":e.code.as_str(), "error":e.message, "details":e.details})),
                        Err(_) => ("error", serde_json::json!({"success":false, "code":"INTERNAL_ERROR", "error":"scrape task failed"})),
                    };
                    let _ = tx.send(Bytes::from(format!("event: {event}\ndata: {data}\n\n"))).await;
                    return;
                }
                _ = tx.closed() => { worker.abort(); return; }
                _ = tokio::time::sleep(super::stream::KEEPALIVE) => {
                    if tx.send(Bytes::from_static(b": keepalive\n\n")).await.is_err() { worker.abort(); return; }
                }
            }
        }
    });
    (
        [
            (header::CONTENT_TYPE, "text/event-stream; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

async fn run(
    s: AppState,
    caller: Option<Extension<Caller>>,
    req: Result<Json<serde_json::Value>, JsonRejection>,
    v2: bool,
    progress: Option<crate::types::MarkdownProgress>,
) -> Result<Json<ScrapeResponse>, ApiError> {
    let (mut req, ignored): (ScrapeRequest, _) =
        lenient_body(req, &crate::fields::scrape_request())?;
    req.options.markdown_progress = progress;
    check_url(&req.url)?;
    // `keepSession` 只有这里认（校验拒收其余入口的）
    let keep = v2 && std::mem::take(&mut req.options.keep_session);
    req.options.owner = caller
        .as_ref()
        .map(|Extension(c)| c.account.to_string())
        .filter(|owner| owner != "anonymous");
    check_scrape_options(&req.options, s.engine.as_ref())?;
    if let Some(Extension(c)) = &caller {
        s.limiter.check_quota(c)?;
    }
    let result = if v2 && (keep || s.engine.supports_interact()) {
        let owner = caller.as_ref().map(|Extension(c)| c.account.as_ref());
        s.engine
            .scrape_session(&req.url, &req.options, keep, owner)
            .await
    } else {
        s.engine.scrape(&req.url, &req.options).await
    };
    match result {
        Ok(mut data) => {
            if let Some(w) = crate::fields::ignored_warning(&ignored) {
                data.metadata
                    .pagesift
                    .get_or_insert_with(Default::default)
                    .warnings
                    .push(w);
            }
            crate::attachments::keep_in(&data, s.jobs.disk().as_ref());
            s.metrics.scrape_ok.fetch_add(1, Ordering::Relaxed);
            s.metrics.page(PageSource::Scrape);
            if let Some(Extension(c)) = &caller {
                s.limiter.consume_pages(c, 1).await;
            }
            Ok(Json(ScrapeResponse {
                success: true,
                warning: data.warning.clone(),
                data,
            }))
        }
        Err(e) => {
            s.metrics.scrape_err.fetch_add(1, Ordering::Relaxed);
            Err(e)
        }
    }
}
