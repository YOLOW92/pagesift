//! HTTP 路由：Firecrawl v2 兼容接口挂在根路径 `/v2/*`，另有 `/healthz` 和 `/metrics`。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, Query, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use pagesift_store::Store;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::engine::Engine;
use crate::error::{ApiError, ErrorCode};
use crate::jobs::{Jobs, Start, rfc3339};
use crate::limits::{Caller, KeyLimits, Limiter};
use crate::metrics::{Metrics, PageSource};
use crate::types::{
    BatchScrapeRequest, CrawlRequest, ExtractRequest, JobErrors, JobStarted, JobStatus, MapRequest,
    MapResponse, ScrapeRequest, ScrapeResponse, SearchRequest,
};
use crate::validate::{check_scrape_options, check_url};
use crate::webhook::{WebhookConfig, WebhookSettings, Webhooks};

/// 任务状态分页时每页最多的文档数。
const PAGE_SIZE: usize = 100;
const DEFAULT_BATCH_CONCURRENCY: usize = 8;

#[derive(Clone, Default)]
pub struct Config {
    /// 为空时不鉴权；否则要求 `Authorization: Bearer <key>`。
    pub api_keys: Vec<String>,
    /// 对外地址，用来生成任务的绝对 URL 和分页的 `next`；不设时用请求的 Host 头。
    pub public_url: Option<String>,
    pub max_batch_concurrency: Option<usize>,
    /// 任务落盘用的存储：重启后结束的任务照样能查，进行中的从断点继续。
    /// 不设时任务只在内存里。
    pub store: Option<Store>,
    /// 单独设了限额的 API key（它们也是有效的 key）。
    pub key_limits: HashMap<String, KeyLimits>,
    /// 其余 key 的限额；不鉴权时所有调用方合用这一份。默认不限。
    pub default_limits: KeyLimits,
    pub webhook: WebhookSettings,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 不打印 key 本身
        f.debug_struct("Config")
            .field("api_keys", &self.api_keys.len())
            .field("public_url", &self.public_url)
            .field("max_batch_concurrency", &self.max_batch_concurrency)
            .field("store", &self.store.is_some())
            .field("key_limits", &self.key_limits.len())
            .field("default_limits", &self.default_limits)
            .field("webhook", &self.webhook)
            .finish()
    }
}

#[derive(Clone)]
pub struct AppState {
    engine: Arc<dyn Engine>,
    /// key → 限额。为空时不鉴权。
    keys: Arc<HashMap<String, Arc<KeyLimits>>>,
    default_limits: Arc<KeyLimits>,
    public_url: Option<String>,
    batch_concurrency: usize,
    jobs: Jobs,
    limiter: Arc<Limiter>,
    webhooks: Webhooks,
    extracts: Arc<Mutex<std::collections::HashMap<String, ExtractJob>>>,
    metrics: Arc<Metrics>,
}

struct ExtractJob {
    status: &'static str,
    data: Option<serde_json::Value>,
    error: Option<ApiError>,
    created: std::time::SystemTime,
}

