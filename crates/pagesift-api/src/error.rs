//! 错误码和 HTTP 状态码（PRD 6.5.2）。响应体沿用 Firecrawl 的 `{success: false, code, error}`，
//! 状态码向 Firecrawl 的同类错误看齐，官方 SDK 抛出的异常类型因此不变（比如超时是 408）。

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    RobotsDisallowed,
    AccessBarrier,
    RateLimited,
    HostCooldown,
    /// 调用方超过了自己 API key 的每分钟请求数。
    RateLimitExceeded,
    /// 调用方同时进行的任务数到了上限。
    ConcurrencyLimitExceeded,
    /// 调用方当天的页面配额用完了。
    QuotaExceeded,
    Timeout,
    SsrfBlocked,
    TooLarge,
    UpstreamError,
    UnsupportedOption,
    InvalidRequest,
    Unauthorized,
    NotFound,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::RobotsDisallowed => "ROBOTS_DISALLOWED",
            ErrorCode::AccessBarrier => "ACCESS_BARRIER",
            ErrorCode::RateLimited => "RATE_LIMITED",
            ErrorCode::HostCooldown => "HOST_COOLDOWN",
            ErrorCode::RateLimitExceeded => "RATE_LIMIT_EXCEEDED",
            ErrorCode::ConcurrencyLimitExceeded => "CONCURRENCY_LIMIT_EXCEEDED",
            ErrorCode::QuotaExceeded => "QUOTA_EXCEEDED",
            ErrorCode::Timeout => "TIMEOUT",
            ErrorCode::SsrfBlocked => "SSRF_BLOCKED",
            ErrorCode::TooLarge => "TOO_LARGE",
            ErrorCode::UpstreamError => "UPSTREAM_ERROR",
            ErrorCode::UnsupportedOption => "UNSUPPORTED_OPTION",
            ErrorCode::InvalidRequest => "INVALID_REQUEST",
            ErrorCode::Unauthorized => "UNAUTHORIZED",
            ErrorCode::NotFound => "NOT_FOUND",
            ErrorCode::Internal => "INTERNAL_ERROR",
        }
    }

    pub fn status(self) -> StatusCode {
        match self {
            ErrorCode::RobotsDisallowed | ErrorCode::AccessBarrier => StatusCode::FORBIDDEN,
            ErrorCode::RateLimited
            | ErrorCode::RateLimitExceeded
            | ErrorCode::ConcurrencyLimitExceeded
            | ErrorCode::QuotaExceeded => StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::HostCooldown => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Timeout => StatusCode::REQUEST_TIMEOUT,
            ErrorCode::SsrfBlocked | ErrorCode::UnsupportedOption | ErrorCode::InvalidRequest => {
                StatusCode::BAD_REQUEST
            }
            ErrorCode::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::UpstreamError => StatusCode::BAD_GATEWAY,
            ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
            ErrorCode::NotFound => StatusCode::NOT_FOUND,
            ErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    /// 放进响应的 `details`，比如拦截页的厂商和置信度。
    pub details: Option<Value>,
    /// 429 时带上 `Retry-After`（秒）。
    pub retry_after: Option<u64>,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
            retry_after: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn with_retry_after(mut self, secs: u64) -> Self {
        self.retry_after = Some(secs);
        self
    }

    pub fn unsupported(what: impl Into<String>) -> Self {
        Self::new(ErrorCode::UnsupportedOption, what)
    }

    pub fn invalid(what: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidRequest, what)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({
            "success": false,
            "code": self.code.as_str(),
            "error": self.message,
        });
        if let Some(d) = self.details {
            body["details"] = d;
        }
        let mut resp = (self.code.status(), Json(body)).into_response();
        // 给监控中间件按错误码计数
        resp.extensions_mut().insert(self.code);
        if let Some(secs) = self.retry_after
            && let Ok(v) = secs.to_string().parse()
        {
            resp.headers_mut().insert("retry-after", v);
        }
        resp
    }
}
