use std::time::Duration;

pub type Result<T> = std::result::Result<T, SearchError>;

/// 错误分类，与 pagesift-llm 的 `LlmError` 保持同一套口径。
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("配置无效: {0}")]
    Config(String),
    /// 请求本身无法被该后端表达（例如不支持的时间范围）。
    #[error("该后端不支持此请求: {0}")]
    Unsupported(String),
    /// 401 / 403（Google 的配额类 403 归入 RateLimited）。
    #[error("鉴权失败 (HTTP {status}): {message}")]
    Auth { status: u16, message: String },
    #[error("被限流或配额用尽")]
    RateLimited { retry_after: Option<Duration> },
    #[error("请求超时")]
    Timeout,
    #[error("上游出错 (HTTP {status}): {message}")]
    Upstream { status: u16, message: String },
    #[error("网络错误: {0}")]
    Network(String),
    #[error("上游响应无法解析: {0}")]
    InvalidResponse(String),
}

impl SearchError {
    pub fn is_retryable(&self) -> bool {
        match self {
            SearchError::RateLimited { .. } | SearchError::Timeout | SearchError::Network(_) => {
                true
            }
            SearchError::Upstream { status, .. } => *status >= 500,
            _ => false,
        }
    }
}
