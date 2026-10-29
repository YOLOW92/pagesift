//! HTTP server routing: Firecrawl v1/v2 compatibility layers, Jina Reader adapter,
//! health checks, and Prometheus metrics.

use std::collections::HashMap;
use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes, HttpBody};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use http_body::{Frame, SizeHint};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use pagesift_store::Store;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::time::Sleep;

use crate::engine::Engine;
use crate::error::{ApiError, ErrorCode};
use crate::jobs::Jobs;
use crate::limits::{Caller, KeyLimits, Limiter};
use crate::metrics::Metrics;
use crate::webhook::{WebhookConfig, WebhookSettings, Webhooks};

/// Maximum documents per page when querying job status.
pub const PAGE_SIZE: usize = 100;
pub const DEFAULT_BATCH_CONCURRENCY: usize = 8;

/// 服务端超时，防 slowloris 和挂死的请求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// 读完请求头的时限（HTTP/1），超时直接断开连接。只在 [`serve_with`] 里生效。
    pub header_read: Duration,
    /// 从请求进来到读完请求体的总时限，不是两次读之间的间隔：每 5 秒滴 1 字节也会超时。
    pub body: Duration,
    /// `/v2/parse` 上传文件的请求体时限（同样是总时限）：50 MB 的文件 30 秒传完要 1.7 MB/s，
    /// 上传单独放宽。
    pub upload_body: Duration,
    /// 整个请求（到响应头发出为止）的时限。要比同步 scrape 的 `timeout` 上限大，
    /// crawl/batch 只是创建任务，很快返回，不受影响。
    pub request: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            header_read: Duration::from_secs(30),
            body: Duration::from_secs(30),
            upload_body: Duration::from_secs(120),
            request: Duration::from_millis(crate::validate::MAX_SCRAPE_TIMEOUT_MS)
                + Duration::from_secs(60),
        }
    }
}

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
    /// 请求体和整请求超时；请求头超时由 [`serve_with`] 的参数决定。
    pub timeouts: Timeouts,
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
            .field("timeouts", &self.timeouts)
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
    let timeouts = config.timeouts;
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

    // Jina Reader 路由也会抓取页面，和 Firecrawl 路由一起放在鉴权、限流和配额之后
    let api = crate::firecrawl::firecrawl_routes()
        .merge(crate::jina::jina_routes())
        .route_layer(middleware::from_fn_with_state(state.clone(), auth));

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/metrics", get(render_metrics))
        .merge(api)
        .with_state(state.clone())
        .fallback(|| async { ApiError::new(ErrorCode::NotFound, "no such endpoint") })
        .layer(middleware::from_fn(uniform_errors))
        .layer(middleware::from_fn_with_state(timeouts, deadline))
        .layer(middleware::from_fn_with_state(state.clone(), observe))
}

/// 用默认超时在 `listener` 上提供服务，`shutdown` 完成后优雅退出。
pub async fn serve(
    listener: TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()> + Send,
) -> std::io::Result<()> {
    serve_with(listener, app, Timeouts::default(), shutdown).await
}

/// 和 `axum::serve(..).with_graceful_shutdown(..)` 行为相同，另外给 HTTP/1 设了请求头读取超时
/// （axum::serve 设不了）：收到 `shutdown` 后停止 accept，通知每个连接优雅关闭，等它们都结束再返回。
pub async fn serve_with(
    listener: TcpListener,
    app: Router,
    timeouts: Timeouts,
    shutdown: impl Future<Output = ()> + Send,
) -> std::io::Result<()> {
    let mut builder = auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(timeouts.header_read);
    let graceful = GracefulShutdown::new();
    let mut shutdown = pin!(shutdown);
    loop {
        let stream = tokio::select! {
            r = listener.accept() => match r {
                Ok((stream, _)) => stream,
                Err(e) => {
                    // 和 axum::serve 一样：单个连接的错误忽略，其他错误（比如文件描述符用完）歇 1 秒再接
                    if !matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionRefused
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::ConnectionReset
                    ) {
                        tracing::error!(error = %e, "accept failed");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        // 响应头和分块正文分两次写时，Nagle 会让第二次写等对端的延迟确认（约 40 ms）
        let _ = stream.set_nodelay(true);
        let conn = builder
            .serve_connection_with_upgrades(
                TokioIo::new(stream),
                TowerToHyperService::new(app.clone()),
            )
            .into_owned();
        let conn = graceful.watch(conn);
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::trace!(error = %e, "connection ended with an error");
            }
        });
    }
    drop(listener);
    graceful.shutdown().await;
    Ok(())
}

