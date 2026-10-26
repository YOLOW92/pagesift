//! Firecrawl v1 and v2 `/batch/scrape` and job status/errors handlers.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use serde::Deserialize;

use crate::error::{ApiError, ErrorCode};
use crate::jobs::{Start, rfc3339};
use crate::limits::Caller;
use crate::server::{AppState, PAGE_SIZE, base_url, body, webhook};
use crate::types::{BatchScrapeRequest, JobErrors, JobStarted, JobStatus};
use crate::validate::{check_scrape_options, check_url, check_url_count};

#[derive(Deserialize)]
pub struct Paging {
    #[serde(default)]
    pub skip: usize,
}

pub async fn batch_start(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    uri: Uri,
    req: Result<Json<BatchScrapeRequest>, JsonRejection>,
) -> Result<Json<JobStarted>, ApiError> {
    let req = body(req)?;
    check_url_count(req.urls.len())?;
    check_scrape_options(&req.options, s.engine.as_ref())?;
    let (mut valid, mut invalid) = (Vec::new(), Vec::new());
    for u in req.urls {
        match check_url(&u) {
            Ok(()) => valid.push(u),
            Err(e) if !req.ignore_invalid_urls => return Err(e),
            Err(_) => invalid.push(u),
        }
    }
    if valid.is_empty() {
        return Err(ApiError::invalid("no valid URLs"));
    }
    let start = Start {
        owner: caller.map(|Extension(c)| c),
        webhook: webhook(&s, req.webhook.as_ref())?,
    };
    let concurrency = req
        .max_concurrency
        .unwrap_or(s.batch_concurrency)
        .min(s.batch_concurrency);
    let id = s
        .jobs
        .start_batch_with(s.engine.clone(), valid, req.options, concurrency, start)
        .await?;
    let prefix = if uri.path().starts_with("/v1") {
        "/v1"
    } else {
        "/v2"
    };
    Ok(Json(JobStarted {
        success: true,
        url: format!("{}{prefix}/batch/scrape/{id}", base_url(&s, &headers)),
        id,
        invalid_urls: req.ignore_invalid_urls.then_some(invalid),
    }))
}

pub async fn job_status(
    State(s): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    Path(id): Path<String>,
    Query(p): Query<Paging>,
) -> Result<Json<JobStatus>, ApiError> {
    let base = base_url(&s, &headers);
    let path = uri.path().to_string();
    let page = s
        .jobs
        .status(&id, p.skip, PAGE_SIZE)
        .await
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("job {id} not found")))?;
    let end = p.skip.min(page.docs) + page.data.len();
    let more = end < page.docs || (page.state.as_str() == "scraping" && end < page.total);
    Ok(Json(JobStatus {
        success: true,
        status: page.state.as_str().into(),
        completed: page.completed,
        total: page.total,
        credits_used: page.completed,
        expires_at: rfc3339(page.expires_at),
        next: (more && end > p.skip).then(|| format!("{base}{path}?skip={end}")),
        data: page.data,
        pagesift: page.summary,
    }))
}

pub async fn job_cancel(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if s.jobs.cancel(&id).await {
        Ok(StatusCode::OK)
    } else {
        Err(ApiError::new(
            ErrorCode::NotFound,
            format!("job {id} not found"),
        ))
    }
}

pub async fn job_errors(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<JobErrors>, ApiError> {
    if let Some((errors, blocked)) = s.jobs.errors(&id).await {
        Ok(Json(JobErrors {
            errors,
            robots_blocked: blocked,
        }))
    } else {
        Err(ApiError::new(
            ErrorCode::NotFound,
            format!("job {id} not found"),
        ))
    }
}
