use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use reqwest::header::{self, HeaderMap, HeaderName, HeaderValue};
use reqwest::redirect;
use tokio::time::{Instant, timeout_at};
use url::{Host, Url};

use crate::HostConnectionStats;
use crate::charset::{self, Decoded};
use crate::client;
use crate::credentials::{HostCredentials, redact_url};
use crate::dns::{
    BoxFuture, DnsLookup, GuardedResolver, ReqwestResolver, ResolveError, SystemLookup,
};
use crate::ssrf::{Blocked, IpCidr, SsrfPolicy, is_public};
use crate::stages::{
    NetworkStage, PhaseSnapshot, StageController, StageControllerConfig, StagePermit, StageSnapshot,
};
use crate::ua::{DEFAULT_USER_AGENT, UserAgentError, validate_user_agent};

mod proxy;
#[cfg(test)]
mod tests;
mod transport;
mod types;

use proxy::*;
use transport::*;
pub use types::*;

pub struct Fetcher {
    transport: Transport,
    config: FetcherConfig,
    credentials: Vec<(HostCredentials, HeaderMap)>,
    stages: StageController,
    resolver: Arc<GuardedResolver>,
    bypass: ProxyBypass,
}

/// 走代理时为判断“域名是否解析到 allow_cidrs”而做的本地解析，最多等这么久。
/// 内网 DNS 正常时毫秒级返回；2 秒足够覆盖慢的内网解析器，又不会让只能经代理
/// 出网的环境每个请求都卡满默认 5 秒的 dns_timeout。
const BYPASS_DNS_TIMEOUT: Duration = Duration::from_secs(2);

