//! 抓取的公开类型：错误、配置、请求、响应与流式响应。

use super::*;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutPhase {
    Dns,
    Connect,
    Tls,
    FirstByte,
    BodyIdle,
    Total,
}

impl fmt::Display for TimeoutPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Dns => "DNS",
            Self::Connect => "connect",
            Self::Tls => "TLS",
            Self::FirstByte => "first byte",
            Self::BodyIdle => "body idle",
            Self::Total => "total",
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    #[error("invalid configuration: {0}")]
    Config(String),
    #[error(transparent)]
    Session(#[from] crate::SessionError),
    #[error(transparent)]
    UserAgent(#[from] UserAgentError),
    #[error("header `{0}` is not allowed or malformed")]
    Header(String),
    #[error("blocked by SSRF protection: {0}")]
    Blocked(#[from] Blocked),
    #[error("{0} timeout")]
    Timeout(TimeoutPhase),
    #[error("too many redirects (limit {0})")]
    TooManyRedirects(usize),
    /// [`RedirectGuard`] 拒绝了某一跳；`source` 是回调返回的原始错误，可以向下转型。
    #[error("redirect to {url} rejected: {source}")]
    RedirectRejected {
        url: String,
        #[source]
        source: BoxError,
    },
    #[error("network error: {0}")]
    Network(String),
    /// 响应声明了压缩编码、开头也像该编码，但一个字节都解不出来。
    #[error("content decoding failed: {0}")]
    ContentDecoding(String),
    /// TLS 证书校验失败。类别是机器可读的，`message` 保留 rustls 的完整原因。
    /// 与 `Network` 分开：证书问题通常重试也不会好，调用方不该按网络抖动退避。
    #[error("TLS error ({kind}): {message}")]
    Tls {
        kind: crate::TlsErrorKind,
        message: String,
    },
    /// 域名解析失败或没有地址：临时问题，和 SSRF 拒绝（永远不许抓）分开。
    #[error("DNS lookup failed: {0}")]
    Dns(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// 每一跳重定向前调用，policy 层在这里做 robots 校验。SSRF 校验由 Fetcher 自己做。
pub trait RedirectGuard: Send + Sync {
    fn check<'a>(&'a self, from: &'a Url, to: &'a Url) -> BoxFuture<'a, Result<(), BoxError>>;
}

pub(super) struct NoGuard;

impl RedirectGuard for NoGuard {
    fn check<'a>(&'a self, _: &'a Url, _: &'a Url) -> BoxFuture<'a, Result<(), BoxError>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone)]
pub struct FetcherConfig {
    pub user_agent: String,
    pub connect_timeout: Duration,
    pub dns_timeout: Duration,
    pub tls_timeout: Duration,
    pub first_byte_timeout: Duration,
    pub body_idle_timeout: Duration,
    pub total_timeout: Duration,
    /// 调用方显式设过超时（`[fetch] timeout_secs`）。为真时自适应调优不得放宽这些值。
    pub timeouts_explicit: bool,
    pub max_redirects: usize,
    pub max_body_bytes: usize,
    pub ssrf: SsrfPolicy,
    /// 出口代理：`http://`、`https://` 或 `socks5://`。
    pub proxy: Option<String>,
    pub credentials: Vec<HostCredentials>,
    pub dns: Arc<dyn DnsLookup>,
    pub pool_idle_timeout: Duration,
    pub pool_max_idle_per_host: usize,
    /// Direct connections use a per-origin closed loop instead of one fixed
    /// concurrency limit for every host.
    pub adaptive_host_connections: bool,
    /// Safety ceiling for the adaptive per-origin hot-connection watermark.
    /// The controller starts small and reaches this only when measurements
    /// show that the origin remains saturated, fast and error-free.
    pub max_connections_per_host: usize,
    pub dns_positive_ttl: Duration,
    pub dns_negative_ttl: Duration,
    pub dns_transient_retries: usize,
    pub happy_eyeballs_delay: Duration,
    /// Requests whose TTFB crosses this boundary transfer from the main HTTP
    /// stage into the bounded tail stage until the body finishes.
    pub tail_threshold: Duration,
    pub stage_controller: StageControllerConfig,
}

impl Default for FetcherConfig {
    fn default() -> Self {
        Self {
            user_agent: DEFAULT_USER_AGENT.to_string(),
            connect_timeout: Duration::from_secs(10),
            dns_timeout: Duration::from_secs(5),
            tls_timeout: Duration::from_secs(10),
            first_byte_timeout: Duration::from_secs(30),
            body_idle_timeout: Duration::from_secs(15),
            total_timeout: Duration::from_secs(60),
            timeouts_explicit: false,
            max_redirects: 5,
            max_body_bytes: 15 * 1024 * 1024,
            ssrf: SsrfPolicy::default(),
            proxy: None,
            credentials: Vec::new(),
            dns: Arc::new(SystemLookup),
            pool_idle_timeout: Duration::from_secs(90),
            pool_max_idle_per_host: 8,
            adaptive_host_connections: true,
            max_connections_per_host: 32,
            dns_positive_ttl: Duration::from_secs(300),
            dns_negative_ttl: Duration::from_secs(30),
            dns_transient_retries: 1,
            happy_eyeballs_delay: Duration::from_millis(250),
            tail_threshold: Duration::from_secs(2),
            stage_controller: StageControllerConfig::default(),
        }
    }
}

impl fmt::Debug for FetcherConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FetcherConfig")
            .field("user_agent", &self.user_agent)
            .field("connect_timeout", &self.connect_timeout)
            .field("dns_timeout", &self.dns_timeout)
            .field("tls_timeout", &self.tls_timeout)
            .field("first_byte_timeout", &self.first_byte_timeout)
            .field("body_idle_timeout", &self.body_idle_timeout)
            .field("total_timeout", &self.total_timeout)
            .field("max_redirects", &self.max_redirects)
            .field("max_body_bytes", &self.max_body_bytes)
            .field("ssrf", &self.ssrf)
            .field("proxy", &self.proxy.as_deref().map(redact_url))
            .field("credentials", &self.credentials)
            .field("adaptive_host_connections", &self.adaptive_host_connections)
            .field("max_connections_per_host", &self.max_connections_per_host)
            .finish_non_exhaustive()
    }
}

/// 单次请求。只支持 GET。
#[derive(Clone, Default)]
pub struct FetchRequest {
    /// Reject a redirect before sending it if scheme/host/port differs from the
    /// initial URL. Useful for explicitly scoped dependency discovery.
    pub same_origin_redirects: bool,
    /// Explicit identity-scoped Cookie session. Stateless by default.
    pub session: Option<Arc<crate::CookieSession>>,
    pub url: String,
    /// 请求自带的头，只发给初始 URL 的主机。
    pub headers: Vec<(String, String)>,
    pub if_none_match: Option<String>,
    pub if_modified_since: Option<SystemTime>,
    /// 覆盖 [`FetcherConfig::max_body_bytes`]。
    pub max_body_bytes: Option<usize>,
    /// 覆盖 [`FetcherConfig::total_timeout`]。
    pub timeout: Option<Duration>,
}

impl FetchRequest {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ..Default::default()
        }
    }

    pub fn with_session(mut self, session: Arc<crate::CookieSession>) -> Self {
        self.session = Some(session);
        self
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn if_none_match(mut self, etag: impl Into<String>) -> Self {
        self.if_none_match = Some(etag.into());
        self
    }

    pub fn if_modified_since(mut self, t: SystemTime) -> Self {
        self.if_modified_since = Some(t);
        self
    }
}

