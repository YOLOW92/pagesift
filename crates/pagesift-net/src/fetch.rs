use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use reqwest::header::{self, HeaderMap, HeaderName, HeaderValue};
use reqwest::redirect;
use tokio::time::{Instant, timeout_at};
use url::Url;

use crate::charset::{self, Decoded};
use crate::client;
use crate::credentials::{HostCredentials, redact_url};
use crate::dns::{
    BoxFuture, DnsLookup, GuardedResolver, ReqwestResolver, ResolveError, SystemLookup,
};
use crate::ssrf::{Blocked, SsrfPolicy};
use crate::ua::{DEFAULT_USER_AGENT, UserAgentError, validate_user_agent};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutPhase {
    Connect,
    FirstByte,
    Total,
}

impl fmt::Display for TimeoutPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Connect => "connect",
            Self::FirstByte => "first byte",
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
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// 每一跳重定向前调用，policy 层在这里做 robots 校验。SSRF 校验由 Fetcher 自己做。
pub trait RedirectGuard: Send + Sync {
    fn check<'a>(&'a self, from: &'a Url, to: &'a Url) -> BoxFuture<'a, Result<(), BoxError>>;
}

struct NoGuard;

impl RedirectGuard for NoGuard {
    fn check<'a>(&'a self, _: &'a Url, _: &'a Url) -> BoxFuture<'a, Result<(), BoxError>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone)]
pub struct FetcherConfig {
    pub user_agent: String,
    pub connect_timeout: Duration,
    pub first_byte_timeout: Duration,
    pub total_timeout: Duration,
    pub max_redirects: usize,
    pub max_body_bytes: usize,
    pub ssrf: SsrfPolicy,
    /// 出口代理：`http://`、`https://` 或 `socks5://`。
    pub proxy: Option<String>,
    pub credentials: Vec<HostCredentials>,
    pub dns: Arc<dyn DnsLookup>,
    pub pool_idle_timeout: Duration,
    pub pool_max_idle_per_host: usize,
}

impl Default for FetcherConfig {
    fn default() -> Self {
        Self {
            user_agent: DEFAULT_USER_AGENT.to_string(),
            connect_timeout: Duration::from_secs(10),
            first_byte_timeout: Duration::from_secs(30),
            total_timeout: Duration::from_secs(60),
            max_redirects: 5,
            max_body_bytes: 15 * 1024 * 1024,
            ssrf: SsrfPolicy::default(),
            proxy: None,
            credentials: Vec::new(),
            dns: Arc::new(SystemLookup),
            pool_idle_timeout: Duration::from_secs(90),
            pool_max_idle_per_host: 8,
        }
    }
}

impl fmt::Debug for FetcherConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FetcherConfig")
            .field("user_agent", &self.user_agent)
            .field("connect_timeout", &self.connect_timeout)
            .field("first_byte_timeout", &self.first_byte_timeout)
            .field("total_timeout", &self.total_timeout)
            .field("max_redirects", &self.max_redirects)
            .field("max_body_bytes", &self.max_body_bytes)
            .field("ssrf", &self.ssrf)
            .field("proxy", &self.proxy.as_deref().map(redact_url))
            .field("credentials", &self.credentials)
            .finish_non_exhaustive()
    }
}

