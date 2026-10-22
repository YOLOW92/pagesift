//! Firecrawl v1 and v2 `/crawl` endpoints.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, Uri};
use axum::Json;

use crate::error::ApiError;
use crate::jobs::Start;
use crate::limits::Caller;
use crate::server::{AppState, base_url, body, webhook};
use crate::types::{CrawlRequest, JobStarted};
use crate::validate::{check_scrape_options, check_url};

pub async fn crawl_start(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    uri: Uri,
    req: Result<Json<CrawlRequest>, JsonRejection>,
) -> Result<Json<JobStarted>, ApiError> {
    let req = body(req)?;
    check_url(&req.url)?;
    check_scrape_options(&req.scrape_options, s.engine.as_ref())?;
    for pattern in req.include_paths.iter().chain(&req.exclude_paths) {
        regex_check(pattern)?;
    }
    let start = Start {
        owner: caller.map(|Extension(c)| c),
        webhook: webhook(&s, req.webhook.as_ref())?,
    };
    let id = s
        .jobs
        .start_crawl_with(s.engine.clone(), req, start)
        .await?;
    let prefix = if uri.path().starts_with("/v1") { "/v1" } else { "/v2" };
    Ok(Json(JobStarted {
        success: true,
        url: format!("{}{prefix}/crawl/{id}", base_url(&s, &headers)),
        id,
        invalid_urls: None,
    }))
}

pub async fn jobs_active(State(s): State<AppState>) -> Json<serde_json::Value> {
    let ids = s.jobs.active().await;
    Json(serde_json::json!({
        "success": true,
        "crawls": ids.into_iter().map(|id| serde_json::json!({ "id": id })).collect::<Vec<_>>(),
    }))
}

pub fn regex_check(pattern: &str) -> Result<(), ApiError> {
    regex::RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .build()
        .map(|_| ())
        .map_err(|e| ApiError::invalid(format!("invalid path pattern {pattern:?}: {e}")))
}
