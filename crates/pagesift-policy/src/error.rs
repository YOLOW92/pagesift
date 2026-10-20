//! 统一错误码（PRD 6.5.2）。

use std::time::Duration;

use pagesift_net::{Blocked, TimeoutPhase};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorCode {
    RobotsDisallowed,
    AccessBarrier,
    RateLimited,
    HostCooldown,
    Timeout,
    SsrfBlocked,
    TooLarge,
    UpstreamError,
    UnsupportedOption,
    InvalidRequest,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RobotsDisallowed => "ROBOTS_DISALLOWED",
            Self::AccessBarrier => "ACCESS_BARRIER",
            Self::RateLimited => "RATE_LIMITED",
            Self::HostCooldown => "HOST_COOLDOWN",
            Self::Timeout => "TIMEOUT",
            Self::SsrfBlocked => "SSRF_BLOCKED",
            Self::TooLarge => "TOO_LARGE",
            Self::UpstreamError => "UPSTREAM_ERROR",
            Self::UnsupportedOption => "UNSUPPORTED_OPTION",
            Self::InvalidRequest => "INVALID_REQUEST",
        }
    }

    pub fn http_status(self) -> u16 {
        match self {
            Self::RobotsDisallowed | Self::AccessBarrier => 403,
            Self::RateLimited => 429,
            Self::HostCooldown => 503,
            Self::Timeout => 408,
            Self::TooLarge => 413,
            Self::UpstreamError => 502,
            Self::SsrfBlocked | Self::UnsupportedOption | Self::InvalidRequest => 400,
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("disallowed by robots.txt: {url}")]
    RobotsDisallowed { url: String },
    #[error("rate limited by the target site")]
    RateLimited { retry_after: Option<Duration> },
    #[error("host `{host}` is cooling down")]
    HostCooldown { host: String, retry_after: Duration },
    #[error("{0} timeout")]
    Timeout(TimeoutPhase),
    #[error("target address not allowed: {0}")]
    SsrfBlocked(Blocked),
    #[error("response exceeds the size limit of {limit} bytes")]
    TooLarge { limit: usize },
    #[error("upstream error: {0}")]
    Upstream(String),
    #[error("unsupported option: {0}")]
    UnsupportedOption(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
}

impl Error {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::RobotsDisallowed { .. } => ErrorCode::RobotsDisallowed,
            Self::RateLimited { .. } => ErrorCode::RateLimited,
            Self::HostCooldown { .. } => ErrorCode::HostCooldown,
            Self::Timeout(_) => ErrorCode::Timeout,
            Self::SsrfBlocked(_) => ErrorCode::SsrfBlocked,
            Self::TooLarge { .. } => ErrorCode::TooLarge,
            Self::Upstream(_) => ErrorCode::UpstreamError,
            Self::UnsupportedOption(_) => ErrorCode::UnsupportedOption,
            Self::InvalidRequest(_) => ErrorCode::InvalidRequest,
        }
    }

    pub fn http_status(&self) -> u16 {
        self.code().http_status()
    }

    /// 给 `Retry-After` 响应头用。
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after } => *retry_after,
            Self::HostCooldown { retry_after, .. } => Some(*retry_after),
            _ => None,
        }
    }
}

impl From<pagesift_net::Error> for Error {
    fn from(e: pagesift_net::Error) -> Self {
        use pagesift_net::Error as N;
        match e {
            N::Blocked(b) => Self::SsrfBlocked(b),
            N::Timeout(p) => Self::Timeout(p),
            N::InvalidUrl(_) | N::UserAgent(_) | N::Header(_) => {
                Self::InvalidRequest(e.to_string())
            }
            // 重定向回调里返回的是本 crate 的错误（如 robots 禁止），原样取回。
            N::RedirectRejected { source, url } => match source.downcast::<Error>() {
                Ok(inner) => *inner,
                Err(other) => Self::Upstream(format!("redirect to {url} rejected: {other}")),
            },
            other => Self::Upstream(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_prd_table() {
        let table = [
            (ErrorCode::RobotsDisallowed, "ROBOTS_DISALLOWED", 403),
            (ErrorCode::AccessBarrier, "ACCESS_BARRIER", 403),
            (ErrorCode::RateLimited, "RATE_LIMITED", 429),
            (ErrorCode::HostCooldown, "HOST_COOLDOWN", 503),
            (ErrorCode::Timeout, "TIMEOUT", 408),
            (ErrorCode::SsrfBlocked, "SSRF_BLOCKED", 400),
            (ErrorCode::TooLarge, "TOO_LARGE", 413),
            (ErrorCode::UpstreamError, "UPSTREAM_ERROR", 502),
            (ErrorCode::UnsupportedOption, "UNSUPPORTED_OPTION", 400),
            (ErrorCode::InvalidRequest, "INVALID_REQUEST", 400),
        ];
        for (code, s, status) in table {
            assert_eq!((code.as_str(), code.http_status()), (s, status));
        }
    }

    #[test]
    fn maps_net_errors() {
        use pagesift_net::Error as N;
        let cases: Vec<(N, ErrorCode)> = vec![
            (
                N::Blocked(Blocked::Scheme("ftp".into())),
                ErrorCode::SsrfBlocked,
            ),
            (N::Timeout(TimeoutPhase::Total), ErrorCode::Timeout),
            (N::InvalidUrl("x".into()), ErrorCode::InvalidRequest),
            (N::TooManyRedirects(5), ErrorCode::UpstreamError),
            (N::Network("reset".into()), ErrorCode::UpstreamError),
            (
                N::RedirectRejected {
                    url: "u".into(),
                    source: Box::new(Error::RobotsDisallowed { url: "u".into() }),
                },
                ErrorCode::RobotsDisallowed,
            ),
            (
                N::RedirectRejected {
                    url: "u".into(),
                    source: "other".into(),
                },
                ErrorCode::UpstreamError,
            ),
        ];
        for (n, code) in cases {
            assert_eq!(Error::from(n).code(), code);
        }
    }
}