/// 单次请求。只支持 GET。
#[derive(Clone, Default)]
pub struct FetchRequest {
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

pub struct Fetcher {
    transport: Transport,
    config: FetcherConfig,
    credentials: Vec<(HostCredentials, HeaderMap)>,
}

impl fmt::Debug for Fetcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fetcher")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Fetcher {
    pub fn new(config: FetcherConfig) -> Result<Self> {
        validate_user_agent(&config.user_agent)?;
        let mut exempt = Vec::new();
        let mut builder = reqwest::Client::builder().no_proxy();
        if let Some(p) = &config.proxy {
            let parsed = Url::parse(p).map_err(|_| Error::Config("invalid proxy URL".into()))?;
            // socks5h/socks4a 让代理解析目标域名，会绕过本地的 DNS 校验，所以不支持。
            if !matches!(parsed.scheme(), "http" | "https" | "socks5") {
                return Err(Error::Config(format!(
                    "proxy scheme `{}` is not supported (use http, https or socks5)",
                    parsed.scheme()
                )));
            }
            if let Some(h) = parsed.host_str() {
                exempt.push(h.trim_matches(['[', ']']).to_string());
            }
            let proxy = reqwest::Proxy::all(p.as_str())
                .map_err(|_| Error::Config("invalid proxy URL".into()))?;
            builder = builder.proxy(proxy);
        }
        let resolver = Arc::new(GuardedResolver {
            lookup: config.dns.clone(),
            policy: config.ssrf.clone(),
            exempt,
        });
        let ua = HeaderValue::from_str(&config.user_agent)
            .map_err(|_| Error::Header("user-agent".into()))?;
        let credentials = config
            .credentials
            .iter()
            .map(|c| Ok((c.clone(), credential_headers(c)?)))
            .collect::<Result<_>>()?;
        // 直连用自研客户端（内存可控）；有出口代理时交给 reqwest 处理代理协议
        if config.proxy.is_none() {
            let direct = client::Client::new(
                resolver,
                config.connect_timeout,
                config.pool_idle_timeout,
                config.pool_max_idle_per_host,
                ua,
            )?;
            return Ok(Self {
                transport: Transport::Direct(direct),
                config,
                credentials,
            });
        }
        let client = builder
            .redirect(redirect::Policy::none())
            .referer(false)
            .dns_resolver(Arc::new(ReqwestResolver(resolver)))
            .connect_timeout(config.connect_timeout)
            .pool_idle_timeout(config.pool_idle_timeout)
            .pool_max_idle_per_host(config.pool_max_idle_per_host)
            .tcp_keepalive(Duration::from_secs(60))
            .user_agent(ua)
            .gzip(true)
            .brotli(true)
            .zstd(true)
            .deflate(true)
            .build()
            .map_err(|e| Error::Config(e.to_string()))?;
        Ok(Self {
            transport: Transport::Proxy(client),
            config,
            credentials,
        })
    }

    pub fn config(&self) -> &FetcherConfig {
        &self.config
    }

    pub fn user_agent(&self) -> &str {
        &self.config.user_agent
    }

    /// 按本 Fetcher 的 SSRF 策略校验 URL。
    pub fn check_url(&self, url: &str) -> Result<Url> {
        self.config.ssrf.check_url(url).map_err(|b| match b {
            Blocked::InvalidUrl => Error::InvalidUrl(redact_url(url)),
            b => Error::Blocked(b),
        })
    }

    pub async fn fetch(&self, req: FetchRequest) -> Result<FetchResponse> {
        self.fetch_with(req, &NoGuard).await
    }

    pub async fn fetch_with(
        &self,
        req: FetchRequest,
        guard: &dyn RedirectGuard,
    ) -> Result<FetchResponse> {
        let start = Instant::now();
        let deadline = start + req.timeout.unwrap_or(self.config.total_timeout);
        let extra = request_headers(&req)?;
        let origin = self.check_url(&req.url)?;
        let mut url = origin.clone();
        let mut chain = Vec::new();
        loop {
            let headers = self.headers_for(&url, &origin, &extra);
            let send = self.transport.get(&url, headers);
            let first_byte = (Instant::now() + self.config.first_byte_timeout).min(deadline);
            let resp = match timeout_at(first_byte, send).await {
                Err(_) if first_byte == deadline => {
                    return Err(Error::Timeout(TimeoutPhase::Total));
                }
                Err(_) => return Err(Error::Timeout(TimeoutPhase::FirstByte)),
                Ok(r) => r?,
            };
            let status = resp.status();
            let location = resp
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            if let (301 | 302 | 303 | 307 | 308, Some(loc)) = (status, location) {
                if chain.len() >= self.config.max_redirects {
                    return Err(Error::TooManyRedirects(self.config.max_redirects));
                }
                let next = self.redirect_target(&url, &loc)?;
                guard
                    .check(&url, &next)
                    .await
                    .map_err(|source| Error::RedirectRejected {
                        url: redact_url(next.as_str()),
                        source,
                    })?;
                chain.push(RedirectHop { url, status });
                url = next;
                continue;
            }
            let first_byte = start.elapsed();
            let mut headers = resp.headers().clone();
            let max = req.max_body_bytes.unwrap_or(self.config.max_body_bytes);
            let direct = matches!(resp, Resp::Direct(_));
            let (body, mut truncated) = read_body(resp, max, deadline).await?;
            // 自研客户端拿到的是原始字节，在这里解压（reqwest 自己解压）
            let body = if direct {
                let (body, cut) = client::decode_body(&mut headers, body, max);
                truncated |= cut;
                body
            } else {
                body
            };
            return Ok(FetchResponse {
                final_url: url,
                status,
                headers,
                body,
                redirect_chain: chain,
                timings: Timings {
                    first_byte,
                    total: start.elapsed(),
                },
                truncated,
                not_modified: status == 304,
            });
        }
    }

