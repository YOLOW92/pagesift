use std::time::Duration;

pub type Result<T> = std::result::Result<T, LlmError>;

/// 错误分类：调用方据此决定是否重试、换 key 或直接报错。
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("配置无效: {0}")]
    Config(String),
    /// 401 / 403。
    #[error("鉴权失败 (HTTP {status}): {message}")]
    Auth { status: u16, message: String },
    /// 429，重试次数用完后仍被限流。
    #[error("被限流")]
    RateLimited { retry_after: Option<Duration> },
    #[error("请求超时")]
    Timeout,
    /// 5xx 或其他非预期状态码。
    #[error("上游出错 (HTTP {status}): {message}")]
    Upstream { status: u16, message: String },
    /// 连接失败等传输层错误。
    #[error("网络错误: {0}")]
    Network(String),
    #[error("上游响应无法解析: {0}")]
    InvalidResponse(String),
    /// 模型输出不满足 schema（已按规则重试仍失败）。
    #[error("输出不满足 schema: {0}")]
    SchemaViolation(String),
}

impl LlmError {
    /// 是否值得稍后重试。
    pub fn is_retryable(&self) -> bool {
        match self {
            LlmError::RateLimited { .. } | LlmError::Timeout | LlmError::Network(_) => true,
            LlmError::Upstream { status, .. } => *status >= 500,
            _ => false,
        }
    }
}
