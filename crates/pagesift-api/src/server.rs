//! HTTP 路由：Firecrawl v2 兼容接口挂在根路径 `/v2/*`，另有 `/healthz` 和 `/metrics`。

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::engine::Engine;
use crate::error::{ApiError, ErrorCode};
use crate::jobs::{Jobs, rfc3339};
use crate::types::{
    BatchScrapeRequest, CrawlRequest, JobErrors, JobStarted, JobStatus, MapRequest, MapResponse,
    ScrapeRequest, ScrapeResponse,
};
use crate::validate::{check_scrape_options, check_url};

/// 任务状态分页时每页最多的文档数。
const PAGE_SIZE: usize = 100;
const DEFAULT_BATCH_CONCURRENCY: usize = 8;

#[derive(Debug, Clone, Default)]
pub struct Config {
    /// 为空时不鉴权；否则要求 `Authorization: Bearer <key>`。
    pub api_keys: Vec<String>,
    /// 对外地址，用来生成任务的绝对 URL 和分页的 `next`；不设时用请求的 Host 头。
    pub public_url: Option<String>,
    pub max_batch_concurrency: Option<usize>,
}

#[derive(Clone)]
pub struct AppState {
    engine: Arc<dyn Engine>,
    keys: Arc<HashSet<String>>,
    public_url: Option<String>,
    batch_concurrency: usize,
    jobs: Jobs,
    metrics: Arc<Metrics>,
}

#[derive(Default)]
struct Metrics {
    requests: Mutex<BTreeMap<(String, u16), u64>>,
    scrape_ok: AtomicU64,
    scrape_err: AtomicU64,
}

pub fn router(engine: Arc<dyn Engine>, config: Config) -> Router {
    let state = AppState {
        engine,
        keys: Arc::new(config.api_keys.into_iter().collect()),
        public_url: config
            .public_url
            .map(|u| u.trim_end_matches('/').to_string()),
        batch_concurrency: config
            .max_batch_concurrency
            .unwrap_or(DEFAULT_BATCH_CONCURRENCY),
        jobs: Jobs::default(),
        metrics: Arc::new(Metrics::default()),
    };
    let v2 = Router::new()
        .route("/v2/scrape", post(scrape))
        .route("/v2/batch/scrape", post(batch_start))
        .route("/v2/batch/scrape/{id}", get(job_status).delete(job_cancel))
        .route("/v2/batch/scrape/{id}/errors", get(job_errors))
        .route("/v2/map", post(map))
        .route("/v2/crawl", post(crawl_start))
        .route("/v2/crawl/active", get(jobs_active))
        .route("/v2/crawl/{id}", get(job_status).delete(job_cancel))
        .route("/v2/crawl/{id}/errors", get(job_errors))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/metrics", get(metrics))
        .merge(v2)
        .fallback(|| async { ApiError::new(ErrorCode::NotFound, "no such endpoint") })
        .layer(middleware::from_fn_with_state(state.clone(), count))
        .with_state(state)
}

async fn auth(State(s): State<AppState>, headers: HeaderMap, req: Request, next: Next) -> Response {
    if s.keys.is_empty() {
        return next.run(req).await;
    }
    let key = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    match key {
        Some(k) if s.keys.contains(k) => next.run(req).await,
        _ => ApiError::new(ErrorCode::Unauthorized, "missing or invalid API key").into_response(),
    }
}

async fn count(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "unmatched".into());
    let resp = next.run(req).await;
    *s.metrics
        .requests
        .lock()
        .await
        .entry((route, resp.status().as_u16()))
        .or_default() += 1;
    resp
}

/// JSON 解析失败时也返回统一的错误格式。
fn body<T>(r: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    r.map(|Json(v)| v)
        .map_err(|e| ApiError::invalid(format!("invalid request body: {}", e.body_text())))
}

fn base_url(s: &AppState, headers: &HeaderMap) -> String {
    if let Some(u) = &s.public_url {
        return u.clone();
    }
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost:3002");
    format!("http://{host}")
}

