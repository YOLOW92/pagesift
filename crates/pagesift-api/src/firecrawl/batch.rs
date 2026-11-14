//! Firecrawl v1 and v2 `/batch/scrape` and job status/errors handlers.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use serde::Deserialize;

use crate::error::{ApiError, ErrorCode};
use crate::jobs::{Start, rfc3339};
use crate::limits::{Caller, apply_http_caller_scope};
use crate::server::{AppState, PAGE_SIZE, base_url, lenient_body, webhook};
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
    req: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<JobStarted>, ApiError> {
    use crate::fields;
    let known = [
        BatchScrapeRequest::OWN_FIELDS,
        fields::names::<crate::types::ScrapeOptions>(),
        fields::CLIENT_IDENTITY,
    ];
    let (mut req, ignored): (BatchScrapeRequest, _) = lenient_body(req, &known)?;
    check_url_count(req.urls.len())?;
    let eff_caller = caller
        .map(|Extension(c)| c)
        .unwrap_or_else(|| Caller::new(None, s.default_limits.clone()));
    apply_http_caller_scope(&mut req.options, Some(&eff_caller), Some(&headers));
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
        owner: Some(eff_caller),
        webhook: webhook(&s, req.webhook.as_ref())?,
    };
    let concurrency = s.batch_concurrency_for(req.max_concurrency);
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
        warnings: fields::ignored_warning(&ignored).into_iter().collect(),
    }))
}

pub async fn job_status(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    uri: Uri,
    Path(id): Path<String>,
    Query(p): Query<Paging>,
) -> Result<Json<JobStatus>, ApiError> {
    authorize_job(&s, &id, caller.as_ref().map(|Extension(c)| c)).await?;
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
    caller: Option<Extension<Caller>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    authorize_job(&s, &id, caller.as_ref().map(|Extension(c)| c)).await?;
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
    caller: Option<Extension<Caller>>,
    Path(id): Path<String>,
) -> Result<Json<JobErrors>, ApiError> {
    authorize_job(&s, &id, caller.as_ref().map(|Extension(c)| c)).await?;
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

pub(super) async fn authorize_job(
    s: &AppState,
    id: &str,
    caller: Option<&Caller>,
) -> Result<(), ApiError> {
    if s.jobs.owned_by(id, caller).await {
        Ok(())
    } else {
        Err(ApiError::new(
            ErrorCode::NotFound,
            format!("job {id} not found"),
        ))
    }
}
