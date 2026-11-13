//! Firecrawl v1 and v2 `/crawl` endpoints.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, Uri};

use crate::error::ApiError;
use crate::jobs::Start;
use crate::limits::Caller;
use crate::server::{AppState, base_url, lenient_body, webhook};
use crate::types::{CrawlRequest, JobStarted};
use crate::validate::{MAX_CRAWL_DEPTH, MAX_CRAWL_LIMIT, check_scrape_options, check_url};

pub async fn crawl_start(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    uri: Uri,
    req: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<JobStarted>, ApiError> {
    use crate::fields;
    let known = [fields::names::<CrawlRequest>(), fields::CLIENT_IDENTITY];
    let (req, ignored): (CrawlRequest, _) = lenient_body(req, &known)?;
    check_crawl_request(&req, s.engine.as_ref())?;
    let start = Start {
        owner: caller.map(|Extension(c)| c),
        webhook: webhook(&s, req.webhook.as_ref())?,
    };
    let id = s
        .jobs
        .start_crawl_with(s.engine.clone(), req, start)
        .await?;
    let prefix = if uri.path().starts_with("/v1") {
        "/v1"
    } else {
        "/v2"
    };
    Ok(Json(JobStarted {
        success: true,
        url: format!("{}{prefix}/crawl/{id}", base_url(&s, &headers)),
        id,
        invalid_urls: None,
        warnings: fields::ignored_warning(&ignored).into_iter().collect(),
    }))
}

pub async fn jobs_active(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
) -> Json<serde_json::Value> {
    let ids = s
        .jobs
        .active_for(caller.as_ref().map(|Extension(c)| c))
        .await;
    Json(serde_json::json!({
        "success": true,
        "crawls": ids.into_iter().map(|id| serde_json::json!({ "id": id })).collect::<Vec<_>>(),
    }))
}

/// 爬取请求的校验（`/crawl` 和 crawl 型监控共用）。
pub(crate) fn check_crawl_request(
    req: &CrawlRequest,
    engine: &dyn crate::engine::Engine,
) -> Result<(), ApiError> {
    check_url(&req.url)?;
    if req.limit > MAX_CRAWL_LIMIT {
        return Err(ApiError::invalid(format!(
            "limit must not exceed {MAX_CRAWL_LIMIT}"
        )));
    }
    let depths = [req.max_discovery_depth.map(|d| d as u64), req.max_depth];
    if depths.into_iter().flatten().any(|d| d > MAX_CRAWL_DEPTH) {
        return Err(ApiError::invalid(format!(
            "maxDiscoveryDepth/maxDepth must not exceed {MAX_CRAWL_DEPTH}"
        )));
    }
    check_scrape_options(&req.scrape_options, engine)?;
    if let Some(g) = &req.graph {
        crate::types::graph::check_graph_options(g)?;
    }
    for pattern in req.include_paths.iter().chain(&req.exclude_paths) {
        regex_check(pattern)?;
    }
    Ok(())
}

pub fn regex_check(pattern: &str) -> Result<(), ApiError> {
    regex::RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .build()
        .map(|_| ())
        .map_err(|e| ApiError::invalid(format!("invalid path pattern {pattern:?}: {e}")))
}
