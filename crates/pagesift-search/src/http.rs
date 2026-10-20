use std::time::Duration;

use reqwest::header::RETRY_AFTER;
use serde_json::Value;

use crate::error::{Result, SearchError};

#[derive(Debug, Clone)]
pub struct HttpOptions {
    pub timeout: Duration,
}

impl Default for HttpOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
        }
    }
}

pub(crate) fn build_client(opts: &HttpOptions) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(opts.timeout)
        .build()
        .map_err(|e| SearchError::Config(e.to_string()))
}

fn map_transport(e: reqwest::Error) -> SearchError {
    if e.is_timeout() {
        SearchError::Timeout
    } else {
        SearchError::Network(e.to_string())
    }
}

/// 发送请求并按统一规则分类错误，成功时返回解析后的 JSON。
/// `quota_403` 为真时把 403 视为配额用尽（Google CSE 的用法）。
pub(crate) async fn send_json(rb: reqwest::RequestBuilder, quota_403: bool) -> Result<Value> {
    let resp = rb.send().await.map_err(map_transport)?;
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let text = resp.text().await.map_err(map_transport)?;
    let code = status.as_u16();
    if status.is_success() {
        return serde_json::from_str(&text)
            .map_err(|e| SearchError::InvalidResponse(e.to_string()));
    }
    let message: String = text.chars().take(500).collect();
    Err(match code {
        429 => SearchError::RateLimited { retry_after },
        403 if quota_403 && is_quota_message(&text) => SearchError::RateLimited { retry_after },
        401 | 403 => SearchError::Auth {
            status: code,
            message,
        },
        _ => SearchError::Upstream {
            status: code,
            message,
        },
    })
}

fn is_quota_message(body: &str) -> bool {
    [
        "rateLimitExceeded",
        "dailyLimitExceeded",
        "quotaExceeded",
        "userRateLimitExceeded",
    ]
    .iter()
    .any(|k| body.contains(k))
}

/// 取字符串字段，缺失时为空串。
pub(crate) fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_owned()
}

pub(crate) fn require_nonempty(name: &str, v: &str) -> Result<()> {
    if v.trim().is_empty() {
        Err(SearchError::Config(format!("{name} 不能为空")))
    } else {
        Ok(())
    }
}