    fn redirect_target(&self, base: &Url, loc: &str) -> Result<Url> {
        let next = base
            .join(loc)
            .map_err(|_| Error::InvalidUrl(redact_url(loc)))?;
        // 绝对地址先按原始写法查一遍混淆 IP，再统一校验规范化后的结果。
        if Url::parse(loc).is_ok() {
            self.check_url(loc)?;
        }
        self.config.ssrf.check_parsed(&next)?;
        Ok(next)
    }

    fn headers_for(&self, url: &Url, origin: &Url, extra: &HeaderMap) -> HeaderMap {
        let host = url.host_str().unwrap_or_default();
        let mut out = HeaderMap::new();
        for (c, h) in &self.credentials {
            if c.matches(host) {
                for (k, v) in h {
                    out.insert(k, v.clone());
                }
            }
        }
        // 请求自带的头只发给初始主机，且不从 https 降级到 http 后继续发送。
        let same_origin = origin.host_str() == url.host_str()
            && !(origin.scheme() == "https" && url.scheme() == "http");
        if same_origin {
            for (k, v) in extra {
                if k == header::COOKIE {
                    merge_cookie(&mut out, v);
                } else {
                    out.insert(k, v.clone());
                }
            }
        } else {
            for k in [header::IF_NONE_MATCH, header::IF_MODIFIED_SINCE] {
                if let Some(v) = extra.get(&k) {
                    out.insert(k, v.clone());
                }
            }
        }
        out
    }
}

fn merge_cookie(out: &mut HeaderMap, v: &HeaderValue) {
    let mut merged = match out.get(header::COOKIE) {
        Some(old) => {
            let mut b = old.as_bytes().to_vec();
            b.extend_from_slice(b"; ");
            b.extend_from_slice(v.as_bytes());
            HeaderValue::from_bytes(&b).unwrap_or_else(|_| v.clone())
        }
        None => v.clone(),
    };
    merged.set_sensitive(true);
    out.insert(header::COOKIE, merged);
}

const FORBIDDEN_HEADERS: [&str; 12] = [
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "upgrade",
    "te",
    "trailer",
    "proxy-authorization",
    "proxy-connection",
    "accept-encoding",
    "expect",
];

fn parse_header(name: &str, value: &str) -> Result<(HeaderName, HeaderValue)> {
    let n = HeaderName::from_bytes(name.trim().as_bytes())
        .map_err(|_| Error::Header(name.to_string()))?;
    if FORBIDDEN_HEADERS.contains(&n.as_str()) {
        return Err(Error::Header(n.to_string()));
    }
    if n == header::USER_AGENT {
        validate_user_agent(value)?;
    }
    let mut v = HeaderValue::from_str(value).map_err(|_| Error::Header(n.to_string()))?;
    v.set_sensitive(true);
    Ok((n, v))
}