impl fmt::Debug for FetchRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("FetchRequest")
            .field("same_origin_redirects", &self.same_origin_redirects)
            .field("url", &redact_url(&self.url))
            .field("headers", &names)
            .field("if_none_match", &self.if_none_match)
            .field("if_modified_since", &self.if_modified_since)
            .field("max_body_bytes", &self.max_body_bytes)
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedirectHop {
    /// 返回重定向的那个 URL。
    pub url: Url,
    pub status: u16,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timings {
    /// 从开始到收到最终响应头（含所有重定向）。
    pub first_byte: Duration,
    pub total: Duration,
}

#[derive(Debug, Clone)]
pub struct FetchResponse {
    pub final_url: Url,
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    pub redirect_chain: Vec<RedirectHop>,
    pub timings: Timings,
    /// 响应体超过上限被截断。
    pub truncated: bool,
    /// 条件请求命中，服务器返回 304。
    pub not_modified: bool,
    /// 传输层提示，例如 `content_encoding_mismatch: ...`、`content_decoding_failed: ...`、
    /// `content_encoding_unsupported: ...`。调用方可并入页面元数据的 warnings。
    pub warnings: Vec<String>,
}

impl FetchResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    pub fn content_type(&self) -> Option<&str> {
        self.header("content-type")
    }

    pub fn etag(&self) -> Option<&str> {
        self.header("etag")
    }

    pub fn last_modified(&self) -> Option<&str> {
        self.header("last-modified")
    }

    /// 按 BOM > Content-Type > meta > 启发式 解码成 UTF-8。
    pub fn text(&self) -> Decoded {
        charset::decode(&self.body, self.content_type(), self.tld())
    }

    /// 解码并取走响应体（之后 `body` 为空）。合法 UTF-8 时不复制。
    pub fn take_text(&mut self) -> Decoded {
        let body = std::mem::take(&mut self.body);
        let content_type = self.content_type().map(str::to_owned);
        charset::decode_owned(body, content_type.as_deref(), self.tld())
    }

    fn tld(&self) -> Option<&[u8]> {
        self.final_url
            .domain()
            .and_then(|d| d.trim_end_matches('.').rsplit('.').next())
            .map(str::as_bytes)
    }
}

/// An undecoded response body. Callers must bound disk usage and retain their
/// policy permit until EOF. Dropping the stream closes an unfinished body.
pub struct FetchStream {
    pub final_url: Url,
    pub status: u16,
    pub headers: HeaderMap,
    pub redirect_chain: Vec<RedirectHop>,
    pub first_byte: Duration,
    pub(super) response: Resp,
    pub(super) deadline: Instant,
    pub(super) body_idle_timeout: Duration,
    pub(super) session_use: Option<crate::session::SessionUse>,
}

impl FetchStream {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    pub async fn chunk(&mut self) -> Result<Option<bytes::Bytes>> {
        let idle_deadline = (Instant::now() + self.body_idle_timeout).min(self.deadline);
        let chunk = timeout_at(idle_deadline, self.response.chunk())
            .await
            .map_err(|_| {
                if idle_deadline == self.deadline {
                    Error::Timeout(TimeoutPhase::Total)
                } else {
                    Error::Timeout(TimeoutPhase::BodyIdle)
                }
            })?;
        if matches!(chunk, Ok(None))
            && let Some(usage) = self.session_use.take()
        {
            usage.finish(self.status);
        }
        if chunk.is_err() {
            self.session_use.take();
        }
        chunk
    }
}