/// 请求体加总时限；整个请求加时限。超时都回统一的 JSON 错误（408 TIMEOUT）。
async fn deadline(State(t): State<Timeouts>, req: Request, next: Next) -> Response {
    let fired = Arc::new(AtomicBool::new(false));
    // 上传放宽的请求体时间也加到整请求时限上，解析仍有完整的 `timeout` 可用
    let (body_limit, request_limit) = if req.uri().path() == "/v2/parse" {
        (
            t.upload_body,
            t.request + t.upload_body.saturating_sub(t.body),
        )
    } else {
        (t.body, t.request)
    };
    let req = if req.body().is_end_stream() {
        req
    } else {
        let fired = fired.clone();
        req.map(|inner| {
            Body::new(DeadlineBody {
                inner,
                sleep: Box::pin(tokio::time::sleep(body_limit)),
                fired,
            })
        })
    };
    match tokio::time::timeout(request_limit, next.run(req)).await {
        // 读请求体超时时提取器给的是 400，这里换成 408
        Ok(_) if fired.load(Ordering::Relaxed) => ApiError::new(
            ErrorCode::Timeout,
            format!(
                "request body was not received within {} s",
                body_limit.as_secs_f64()
            ),
        )
        .into_response(),
        Ok(resp) => resp,
        Err(_) => ApiError::new(
            ErrorCode::Timeout,
            format!(
                "request did not complete within {} s",
                request_limit.as_secs_f64()
            ),
        )
        .into_response(),
    }
}

/// 到点还没读完就报错的请求体。
struct DeadlineBody {
    inner: Body,
    sleep: Pin<Box<Sleep>>,
    fired: Arc<AtomicBool>,
}

impl HttpBody for DeadlineBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let this = self.get_mut();
        if let Poll::Ready(frame) = Pin::new(&mut this.inner).poll_frame(cx) {
            return Poll::Ready(frame);
        }
        if this.sleep.as_mut().poll(cx).is_ready() {
            this.fired.store(true, Ordering::Relaxed);
            return Poll::Ready(Some(Err(axum::Error::new("request body timed out"))));
        }
        Poll::Pending
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// axum 自带的拒绝（查询参数、路径参数、JSON 解析失败、405 等）是纯文本或空 body，
/// 统一换成 `{success: false, code, error}`，状态码和 `Allow` 等响应头不变。
async fn uniform_errors(req: Request, next: Next) -> Response {
    let resp = next.run(req).await;
    let status = resp.status();
    // 自己的错误都带 ErrorCode 扩展，不用动
    if !status.is_client_error() || resp.extensions().get::<ErrorCode>().is_some() {
        return resp;
    }
    let plain = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .is_none_or(|v| v.as_bytes().starts_with(b"text/plain"));
    if !plain {
        return resp;
    }
    let (parts, body) = resp.into_parts();
    let text = axum::body::to_bytes(body, 64 * 1024)
        .await
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .unwrap_or_default();
    let code = match status {
        StatusCode::NOT_FOUND => ErrorCode::NotFound,
        StatusCode::PAYLOAD_TOO_LARGE => ErrorCode::TooLarge,
        StatusCode::UNAUTHORIZED => ErrorCode::Unauthorized,
        _ => ErrorCode::InvalidRequest,
    };
    let message = if text.is_empty() {
        status
            .canonical_reason()
            .unwrap_or("bad request")
            .to_lowercase()
    } else {
        text
    };
    let mut out = ApiError::new(code, message).into_response();
    *out.status_mut() = status;
    for (name, value) in &parts.headers {
        if name != header::CONTENT_TYPE && name != header::CONTENT_LENGTH {
            out.headers_mut().append(name, value.clone());
        }
    }
    out
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
    // 计入每分钟请求数的是会抓取的请求：POST，以及 Jina 的 `GET /r/{url}`；轮询类 GET 不算
    if (req.method() == Method::POST || req.uri().path().starts_with("/r/"))
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

pub fn webhook(
    s: &AppState,
    v: Option<&serde_json::Value>,
) -> Result<Option<WebhookConfig>, ApiError> {
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