impl fmt::Debug for Fetcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fetcher")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Fetcher {
    pub fn new(config: FetcherConfig) -> Result<Self> {
        Self::new_inner(config, true)
    }

    /// 命名出口专用：这是调用方为本次请求显式选择的代理，不能被环境里的
    /// `NO_PROXY` 或 SSRF 放行名单悄悄改成直连。
    pub fn new_egress(config: FetcherConfig) -> Result<Self> {
        Self::new_inner(config, false)
    }

    fn new_inner(mut config: FetcherConfig, honor_bypass: bool) -> Result<Self> {
        validate_user_agent(&config.user_agent)?;
        if config.proxy.is_none()
            && let Some(env_p) = detect_env_proxy()
        {
            config.proxy = Some(env_p);
        }
        let mut exempt = Vec::new();
        let mut builder = reqwest::Client::builder();
        let mut raw_builder = reqwest::Client::builder();
        let bypass = if honor_bypass {
            ProxyBypass::from_env(&config.ssrf)
        } else {
            ProxyBypass::default()
        };
        if let Some(p) = &config.proxy {
            let parsed = Url::parse(p).map_err(|_| Error::Config("invalid proxy URL".into()))?;
            if !matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h") {
                return Err(Error::Config(format!(
                    "proxy scheme `{}` is not supported (use http, https, socks5 or socks5h)",
                    parsed.scheme()
                )));
            }
            if let Some(h) = parsed.host_str() {
                exempt.push(h.trim_matches(['[', ']']).to_string());
            }
            let proxy = reqwest::Proxy::all(p.as_str())
                .map_err(|_| Error::Config("invalid proxy URL".into()))?;
            // NO_PROXY 和放行名单里的目标不要让 reqwest 也走代理；逐请求的直连选择
            // 在 direct_route 里再做一次（域名解析到 allow_cidrs 的情况只有那里知道）。
            let proxy = if let Some(no_proxy) = bypass.reqwest_no_proxy() {
                proxy.no_proxy(Some(no_proxy))
            } else {
                proxy
            };
            raw_builder = raw_builder.proxy(proxy.clone());
            builder = builder.proxy(proxy);
        } else {
            builder = builder.no_proxy();
            raw_builder = raw_builder.no_proxy();
        }
        // 白名单里的主机解析到内网地址也放行
        exempt.extend(config.ssrf.allow_hosts.iter().cloned());
        let stages = StageController::new(config.stage_controller.clone());
        let resolver = Arc::new(GuardedResolver::new(
            config.dns.clone(),
            config.ssrf.clone(),
            exempt,
            config.dns_positive_ttl,
            config.dns_negative_ttl,
            config.dns_transient_retries,
            stages.clone(),
        ));
        let ua = HeaderValue::from_str(&config.user_agent)
            .map_err(|_| Error::Header("user-agent".into()))?;
        let credentials = config
            .credentials
            .iter()
            .map(|c| Ok((c.clone(), credential_headers(c)?)))
            .collect::<Result<_>>()?;
        // 直连用自研客户端（内存可控）；有出口代理时交给 reqwest 处理代理协议。
        // 即使配了代理也保留直连客户端：NO_PROXY / allow_hosts / allow_cidrs 命中的目标
        // 必须绕过代理，否则内网地址会进出口代理日志。
        let direct = client::Client::new(
            resolver.clone(),
            config.connect_timeout,
            config.dns_timeout,
            config.tls_timeout,
            config.first_byte_timeout,
            config.happy_eyeballs_delay,
            config.tail_threshold,
            config.pool_idle_timeout,
            config.pool_max_idle_per_host,
            config.adaptive_host_connections,
            config.max_connections_per_host,
            stages.clone(),
            ua.clone(),
        )?;
        if config.proxy.is_none() {
            return Ok(Self {
                transport: Transport::Direct(direct),
                config,
                credentials,
                stages,
                resolver,
                bypass,
            });
        }
        let build = |builder: reqwest::ClientBuilder| {
            builder
                .redirect(redirect::Policy::none())
                .referer(false)
                .dns_resolver(Arc::new(ReqwestResolver(resolver.clone())))
                .connect_timeout(config.connect_timeout)
                .pool_idle_timeout(config.pool_idle_timeout)
                .pool_max_idle_per_host(config.pool_max_idle_per_host)
                .tcp_keepalive(Duration::from_secs(60))
                .user_agent(ua.clone())
        };
        let client = build(builder)
            .gzip(true)
            .brotli(true)
            .zstd(true)
            .deflate(true)
            .build()
            .map_err(|e| Error::Config(e.to_string()))?;
        let raw = build(raw_builder)
            .gzip(false)
            .brotli(false)
            .zstd(false)
            .deflate(false)
            .build()
            .map_err(|e| Error::Config(e.to_string()))?;
        Ok(Self {
            transport: Transport::Proxy {
                direct,
                decoded: client,
                identity: raw,
            },
            config,
            credentials,
            stages,
            resolver,
            bypass,
        })
    }

    pub fn config(&self) -> &FetcherConfig {
        &self.config
    }

    pub fn user_agent(&self) -> &str {
        &self.config.user_agent
    }

    /// Snapshot the direct transport's per-origin controller state.
    /// Proxied transports currently expose no origin-local connection state.
    pub fn host_connection_stats(&self) -> Vec<HostConnectionStats> {
        match &self.transport {
            Transport::Direct(client) => client.host_connection_stats(),
            Transport::Proxy { .. } => Vec::new(),
        }
    }

    /// 这个 URL 是否应该绕过代理直连。
    ///
    /// 配了 allow_cidrs / allow_ips 时，未列名的域名在本地解析一次，结果落在放行网段内
    /// 才直连；解析失败或超时都交给代理，不报错。超时取 min(dns_timeout,
    /// [`BYPASS_DNS_TIMEOUT`])，只能经代理出网的环境每个请求最多多等这么久。
    async fn direct_route(&self, url: &Url) -> bool {
        if matches!(self.transport, Transport::Direct(_)) || self.bypass.applies(url) {
            return true;
        }
        let Some(host) = self.bypass.resolution_target(url) else {
            return false;
        };
        let limit = self.config.dns_timeout.min(BYPASS_DNS_TIMEOUT);
        match tokio::time::timeout(limit, self.resolver.resolve_checked(host)).await {
            Ok(Ok(ips)) => self.bypass.resolved_allowed(&ips),
            _ => false,
        }
    }

    pub fn stage_snapshots(&self) -> Vec<StageSnapshot> {
        self.stages.snapshots()
    }

    pub fn phase_snapshots(&self) -> Vec<PhaseSnapshot> {
        self.stages.phase_snapshots()
    }

    /// Admission for parsing or bounded tail work, sharing the same controller
    /// as the network stages. Callers must finish the permit with the real outcome.
    pub async fn acquire_stage(&self, stage: NetworkStage) -> StagePermit {
        self.stages.acquire(stage).await
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
        let max = req.max_body_bytes.unwrap_or(self.config.max_body_bytes);
        let mut stream = self.open(req, guard, false).await?;
        let first_byte = stream.first_byte;
        let status = stream.status;
        let mut headers = stream.headers;
        let direct = matches!(stream.response, Resp::Direct(_));
        let (body, mut truncated) = read_body(
            stream.response,
            max,
            stream.deadline,
            stream.body_idle_timeout,
        )
        .await?;
        let mut warnings = Vec::new();
        let body = if direct {
            let decoded = client::decode_body(&mut headers, body, max, truncated)?;
            truncated = decoded.truncated;
            warnings = decoded.warnings;
            decoded.body
        } else {
            body
        };
        if let Some(usage) = stream.session_use.take() {
            usage.finish(status);
        }
        Ok(FetchResponse {
            final_url: stream.final_url,
            status,
            headers,
            body,
            redirect_chain: stream.redirect_chain,
            timings: Timings {
                first_byte,
                total: start.elapsed(),
            },
            truncated,
            not_modified: status == 304,
            warnings,
        })
    }

    /// Stream identity-encoded bytes with the same DNS, redirects and credential
    /// checks as fetch(). Content decoding is deliberately disabled for Range.
    pub async fn fetch_stream_with(
        &self,
        req: FetchRequest,
        guard: &dyn RedirectGuard,
    ) -> Result<FetchStream> {
        self.open(req, guard, true).await
    }

    pub async fn fetch_stream(&self, req: FetchRequest) -> Result<FetchStream> {
        self.fetch_stream_with(req, &NoGuard).await
    }

    async fn open(
        &self,
        req: FetchRequest,
        guard: &dyn RedirectGuard,
        raw: bool,
    ) -> Result<FetchStream> {
        let start = Instant::now();
        let request_override = req.timeout;
        let body_budget = req.timeout.unwrap_or(self.config.total_timeout);
        let extra = request_headers(&req)?;
        let origin = self.check_url(&req.url)?;
        if req.session.is_some() && extra.contains_key(header::COOKIE) {
            return Err(Error::Header(
                "cookie conflicts with explicit session".into(),
            ));
        }
        let session_use = req.session.as_ref().map(|s| s.begin(&origin)).transpose()?;
        let mut url = origin.clone();
        let mut chain = Vec::new();
        loop {
            let mut headers = self.headers_for(&url, &origin, &extra, req.session.is_some());
            if let Some(session) = &req.session {
                // Explicit sessions own Cookie completely, including when host
                // credentials configured a static cookie for another identity.
                headers.remove(header::COOKIE);
                if let Some(cookie) = session.cookie_header(&url)? {
                    headers.insert(header::COOKIE, cookie);
                }
            }
            if raw {
                headers.insert(
                    header::ACCEPT_ENCODING,
                    HeaderValue::from_static("identity"),
                );
            }
            let direct_route = self.direct_route(&url).await;
            let send = self.transport.get(&url, headers, raw, direct_route);
            let mut resp = if direct_route {
                // Direct transport enforces DNS, TCP, TLS and TTFB deadlines
                // after each stage acquires capacity. Queue time cannot create
                // a false failure-feedback loop.
                //
                // 总预算不能套在这一层：这里包含等待容量的排队时间，把排队算进超时会
                // 让高并发下大量请求还没发出去就被判超时。用户配置的 timeout_secs
                // 通过在 config 层收紧 first_byte_timeout / body_idle_timeout 生效，
                // 那两个 deadline 都从取得容量之后才开始计时。
                if let Some(limit) = request_override {
                    timeout_at(Instant::now() + limit, send)
                        .await
                        .map_err(|_| Error::Timeout(TimeoutPhase::Total))??
                } else {
                    send.await?
                }
            } else {
                let first_byte = Instant::now() + self.config.first_byte_timeout;
                match timeout_at(first_byte, send).await {
                    Err(_) => return Err(Error::Timeout(TimeoutPhase::FirstByte)),
                    Ok(result) => result?,
                }
            };
            let status = resp.status();
            if let Some(session) = &req.session {
                session.receive(&url, resp.headers());
            }
            let location = resp
                .headers()
                .get(header::LOCATION)
                .map(|v| location_text(v.as_bytes()));
            if let (301 | 302 | 303 | 307 | 308, Some(loc)) = (status, location) {
                if chain.len() >= self.config.max_redirects {
                    return Err(Error::TooManyRedirects(self.config.max_redirects));
                }
                let next = upgrade_downgrade(&url, self.redirect_target(&url, &loc)?, &chain);
                if req.same_origin_redirects && next.origin() != origin.origin() {
                    return Err(Error::Config(
                        "cross-origin redirect outside request scope".into(),
                    ));
                }
                guard
                    .check(&url, &next)
                    .await
                    .map_err(|source| Error::RedirectRejected {
                        url: redact_url(next.as_str()),
                        source,
                    })?;
                chain.push(RedirectHop { url, status });
                resp.abandon();
                url = next;
                continue;
            }
            let headers = resp.headers().clone();
            if raw
                && headers
                    .get(header::CONTENT_ENCODING)
                    .is_some_and(|v| v != "identity")
            {
                return Err(Error::Network(
                    "stream requires identity content encoding".into(),
                ));
            }
            let measured_ttfb = resp.ttfb().unwrap_or_else(|| start.elapsed());
            if measured_ttfb >= self.config.tail_threshold {
                resp.move_to_tail(&self.stages).await;
            }
            let deadline = Instant::now() + body_budget;
            return Ok(FetchStream {
                final_url: url,
                status,
                headers,
                redirect_chain: chain,
                first_byte: measured_ttfb,
                response: resp,
                deadline,
                body_idle_timeout: self.config.body_idle_timeout,
                session_use,
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

    fn headers_for(
        &self,
        url: &Url,
        origin: &Url,
        extra: &HeaderMap,
        strict_origin: bool,
    ) -> HeaderMap {
        // A session's identity includes its explicit/configured credentials.
        // A redirect to another port/scheme/host starts without those headers.
        if strict_origin && url.origin() != origin.origin() {
            return HeaderMap::new();
        }
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

/// 同一主机把 https 跳到 http（老站点的规范化规则常这样写：`https://x/a` → `http://x/a/`）：
/// 直接走 https。刚才就是用 https 连上的这个主机，明文那一跳通常只会再跳回 https；省掉它既少一次
/// 往返，也不在明文里走一遍（只放行 https 的网络里，明文那一跳会直接失败）。服务器真要强制
/// http（升级后的地址已经在跳转链里，再升级就成环）时照它的来。
fn upgrade_downgrade(from: &Url, mut next: Url, chain: &[RedirectHop]) -> Url {
    if from.scheme() != "https" || next.scheme() != "http" || from.host_str() != next.host_str() {
        return next;
    }
    let http = next.clone();
    if next.set_scheme("https").is_err() || next.set_port(from.port()).is_err() {
        return http;
    }
    let looped = next == *from || chain.iter().any(|hop| hop.url == next);
    if looped { http } else { next }
}