/// 组装路由。给了 [`Config::store`] 时会恢复存储里的任务，需要在 tokio 运行时里调用。
pub fn router(engine: Arc<dyn Engine>, config: Config) -> Router {
    let default_limits = Arc::new(config.default_limits);
    let mut keys: HashMap<String, Arc<KeyLimits>> = config
        .api_keys
        .into_iter()
        .map(|k| (k, default_limits.clone()))
        .collect();
    keys.extend(config.key_limits.into_iter().map(|(k, l)| (k, Arc::new(l))));
    // 恢复任务时按调用方名字找回限额
    let accounts: HashMap<String, Arc<KeyLimits>> = keys
        .iter()
        .map(|(k, l)| {
            (
                Caller::new(Some(k), l.clone()).account.to_string(),
                l.clone(),
            )
        })
        .collect();
    let metrics = Arc::new(Metrics::default());
    let limiter = Arc::new(Limiter::new(config.store.clone()));
    let webhooks = Webhooks::new(config.webhook, metrics.clone());
    let jobs = Jobs::new(
        config.store,
        limiter.clone(),
        Some(webhooks.clone()),
        metrics.clone(),
    );
    if tokio::runtime::Handle::try_current().is_ok() {
        let fallback = default_limits.clone();
        jobs.restore(&engine, &|account| {
            accounts
                .get(account)
                .cloned()
                .unwrap_or_else(|| fallback.clone())
        });
    } else {
        tracing::warn!("router built outside a tokio runtime: saved jobs are not resumed");
    }
    let state = AppState {
        engine,
        keys: Arc::new(keys),
        default_limits,
        public_url: config
            .public_url
            .map(|u| u.trim_end_matches('/').to_string()),
        batch_concurrency: config
            .max_batch_concurrency
            .unwrap_or(DEFAULT_BATCH_CONCURRENCY),
        jobs,
        limiter,
        webhooks,
        extracts: Arc::default(),
        metrics,
    };
    let v2 = Router::new()
        .route("/v2/scrape", post(scrape))
        .route("/v2/batch/scrape", post(batch_start))
        .route("/v2/batch/scrape/{id}", get(job_status).delete(job_cancel))
        .route("/v2/batch/scrape/{id}/errors", get(job_errors))
        .route("/v2/map", post(map))
        .route("/v2/search", post(search))
        .route("/v2/extract", post(extract_start))
        .route("/v2/extract/{id}", get(extract_status))
        .route("/v2/crawl", post(crawl_start))
        .route("/v2/crawl/active", get(jobs_active))
        .route("/v2/crawl/{id}", get(job_status).delete(job_cancel))
        .route("/v2/crawl/{id}/errors", get(job_errors))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/metrics", get(render_metrics))
        .merge(v2)
        .fallback(|| async { ApiError::new(ErrorCode::NotFound, "no such endpoint") })
        .layer(middleware::from_fn_with_state(state.clone(), observe))
        .with_state(state)
}

/// 鉴权并认出调用方；发起工作的请求（POST）按调用方的每分钟请求数限流。
/// 查询状态、取消这类轮询请求不计数（Firecrawl 对它们的限额也宽得多）。
async fn auth(
    State(s): State<AppState>,
    headers: HeaderMap,
    mut req: Request,
    next: Next,
) -> Response {
    let caller = if s.keys.is_empty() {
        Caller::new(None, s.default_limits.clone())
    } else {
        let key = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim);
        match key.and_then(|k| s.keys.get(k).map(|l| (k, l))) {
            Some((k, l)) => Caller::new(Some(k), l.clone()),
            None => {
                return ApiError::new(ErrorCode::Unauthorized, "missing or invalid API key")
                    .into_response();
            }
        }
    };
    if req.method() == Method::POST
        && let Err(e) = s.limiter.check_rate(&caller)
    {
        let mut resp = e.into_response();
        resp.extensions_mut().insert(caller);
        return resp;
    }
    req.extensions_mut().insert(caller.clone());
    let mut resp = next.run(req).await;
    resp.extensions_mut().insert(caller);
    resp
}