fn credential_headers(c: &HostCredentials) -> Result<HeaderMap> {
    let mut out = HeaderMap::new();
    for (k, v) in &c.headers {
        let (n, v) = parse_header(k, v.expose())?;
        out.insert(n, v);
    }
    if !c.cookies.is_empty() {
        let joined = c
            .cookies
            .iter()
            .map(|(k, v)| format!("{k}={}", v.expose()))
            .collect::<Vec<_>>()
            .join("; ");
        let (_, v) = parse_header("cookie", &joined)?;
        merge_cookie(&mut out, &v);
    }
    Ok(out)
}

fn request_headers(req: &FetchRequest) -> Result<HeaderMap> {
    let mut out = HeaderMap::new();
    for (k, v) in &req.headers {
        let (n, v) = parse_header(k, v)?;
        if n == header::COOKIE {
            merge_cookie(&mut out, &v);
        } else {
            out.insert(n, v);
        }
    }
    if let Some(etag) = &req.if_none_match {
        let v = HeaderValue::from_str(etag).map_err(|_| Error::Header("if-none-match".into()))?;
        out.insert(header::IF_NONE_MATCH, v);
    }
    if let Some(t) = req.if_modified_since {
        let v = HeaderValue::from_str(&httpdate::fmt_http_date(t))
            .map_err(|_| Error::Header("if-modified-since".into()))?;
        out.insert(header::IF_MODIFIED_SINCE, v);
    }
    Ok(out)
}

async fn read_body(mut resp: Resp, max: usize, deadline: Instant) -> Result<(Vec<u8>, bool)> {
    // 有 Content-Length 就一次分配到位，免得边读边扩容（扩容会多占一倍并反复拷贝）
    let hint = resp
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(0, |n| n.min(max as u64) as usize);
    let mut body = Vec::with_capacity(hint);
    loop {
        let chunk = match timeout_at(deadline, resp.chunk()).await {
            Err(_) => return Err(Error::Timeout(TimeoutPhase::Total)),
            Ok(c) => c?,
        };
        let Some(chunk) = chunk else {
            return Ok((body, false));
        };
        let room = max - body.len();
        if chunk.len() > room {
            body.extend_from_slice(&chunk[..room]);
            return Ok((body, true));
        }
        body.extend_from_slice(&chunk);
    }
}

enum Transport {
    Direct(client::Client),
    Proxy(reqwest::Client),
}

enum Resp {
    Direct(client::Response),
    Proxy(reqwest::Response),
}

impl Transport {
    async fn get(&self, url: &Url, headers: HeaderMap) -> Result<Resp> {
        match self {
            Transport::Direct(c) => c.get(url, headers).await.map(Resp::Direct),
            Transport::Proxy(c) => c
                .get(url.clone())
                .headers(headers)
                .send()
                .await
                .map(Resp::Proxy)
                .map_err(map_reqwest),
        }
    }
}

impl Resp {
    fn status(&self) -> u16 {
        match self {
            Resp::Direct(r) => r.status,
            Resp::Proxy(r) => r.status().as_u16(),
        }
    }

    fn headers(&self) -> &HeaderMap {
        match self {
            Resp::Direct(r) => &r.headers,
            Resp::Proxy(r) => r.headers(),
        }
    }

    async fn chunk(&mut self) -> Result<Option<bytes::Bytes>> {
        match self {
            Resp::Direct(r) => r.chunk().await,
            Resp::Proxy(r) => r.chunk().await.map_err(map_reqwest),
        }
    }
}

fn map_reqwest(e: reqwest::Error) -> Error {
    let mut src: Option<&(dyn std::error::Error + 'static)> = Some(&e);
    while let Some(s) = src {
        if let Some(ResolveError::Blocked(b)) = s.downcast_ref::<ResolveError>() {
            return Error::Blocked(b.clone());
        }
        src = s.source();
    }
    if e.is_timeout() {
        return Error::Timeout(if e.is_connect() {
            TimeoutPhase::Connect
        } else {
            TimeoutPhase::FirstByte
        });
    }
    let e = e.without_url();
    let mut msg = e.to_string();
    let mut src = std::error::Error::source(&e);
    while let Some(s) = src {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        src = s.source();
    }
    Error::Network(msg)
}
