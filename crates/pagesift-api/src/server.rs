//! HTTP server routing: Firecrawl v1/v2 compatibility layers, Jina Reader adapter,
//! health checks, and Prometheus metrics.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use pagesift_store::Store;
use tokio::sync::Mutex;

use crate::engine::Engine;
use crate::error::{ApiError, ErrorCode};
use crate::jobs::Jobs;
use crate::limits::{Caller, KeyLimits, Limiter};
use crate::metrics::Metrics;
use crate::webhook::{WebhookConfig, WebhookSettings, Webhooks};

/// Maximum documents per page when querying job status.
pub const PAGE_SIZE: usize = 100;
pub const DEFAULT_BATCH_CONCURRENCY: usize = 8;

#[derive(Clone, Default)]
pub struct Config {
    /// When empty, authentication is disabled; otherwise requires `Authorization: Bearer <key>`.
    pub api_keys: Vec<String>,
    /// Public URL for building absolute resource links.
    pub public_url: Option<String>,
    pub max_batch_concurrency: Option<usize>,
    /// Persistent store for jobs and resume capability.
    pub store: Option<Store>,
    /// Per-key custom limits.
    pub key_limits: HashMap<String, KeyLimits>,
    /// Default limits applied to other keys or unauthenticated callers.
    pub default_limits: KeyLimits,
    pub webhook: WebhookSettings,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
    pub engine: Arc<dyn Engine>,
    pub keys: Arc<HashMap<String, Arc<KeyLimits>>>,
    pub default_limits: Arc<KeyLimits>,
    pub public_url: Option<String>,
    pub batch_concurrency: usize,
    pub jobs: Jobs,
    pub limiter: Arc<Limiter>,
    pub webhooks: Webhooks,
    pub extracts: Arc<Mutex<HashMap<String, ExtractJob>>>,
    pub metrics: Arc<Metrics>,
}

pub struct ExtractJob {
    pub status: &'static str,
    pub data: Option<serde_json::Value>,
    pub error: Option<ApiError>,
    pub created: std::time::SystemTime,
}

/// Construct complete HTTP router with Firecrawl v1/v2, Jina Reader, and metrics.
pub fn router(engine: Arc<dyn Engine>, config: Config) -> Router {
    let default_limits = Arc::new(config.default_limits);
    let mut keys: HashMap<String, Arc<KeyLimits>> = config
        .api_keys
        .into_iter()
        .map(|k| (k, default_limits.clone()))
        .collect();
    keys.extend(config.key_limits.into_iter().map(|(k, l)| (k, Arc::new(l))));

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
        engine: engine.clone(),
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

    let firecrawl = crate::firecrawl::firecrawl_routes()
        .route_layer(middleware::from_fn_with_state(state.clone(), auth));
    let jina = crate::jina::jina_router(engine);

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/metrics", get(render_metrics))
        .merge(firecrawl)
        .with_state(state.clone())
        .merge(jina)
        .fallback(|| async { ApiError::new(ErrorCode::NotFound, "no such endpoint") })
        .layer(middleware::from_fn_with_state(state.clone(), observe))
}

/// Authentication and rate limiting middleware.
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

/// Metrics and structured request tracing middleware.
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
    if route.starts_with("/v2/") || route.starts_with("/v1/") || route.starts_with("/r") {
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

pub fn body<T>(r: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    r.map(|Json(v)| v)
        .map_err(|e| ApiError::invalid(format!("invalid request body: {}", e.body_text())))
}

pub fn base_url(s: &AppState, headers: &HeaderMap) -> String {
    if let Some(u) = &s.public_url {
        return u.clone();
    }
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost:3002");
    format!("http://{host}")
}

pub fn webhook(s: &AppState, v: Option<&serde_json::Value>) -> Result<Option<WebhookConfig>, ApiError> {
    v.filter(|v| !v.is_null())
        .map(|v| WebhookConfig::parse(v, s.webhooks.ssrf()))
        .transpose()
}

async fn render_metrics(State(s): State<AppState>) -> impl IntoResponse {
    let out = s.metrics.render(&s.jobs.counts().await);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        out,
    )
}