async fn scrape(
    State(s): State<AppState>,
    req: Result<Json<ScrapeRequest>, JsonRejection>,
) -> Result<Json<ScrapeResponse>, ApiError> {
    let req = body(req)?;
    check_url(&req.url)?;
    check_scrape_options(&req.options, s.engine.as_ref())?;
    match s.engine.scrape(&req.url, &req.options).await {
        Ok(data) => {
            s.metrics.scrape_ok.fetch_add(1, Ordering::Relaxed);
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

async fn batch_start(
    State(s): State<AppState>,
    headers: HeaderMap,
    req: Result<Json<BatchScrapeRequest>, JsonRejection>,
) -> Result<Json<JobStarted>, ApiError> {
    let req = body(req)?;
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
    if req.webhook.is_some() {
        return Err(ApiError::unsupported(
            "webhooks are not available in this build",
        ));
    }
    let concurrency = req
        .max_concurrency
        .unwrap_or(s.batch_concurrency)
        .min(s.batch_concurrency);
    let id = s
        .jobs
        .start_batch(s.engine.clone(), valid, req.options, concurrency)
        .await;
    Ok(Json(JobStarted {
        success: true,
        url: format!("{}/v2/batch/scrape/{id}", base_url(&s, &headers)),
        id,
        invalid_urls: req.ignore_invalid_urls.then_some(invalid),
    }))
}

#[derive(Deserialize)]
struct Paging {
    #[serde(default)]
    skip: usize,
}

async fn job_status(
    State(s): State<AppState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    Path(id): Path<String>,
    Query(p): Query<Paging>,
) -> Result<Json<JobStatus>, ApiError> {
    let base = base_url(&s, &headers);
    let path = uri.path().to_string();
    s.jobs
        .get(&id, |job| {
            let end = (p.skip + PAGE_SIZE).min(job.data.len());
            let data = job
                .data
                .get(p.skip.min(end)..end)
                .unwrap_or_default()
                .to_vec();
            let more =
                end < job.data.len() || (job.state.as_str() == "scraping" && end < job.total);
            JobStatus {
                success: true,
                status: job.state.as_str().into(),
                completed: job.completed,
                total: job.total,
                credits_used: job.completed,
                expires_at: rfc3339(job.expires_at()),
                next: (more && end > p.skip).then(|| format!("{base}{path}?skip={end}")),
                data,
            }
        })
        .await
        .map(Json)
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("job {id} not found")))
}

async fn job_errors(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<JobErrors>, ApiError> {
    s.jobs
        .get(&id, |job| JobErrors {
            errors: job.errors.clone(),
            robots_blocked: job.robots_blocked.clone(),
        })
        .await
        .map(Json)
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("job {id} not found")))
}

async fn job_cancel(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    if s.jobs.cancel(&id).await {
        Ok(Json(serde_json::json!({ "success": true, "status": "cancelled" })).into_response())
    } else {
        Err(ApiError::new(
            ErrorCode::NotFound,
            format!("job {id} not found"),
        ))
    }
}

async fn jobs_active(State(s): State<AppState>) -> Json<serde_json::Value> {
    let ids = s.jobs.active().await;
    Json(serde_json::json!({
        "success": true,
        "crawls": ids.into_iter().map(|id| serde_json::json!({ "id": id })).collect::<Vec<_>>(),
    }))
}

async fn map(
    State(s): State<AppState>,
    req: Result<Json<MapRequest>, JsonRejection>,
) -> Result<Json<MapResponse>, ApiError> {
    let req = body(req)?;
    check_url(&req.url)?;
    let links = s.engine.map(&req).await?;
    Ok(Json(MapResponse {
        success: true,
        links,
    }))
}

async fn crawl_start(
    State(s): State<AppState>,
    headers: HeaderMap,
    req: Result<Json<CrawlRequest>, JsonRejection>,
) -> Result<Json<JobStarted>, ApiError> {
    let req = body(req)?;
    check_url(&req.url)?;
    check_scrape_options(&req.scrape_options, s.engine.as_ref())?;
    if req.webhook.is_some() {
        return Err(ApiError::unsupported(
            "webhooks are not available in this build",
        ));
    }
    for pattern in req.include_paths.iter().chain(&req.exclude_paths) {
        regex_check(pattern)?;
    }
    let id = s.jobs.start_crawl(s.engine.clone(), req).await;
    Ok(Json(JobStarted {
        success: true,
        url: format!("{}/v2/crawl/{id}", base_url(&s, &headers)),
        id,
        invalid_urls: None,
    }))
}

/// includePaths 和 excludePaths 是正则；写错时在启动任务前就报错。
fn regex_check(pattern: &str) -> Result<(), ApiError> {
    regex::RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .build()
        .map(|_| ())
        .map_err(|e| ApiError::invalid(format!("invalid path pattern {pattern:?}: {e}")))
}

async fn metrics(State(s): State<AppState>) -> impl IntoResponse {
    let mut out = String::from(
        "# HELP pagesift_http_requests_total HTTP requests by route and status.\n# TYPE pagesift_http_requests_total counter\n",
    );
    for ((route, status), n) in s.metrics.requests.lock().await.iter() {
        out.push_str(&format!(
            "pagesift_http_requests_total{{route=\"{route}\",status=\"{status}\"}} {n}\n"
        ));
    }
    out.push_str("# HELP pagesift_scrapes_total Single-page scrapes by outcome.\n# TYPE pagesift_scrapes_total counter\n");
    out.push_str(&format!(
        "pagesift_scrapes_total{{outcome=\"ok\"}} {}\npagesift_scrapes_total{{outcome=\"error\"}} {}\n",
        s.metrics.scrape_ok.load(Ordering::Relaxed),
        s.metrics.scrape_err.load(Ordering::Relaxed)
    ));
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        out,
    )
}