/// 每个请求的监控指标和结构化日志：按路由的请求数和延迟、在途请求数、按错误码的错误数。
async fn observe(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "unmatched".into());
    let method = req.method().clone();
    let started = Instant::now();
    s.metrics.request_started();
    let resp = next.run(req).await;
    let elapsed = started.elapsed();
    let status = resp.status().as_u16();
    s.metrics.request_finished(&route, status, elapsed);
    let code = resp.extensions().get::<ErrorCode>().map(|c| c.as_str());
    if let Some(code) = code {
        s.metrics.error(code, "http");
    }
    let account = resp.extensions().get::<Caller>().map(|c| c.account.clone());
    let latency_ms = elapsed.as_secs_f64() * 1000.0;
    if route.starts_with("/v2/") {
        tracing::info!(
            target: "pagesift::http",
            %method, %route, status, latency_ms, account = account.as_deref(), code,
            "request"
        );
    } else {
        tracing::debug!(target: "pagesift::http", %method, %route, status, latency_ms, "request");
    }
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
    caller: Option<Extension<Caller>>,
    req: Result<Json<ScrapeRequest>, JsonRejection>,
) -> Result<Json<ScrapeResponse>, ApiError> {
    let req = body(req)?;
    check_url(&req.url)?;
    check_scrape_options(&req.options, s.engine.as_ref())?;
    if let Some(Extension(c)) = &caller {
        s.limiter.check_quota(c)?;
    }
    match s.engine.scrape(&req.url, &req.options).await {
        Ok(data) => {
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

async fn batch_start(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
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

async fn job_errors(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<JobErrors>, ApiError> {
    s.jobs
        .errors(&id)
        .await
        .map(|(errors, robots_blocked)| {
            Json(JobErrors {
                errors,
                robots_blocked,
            })
        })
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

async fn search(
    State(s): State<AppState>,
    req: Result<Json<SearchRequest>, JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let req = body(req)?;
    if req.query.trim().is_empty() {
        return Err(ApiError::invalid("query must not be empty"));
    }
    if !(1..=100).contains(&req.limit) {
        return Err(ApiError::invalid("limit must be between 1 and 100"));
    }
    if let Some(o) = &req.scrape_options {
        check_scrape_options(o, s.engine.as_ref())?;
    }
    let data = s.engine.search(&req).await?;
    let credits = data["web"].as_array().map_or(0, Vec::len);
    Ok(Json(serde_json::json!({
        "success": true,
        "data": data,
        "creditsUsed": credits,
        "id": uuid::Uuid::new_v4().to_string(),
    })))
}

async fn extract_start(
    State(s): State<AppState>,
    req: Result<Json<ExtractRequest>, JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let req = body(req)?;
    if req.urls.is_empty() {
        return Err(ApiError::invalid("urls must not be empty"));
    }
    if req.prompt.is_none() && req.schema.is_none() {
        return Err(ApiError::invalid("extract needs a prompt or a schema"));
    }
    if req.enable_web_search {
        return Err(ApiError::unsupported(
            "enableWebSearch is not available in this build",
        ));
    }
    for u in &req.urls {
        if let Err(e) = check_url(u.trim_end_matches("/*"))
            && !req.ignore_invalid_urls
        {
            return Err(e);
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    {
        let mut map = s.extracts.lock().await;
        let now = std::time::SystemTime::now();
        map.retain(|_, j| j.created + crate::jobs::JOB_TTL > now);
        map.insert(
            id.clone(),
            ExtractJob {
                status: "processing",
                data: None,
                error: None,
                created: now,
            },
        );
    }
    let (engine, extracts, job_id) = (s.engine.clone(), s.extracts.clone(), id.clone());
    tokio::spawn(async move {
        let result = engine.extract(&req).await;
        if let Some(job) = extracts.lock().await.get_mut(&job_id) {
            match result {
                Ok(v) => {
                    job.status = "completed";
                    job.data = Some(v);
                }
                Err(e) => {
                    job.status = "failed";
                    job.error = Some(e);
                }
            }
        }
    });
    Ok(Json(serde_json::json!({ "success": true, "id": id })))
}

async fn extract_status(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let map = s.extracts.lock().await;
    let job = map
        .get(&id)
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("extract job {id} not found")))?;
    let mut body = serde_json::json!({
        "success": job.error.is_none(),
        "status": job.status,
        "expiresAt": rfc3339(job.created + crate::jobs::JOB_TTL),
    });
    if let Some(d) = &job.data {
        body["data"] = d.clone();
    }
    if let Some(e) = &job.error {
        body["error"] = format!("{}: {}", e.code.as_str(), e.message).into();
    }
    Ok(Json(body))
}

async fn crawl_start(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
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
    Ok(Json(JobStarted {
        success: true,
        url: format!("{}/v2/crawl/{id}", base_url(&s, &headers)),
        id,
        invalid_urls: None,
    }))
}

/// 请求里的 `webhook`：解析并做 SSRF 校验。
fn webhook(s: &AppState, v: Option<&serde_json::Value>) -> Result<Option<WebhookConfig>, ApiError> {
    v.filter(|v| !v.is_null())
        .map(|v| WebhookConfig::parse(v, s.webhooks.ssrf()))
        .transpose()
}

/// includePaths 和 excludePaths 是正则；写错时在启动任务前就报错。
fn regex_check(pattern: &str) -> Result<(), ApiError> {
    regex::RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .build()
        .map(|_| ())
        .map_err(|e| ApiError::invalid(format!("invalid path pattern {pattern:?}: {e}")))
}

async fn render_metrics(State(s): State<AppState>) -> impl IntoResponse {
    let out = s.metrics.render(&s.jobs.counts().await);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        out,
    )
}
