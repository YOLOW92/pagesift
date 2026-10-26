//! 直连用的 HTTP 客户端，直接建在 hyper 的连接层上（配置了出口代理时仍走 reqwest）。
//!
//! 自己做这一层是为了内存：hyper 默认每条 HTTP/1 连接的读缓冲会自适应涨到约 400 KB，
//! 连接回到池里以后这块缓冲一直留着。并发爬几百个站时，空闲连接的缓冲就占了堆的四成；
//! reqwest 没有暴露这个参数。这里每次固定读 16 KB，正文边读边交给调用方，缓冲不会变大。
//! 连接池也在这里：HTTP/1 每个主机最多留 `max_idle` 条空闲连接，HTTP/2 每个主机一条共享连接。
//! DNS 走带 SSRF 校验的解析器，TLS 用 rustls 和系统根证书，ALPN 协商 h2。

use std::collections::HashMap;
use std::io::Read;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::body::Incoming;
use hyper::client::conn::{http1, http2};
use hyper::header::{self, HeaderMap, HeaderValue};
use hyper::{Request, Uri};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout_at};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::{self, pki_types::ServerName};
use url::Url;

use crate::dns::{GuardedResolver, ResolveError};
use crate::fetch::{Error, Result, TimeoutPhase};
use crate::host_connections::{
    HostConnectionControl, HostConnectionStats, HostOutcome, HostPermit,
};
use crate::ssrf::Blocked;
use crate::stages::{NetworkStage, StageController, StageOutcome, StagePermit, TransportPhase};

/// HTTP/1 每次读取的固定大小，也就是每条连接读缓冲的常驻大小。
const READ_BUF: usize = 16 * 1024;
/// 空闲连接总数超过这个值时，顺手清掉过期的。
const SWEEP_AT: usize = 4096;
const ACCEPT_ENCODING: &str = "gzip, br, zstd, deflate";

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    https: bool,
    host: String,
    port: u16,
}

type H1 = http1::SendRequest<Empty<Bytes>>;
type H2 = http2::SendRequest<Empty<Bytes>>;

#[derive(Default)]
struct Pools {
    h1: HashMap<Key, Vec<(H1, Instant)>>,
    h2: HashMap<Key, H2>,
    idle: usize,
}

struct Inner {
    resolver: Arc<GuardedResolver>,
    tls: TlsConnector,
    connect_timeout: Duration,
    dns_timeout: Duration,
    tls_timeout: Duration,
    first_byte_timeout: Duration,
    happy_eyeballs_delay: Duration,
    tail_threshold: Duration,
    idle_timeout: Duration,
    max_idle: usize,
    adaptive_connections: bool,
    max_hot_per_host: usize,
    user_agent: HeaderValue,
    pools: Mutex<Pools>,
    hosts: Mutex<HashMap<Key, Arc<HostConnectionControl>>>,
    stages: StageController,
}

#[derive(Clone)]
pub(crate) struct Client(Arc<Inner>);

pub(crate) struct Response {
    pub status: u16,
    pub headers: HeaderMap,
    pub ttfb: Duration,
    body: Incoming,
    /// HTTP/1 连接：正文读完后放回池里。
    release: Option<(Arc<Inner>, Key, H1)>,
    lifecycle: Option<HostPermit>,
    stage: Option<StagePermit>,
    stages: StageController,
    body_started: Instant,
    body_recorded: bool,
    tail_threshold: Duration,
    in_tail: bool,
}

impl Response {
    /// 下一段正文；读完返回 `None`，此时连接回到池里。
    pub async fn chunk(&mut self) -> Result<Option<Bytes>> {
        loop {
            let frame = if self.in_tail {
                self.body.frame().await
            } else {
                let tail_at = self.body_started + self.tail_threshold;
                match timeout_at(tail_at, self.body.frame()).await {
                    Ok(frame) => frame,
                    Err(_) => {
                        let stages = self.stages.clone();
                        self.move_to_tail(&stages).await;
                        continue;
                    }
                }
            };
            match frame {
                None => {
                    if let Some((inner, key, conn)) = self.release.take() {
                        inner.put_h1(key, conn);
                    }
                    if let Some(permit) = self.lifecycle.take() {
                        let outcome = if matches!(self.status, 429 | 503) {
                            HostOutcome::Overload
                        } else if self.status >= 500 {
                            HostOutcome::Failure
                        } else {
                            HostOutcome::Success
                        };
                        permit.finish(outcome);
                    }
                    if let Some(permit) = self.stage.take() {
                        permit.finish(stage_outcome_for_status(self.status));
                    }
                    self.record_body(stage_outcome_for_status(self.status));
                    return Ok(None);
                }
                Some(Err(e)) => {
                    if let Some(permit) = self.lifecycle.take() {
                        permit.finish(HostOutcome::Failure);
                    }
                    if let Some(permit) = self.stage.take() {
                        permit.finish(StageOutcome::TransientFailure);
                    }
                    self.record_body(StageOutcome::TransientFailure);
                    return Err(network(&e));
                }
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        return Ok(Some(data));
                    }
                }
            }
        }
    }

    /// Release controller pressure without teaching from an intentionally
    /// abandoned body (redirects and caller-requested truncation).
    pub(crate) fn abandon(&mut self) {
        if let Some(permit) = self.lifecycle.take() {
            permit.abandon();
        }
        if let Some(permit) = self.stage.take() {
            permit.finish(stage_outcome_for_status(self.status));
        }
        self.record_body(stage_outcome_for_status(self.status));
    }

    pub(crate) async fn move_to_tail(&mut self, stages: &StageController) {
        if self.in_tail {
            return;
        }
        if let Some(permit) = self.stage.take() {
            permit.finish(StageOutcome::Success);
        }
        self.stage = Some(stages.acquire(NetworkStage::Tail).await);
        self.in_tail = true;
    }

    fn record_body(&mut self, outcome: StageOutcome) {
        if !self.body_recorded {
            self.body_recorded = true;
            self.stages
                .record_phase(TransportPhase::Body, self.body_started.elapsed(), outcome);
        }
    }
}

impl Drop for Response {
    fn drop(&mut self) {
        self.record_body(StageOutcome::TransientFailure);
    }
}

impl Client {
    pub(crate) fn new(
        resolver: Arc<GuardedResolver>,
        connect_timeout: Duration,
        dns_timeout: Duration,
        tls_timeout: Duration,
        first_byte_timeout: Duration,
        happy_eyeballs_delay: Duration,
        tail_threshold: Duration,
        idle_timeout: Duration,
        max_idle: usize,
        adaptive_connections: bool,
        max_hot_per_host: usize,
        stages: StageController,
        user_agent: HeaderValue,
    ) -> Result<Self> {
        let mut roots = rustls::RootCertStore::empty();
        let native = rustls_native_certs::load_native_certs();
        for cert in native.certs {
            // 个别系统证书格式不对时跳过，不影响其他证书
            let _ = roots.add(cert);
        }
        if roots.is_empty() {
            return Err(Error::Config("no system root certificates found".into()));
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| Error::Config(e.to_string()))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(Self(Arc::new(Inner {
            resolver,
            tls: TlsConnector::from(Arc::new(tls)),
            connect_timeout,
            dns_timeout,
            tls_timeout,
            first_byte_timeout,
            happy_eyeballs_delay,
            tail_threshold,
            idle_timeout,
            max_idle,
            adaptive_connections,
            max_hot_per_host: max_hot_per_host.max(1),
            user_agent,
            pools: Mutex::new(Pools::default()),
            hosts: Mutex::new(HashMap::new()),
            stages,
        })))
    }

    /// 发一个 GET。`headers` 是调用方的请求头（凭证、条件请求等），连接相关的头由这里加。
    pub(crate) async fn get(&self, url: &Url, headers: HeaderMap) -> Result<Response> {
        let https = match url.scheme() {
            "https" => true,
            "http" => false,
            s => return Err(Error::InvalidUrl(format!("unsupported scheme {s}"))),
        };
        let host = url
            .host_str()
            .ok_or_else(|| Error::InvalidUrl(crate::redact_url(url.as_str())))?;
        let port = url
            .port_or_known_default()
            .unwrap_or(if https { 443 } else { 80 });
        let key = Key {
            https,
            host: host.to_string(),
            port,
        };
        let control = self.0.host_control(&key);
        let mut lifecycle = Some(control.acquire().await);
        let mut stage = None;
        let path = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_string(),
        };
        let authority = match url.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.to_string(),
        };

        // 先用池里的连接；若池化连接已在空闲期被对端单方面关闭（如 broken pipe / RST / channel closed），
        // 自动利用全新连接进行一次透明重试（符合 RFC 规范中对幂等 GET 请求在池化连接上的标准容错要求）。
        let mut pooled = self.0.take(&key).await;
        let mut retried = false;

        loop {
            let (conn, from_pool) = match pooled.take() {
                Some(c) => {
                    lifecycle.as_mut().expect("lifecycle permit").reused();
                    (c, true)
                }
                None => {
                    let started = Instant::now();
                    match self.connect(&key).await {
                        Ok(conn) => {
                            lifecycle
                                .as_mut()
                                .expect("lifecycle permit")
                                .connected_in(started.elapsed());
                            (conn, false)
                        }
                        Err(error) => {
                            lifecycle
                                .take()
                                .expect("lifecycle permit")
                                .finish(HostOutcome::Failure);
                            return Err(error);
                        }
                    }
                }
            };

            if stage.is_none() {
                stage = Some(self.0.stages.acquire(NetworkStage::Http).await);
            }

            let req_uri = match &conn {
                Conn::H1(_) => path.parse::<Uri>(),
                Conn::H2(_) => format!("{}://{authority}{path}", url.scheme()).parse::<Uri>(),
            }
            .map_err(|_| Error::InvalidUrl(crate::redact_url(url.as_str())))?;

            let mut req = Request::get(req_uri)
                .body(Empty::<Bytes>::new())
                .map_err(|e| Error::Network(e.to_string()))?;

            let h = req.headers_mut();
            if matches!(conn, Conn::H1(_)) {
                h.insert(
                    header::HOST,
                    HeaderValue::from_str(&authority).map_err(|_| Error::Header("host".into()))?,
                );
            }
            h.insert(header::USER_AGENT, self.0.user_agent.clone());
            // 现代合规 Accept 与 Accept-Language 头部，显著降低 CDN/WAF 误判拦截率
            h.insert(
                header::ACCEPT,
                HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8"),
            );
            h.insert(
                header::ACCEPT_LANGUAGE,
                HeaderValue::from_static("en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7"),
            );
            h.insert(
                header::ACCEPT_ENCODING,
                HeaderValue::from_static(ACCEPT_ENCODING),
            );
            for (k, v) in headers.iter() {
                h.insert(k, v.clone());
            }

            let send_started = Instant::now();
            let send_deadline = send_started + self.0.first_byte_timeout;
            match conn {
                Conn::H1(mut s) => match timeout_at(send_deadline, s.send_request(req)).await {
                    Ok(Ok(resp)) => {
                        let (parts, body) = resp.into_parts();
                        self.0.stages.record_phase(
                            TransportPhase::Ttfb,
                            send_started.elapsed(),
                            stage_outcome_for_status(parts.status.as_u16()),
                        );
                        return Ok(Response {
                            status: parts.status.as_u16(),
                            headers: parts.headers,
                            ttfb: send_started.elapsed(),
                            body,
                            release: Some((self.0.clone(), key, s)),
                            lifecycle,
                            stage,
                            stages: self.0.stages.clone(),
                            body_started: Instant::now(),
                            body_recorded: false,
                            tail_threshold: self.0.tail_threshold,
                            in_tail: false,
                        });
                    }
                    Ok(Err(_e)) if from_pool && !retried => {
                        self.0.stages.record_phase(
                            TransportPhase::Ttfb,
                            send_started.elapsed(),
                            StageOutcome::TransientFailure,
                        );
                        retried = true;
                        continue;
                    }
                    Ok(Err(e)) => {
                        lifecycle
                            .take()
                            .expect("lifecycle permit")
                            .finish(HostOutcome::Failure);
                        let error = network(&e);
                        self.0.stages.record_phase(
                            TransportPhase::Ttfb,
                            send_started.elapsed(),
                            stage_outcome_for_error(&error),
                        );
                        stage
                            .take()
                            .expect("HTTP stage permit")
                            .finish(stage_outcome_for_error(&error));
                        return Err(error);
                    }
                    Err(_) => {
                        lifecycle
                            .take()
                            .expect("lifecycle permit")
                            .finish(HostOutcome::Failure);
                        let error = Error::Timeout(TimeoutPhase::FirstByte);
                        self.0.stages.record_phase(
                            TransportPhase::Ttfb,
                            send_started.elapsed(),
                            StageOutcome::TransientFailure,
                        );
                        stage
                            .take()
                            .expect("HTTP stage permit")
                            .finish(StageOutcome::TransientFailure);
                        return Err(error);
                    }
                },
                Conn::H2(mut s) => match timeout_at(send_deadline, s.send_request(req)).await {
                    Ok(Ok(resp)) => {
                        let (parts, body) = resp.into_parts();
                        self.0.stages.record_phase(
                            TransportPhase::Ttfb,
                            send_started.elapsed(),
                            stage_outcome_for_status(parts.status.as_u16()),
                        );
                        return Ok(Response {
                            status: parts.status.as_u16(),
                            headers: parts.headers,
                            ttfb: send_started.elapsed(),
                            body,
                            release: None,
                            lifecycle,
                            stage,
                            stages: self.0.stages.clone(),
                            body_started: Instant::now(),
                            body_recorded: false,
                            tail_threshold: self.0.tail_threshold,
                            in_tail: false,
                        });
                    }
                    Ok(Err(_e)) if from_pool && !retried => {
                        self.0.stages.record_phase(
                            TransportPhase::Ttfb,
                            send_started.elapsed(),
                            StageOutcome::TransientFailure,
                        );
                        retried = true;
                        self.0.remove_h2(&key);
                        continue;
                    }
                    Ok(Err(e)) => {
                        lifecycle
                            .take()
                            .expect("lifecycle permit")
                            .finish(HostOutcome::Failure);
                        let error = network(&e);
                        self.0.stages.record_phase(
                            TransportPhase::Ttfb,
                            send_started.elapsed(),
                            stage_outcome_for_error(&error),
                        );
                        stage
                            .take()
                            .expect("HTTP stage permit")
                            .finish(stage_outcome_for_error(&error));
                        return Err(error);
                    }
                    Err(_) => {
                        lifecycle
                            .take()
                            .expect("lifecycle permit")
                            .finish(HostOutcome::Failure);
                        let error = Error::Timeout(TimeoutPhase::FirstByte);
                        self.0.stages.record_phase(
                            TransportPhase::Ttfb,
                            send_started.elapsed(),
                            StageOutcome::TransientFailure,
                        );
                        stage
                            .take()
                            .expect("HTTP stage permit")
                            .finish(StageOutcome::TransientFailure);
                        return Err(error);
                    }
                },
            }
        }
    }

    pub(crate) fn host_connection_stats(&self) -> Vec<HostConnectionStats> {
        self.0.host_connection_stats()
    }

    async fn connect(&self, key: &Key) -> Result<Conn> {
        let ips: Vec<IpAddr> = match key.host.trim_matches(['[', ']']).parse::<IpAddr>() {
            // IP 字面量在 check_url 里已经按 SSRF 规则校验过
            Ok(ip) => vec![ip],
            Err(_) => {
                let dns_deadline = Instant::now() + self.0.dns_timeout;
                match timeout_at(dns_deadline, self.0.resolver.resolve_checked(&key.host)).await {
                    Err(_) => return Err(Error::Timeout(TimeoutPhase::Dns)),
                    Ok(Err(ResolveError::Blocked(Blocked::NoAddress(h)))) => {
                        return Err(Error::Dns(format!("no address for `{h}`")));
                    }
                    Ok(Err(ResolveError::Blocked(b))) => return Err(Error::Blocked(b)),
                    Ok(Err(ResolveError::Io(e))) => {
                        return Err(Error::Dns(format!("{}: {e}", key.host)));
                    }
                    Ok(Ok(ips)) => ips,
                }
            }
        };
        let handshake = self.0.stages.acquire(NetworkStage::Handshake).await;
        let connect_deadline = Instant::now() + self.0.connect_timeout;
        let tcp_started = Instant::now();
        let tcp = match happy_eyeballs(ips, key.port, connect_deadline, self.0.happy_eyeballs_delay)
            .await
        {
            Ok(tcp) => {
                self.0.stages.record_phase(
                    TransportPhase::Tcp,
                    tcp_started.elapsed(),
                    StageOutcome::Success,
                );
                tcp
            }
            Err(error) => {
                self.0.stages.record_phase(
                    TransportPhase::Tcp,
                    tcp_started.elapsed(),
                    stage_outcome_for_error(&error),
                );
                handshake.finish(StageOutcome::TransientFailure);
                return Err(error);
            }
        };
        let _ = tcp.set_nodelay(true);
        if !key.https {
            let result = h1_handshake(TokioIo::new(tcp)).await;
            handshake.finish(result.as_ref().map_or_else(
                |error| stage_outcome_for_error(error),
                |_| StageOutcome::Success,
            ));
            return result;
        }
        let name = ServerName::try_from(key.host.trim_matches(['[', ']']).to_string())
            .map_err(|_| Error::InvalidUrl(key.host.clone()))?;
        let tls_deadline = Instant::now() + self.0.tls_timeout;
        let tls_started = Instant::now();
        let tls = match timeout_at(tls_deadline, self.0.tls.connect(name, tcp)).await {
            Err(_) => {
                self.0.stages.record_phase(
                    TransportPhase::Tls,
                    tls_started.elapsed(),
                    StageOutcome::TransientFailure,
                );
                handshake.finish(StageOutcome::TransientFailure);
                return Err(Error::Timeout(TimeoutPhase::Tls));
            }
            Ok(Err(error)) => {
                self.0.stages.record_phase(
                    TransportPhase::Tls,
                    tls_started.elapsed(),
                    StageOutcome::DeterministicFailure,
                );
                handshake.finish(StageOutcome::DeterministicFailure);
                return Err(crate::tls::from_io(&error));
            }
            Ok(Ok(tls)) => {
                self.0.stages.record_phase(
                    TransportPhase::Tls,
                    tls_started.elapsed(),
                    StageOutcome::Success,
                );
                tls
            }
        };
        if tls.get_ref().1.alpn_protocol() == Some(b"h2") {
            let result = http2::Builder::new(TokioExecutor::new())
                .handshake(TokioIo::new(tls))
                .await;
            let (sender, conn) = match result {
                Ok(connection) => connection,
                Err(error) => {
                    let error = network(&error);
                    handshake.finish(stage_outcome_for_error(&error));
                    return Err(error);
                }
            };
            tokio::spawn(conn);
            self.0.put_h2(key.clone(), sender.clone());
            handshake.finish(StageOutcome::Success);
            return Ok(Conn::H2(sender));
        }
        let result = h1_handshake(TokioIo::new(tls)).await;
        handshake.finish(result.as_ref().map_or_else(
            |error| stage_outcome_for_error(error),
            |_| StageOutcome::Success,
        ));
        result
    }
}

async fn happy_eyeballs(
    ips: Vec<IpAddr>,
    port: u16,
    deadline: Instant,
    fallback_delay: Duration,
) -> Result<TcpStream> {
    let candidates = interleave_address_families(ips);
    if candidates.is_empty() {
        return Err(Error::Network("no addresses to connect to".into()));
    }
    let mut attempts = JoinSet::new();
    for (index, ip) in candidates.into_iter().enumerate() {
        let delay = fallback_delay.saturating_mul(index as u32);
        attempts.spawn(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            match timeout_at(deadline, TcpStream::connect((ip, port))).await {
                Ok(result) => result,
                Err(_) => Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "connect timed out",
                )),
            }
        });
    }
    let mut last = None;
    while let Some(result) = attempts.join_next().await {
        match result {
            Ok(Ok(stream)) => {
                attempts.abort_all();
                return Ok(stream);
            }
            Ok(Err(error)) => last = Some(error),
            Err(error) if error.is_cancelled() => {}
            Err(error) => {
                last = Some(std::io::Error::other(format!(
                    "connect task failed: {error}"
                )))
            }
        }
        if Instant::now() >= deadline {
            attempts.abort_all();
            return Err(Error::Timeout(TimeoutPhase::Connect));
        }
    }
    if Instant::now() >= deadline
        || last
            .as_ref()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::TimedOut)
    {
        return Err(Error::Timeout(TimeoutPhase::Connect));
    }
    Err(Error::Network(format!(
        "connect error: {}",
        last.map_or_else(|| "no viable address".into(), |error| error.to_string())
    )))
}

fn interleave_address_families(ips: Vec<IpAddr>) -> Vec<IpAddr> {
    let mut v4 = ips.iter().copied().filter(IpAddr::is_ipv4);
    let mut v6 = ips.iter().copied().filter(IpAddr::is_ipv6);
    let start_v6 = ips.first().is_some_and(IpAddr::is_ipv6);
    let mut out = Vec::with_capacity(ips.len());
    loop {
        let before = out.len();
        if start_v6 {
            out.extend(v6.next());
            out.extend(v4.next());
        } else {
            out.extend(v4.next());
            out.extend(v6.next());
        }
        if out.len() == before {
            return out;
        }
    }
}

fn stage_outcome_for_status(status: u16) -> StageOutcome {
    if matches!(status, 429 | 503) {
        StageOutcome::Overload
    } else {
        StageOutcome::Success
    }
}

fn stage_outcome_for_error(error: &Error) -> StageOutcome {
    match error {
        Error::Timeout(_) | Error::Dns(_) => StageOutcome::TransientFailure,
        Error::Blocked(_) | Error::RedirectRejected { .. } => StageOutcome::PolicyRejected,
        Error::InvalidUrl(_)
        | Error::Config(_)
        | Error::Session(_)
        | Error::UserAgent(_)
        | Error::Header(_)
        | Error::TooManyRedirects(_) => StageOutcome::DeterministicFailure,
        Error::Tls { .. } => StageOutcome::DeterministicFailure,
        Error::Network(_) => StageOutcome::TransientFailure,
    }
}

enum Conn {
    H1(H1),
    H2(H2),
}

async fn h1_handshake<T>(io: T) -> Result<Conn>
where
    T: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let (sender, conn) = http1::Builder::new()
        .read_buf_exact_size(Some(READ_BUF))
        .handshake(io)
        .await
        .map_err(|e| network(&e))?;
    tokio::spawn(async move {
        // 连接出错或对方关闭都在这里结束；请求那边会从 send_request 拿到错误
        let _ = conn.await;
    });
    Ok(Conn::H1(sender))
}

impl Inner {
    fn host_control(&self, key: &Key) -> Arc<HostConnectionControl> {
        let mut hosts = self.hosts.lock().expect("host controls lock poisoned");
        hosts
            .entry(key.clone())
            .or_insert_with(|| {
                HostConnectionControl::new(self.adaptive_connections, self.max_hot_per_host)
            })
            .clone()
    }

    fn host_connection_stats(&self) -> Vec<HostConnectionStats> {
        let hosts = self.hosts.lock().expect("host controls lock poisoned");
        let mut stats = hosts
            .iter()
            .map(|(key, control)| {
                let scheme = if key.https { "https" } else { "http" };
                control.stats(format!("{scheme}://{}:{}", key.host, key.port))
            })
            .collect::<Vec<_>>();
        stats.sort_by(|a, b| a.origin.cmp(&b.origin));
        stats
    }

    async fn take(&self, key: &Key) -> Option<Conn> {
        if let Some(h2) = self.h2_for(key) {
            let mut s = h2;
            if s.ready().await.is_ok() {
                return Some(Conn::H2(s));
            }
            self.pools.lock().expect("lock poisoned").h2.remove(key);
        }
        loop {
            let (mut s, since) = {
                let mut pools = self.pools.lock().expect("lock poisoned");
                let list = pools.h1.get_mut(key)?;
                let entry = list.pop();
                if list.is_empty() {
                    pools.h1.remove(key);
                }
                let entry = entry?;
                pools.idle -= 1;
                entry
            };
            if since.elapsed() >= self.idle_timeout || s.is_closed() {
                continue;
            }
            if s.ready().await.is_ok() {
                return Some(Conn::H1(s));
            }
        }
    }

    fn h2_for(&self, key: &Key) -> Option<H2> {
        let pools = self.pools.lock().expect("lock poisoned");
        let s = pools.h2.get(key)?;
        (!s.is_closed()).then(|| s.clone())
    }

    fn put_h2(&self, key: Key, sender: H2) {
        self.pools
            .lock()
            .expect("lock poisoned")
            .h2
            .insert(key, sender);
    }

    fn remove_h2(&self, key: &Key) {
        let mut pools = self.pools.lock().expect("lock poisoned");
        pools.h2.remove(key);
    }

    fn put_h1(&self, key: Key, sender: H1) {
        if self.max_idle == 0 || sender.is_closed() {
            return;
        }
        // Keep one cold probe connection beyond the current hot watermark.
        // Re-probing then reuses an existing socket instead of reconnecting on
        // every 4 -> 5 -> 4 capacity oscillation.
        let idle_target = self
            .host_control(&key)
            .target()
            .saturating_add(1)
            .min(self.max_idle);
        let mut pools = self.pools.lock().expect("lock poisoned");
        if pools.idle >= SWEEP_AT || pools.h1.len() >= 512 {
            let timeout = self.idle_timeout;
            let mut removed = 0;
            pools.h1.retain(|_, list| {
                let before = list.len();
                list.retain(|(s, since)| since.elapsed() < timeout && !s.is_closed());
                removed += before - list.len();
                !list.is_empty()
            });
            pools.idle -= removed;
            pools.h2.retain(|_, s| !s.is_closed());
        }
        let list = pools.h1.entry(key).or_default();
        if list.len() >= idle_target {
            return;
        }
        list.push((sender, Instant::now()));
        pools.idle += 1;
    }
}

fn network(e: &hyper::Error) -> Error {
    let mut msg = e.to_string();
    let mut src = std::error::Error::source(e);
    while let Some(s) = src {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        src = s.source();
    }
    Error::Network(msg)
}

/// 按 Content-Encoding 解压，最多输出 `max` 字节；返回解压结果和是否被截断。
/// 不认识的编码原样返回。解压成功后去掉 Content-Encoding 和 Content-Length（它们描述的是压缩前）。
pub(crate) fn decode_body(headers: &mut HeaderMap, raw: Vec<u8>, max: usize) -> (Vec<u8>, bool) {
    let encoding = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_ascii_lowercase());
    let Some(encoding) = encoding.filter(|e| !e.is_empty() && e != "identity") else {
        return (raw, false);
    };
    let mut reader: Box<dyn Read + '_> = match encoding.as_str() {
        "gzip" | "x-gzip" => Box::new(flate2::read::MultiGzDecoder::new(&raw[..])),
        "deflate" => {
            // 规范是 zlib 封装，但不少服务器发的是裸 deflate：看头两个字节判断
            let zlib = raw.len() >= 2
                && raw[0] & 0x0f == 8
                && u16::from_be_bytes([raw[0], raw[1]]).is_multiple_of(31);
            if zlib {
                Box::new(flate2::read::ZlibDecoder::new(&raw[..]))
            } else {
                Box::new(flate2::read::DeflateDecoder::new(&raw[..]))
            }
        }
        "br" => Box::new(brotli_decompressor::Decompressor::new(&raw[..], 16 * 1024)),
        "zstd" => match zstd::stream::read::Decoder::with_buffer(&raw[..]) {
            Ok(d) => Box::new(d),
            Err(_) => return (raw, false),
        },
        _ => return (raw, false),
    };
    let mut out = Vec::with_capacity(raw.len().saturating_mul(4).min(max));
    let mut buf = [0u8; 16 * 1024];
    let mut truncated = false;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let room = max - out.len();
                if n > room {
                    out.extend_from_slice(&buf[..room]);
                    truncated = true;
                    break;
                }
                out.extend_from_slice(&buf[..n]);
            }
            // 数据不完整（比如压缩流本身被截断）：保留已经解出的部分
            Err(_) => {
                truncated = true;
                break;
            }
        }
    }
    drop(reader);
    headers.remove(header::CONTENT_ENCODING);
    headers.remove(header::CONTENT_LENGTH);
    (out, truncated)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn happy_eyeballs_interleaves_address_families() {
        let addresses = vec![
            "2001:db8::1".parse().unwrap(),
            "2001:db8::2".parse().unwrap(),
            "192.0.2.1".parse().unwrap(),
            "192.0.2.2".parse().unwrap(),
        ];
        let interleaved = interleave_address_families(addresses);
        assert!(interleaved[0].is_ipv6());
        assert!(interleaved[1].is_ipv4());
        assert!(interleaved[2].is_ipv6());
        assert!(interleaved[3].is_ipv4());
    }

    fn decode(encoding: &str, raw: Vec<u8>, max: usize) -> (Vec<u8>, bool, HeaderMap) {
        let mut h = HeaderMap::new();
        h.insert(
            header::CONTENT_ENCODING,
            HeaderValue::from_str(encoding).unwrap(),
        );
        h.insert(header::CONTENT_LENGTH, HeaderValue::from_static("1"));
        let (body, cut) = decode_body(&mut h, raw, max);
        (body, cut, h)
    }

    #[test]
    fn decodes_every_supported_encoding() {
        let text = b"hello hello hello hello".repeat(50);
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        z.write_all(&text).unwrap();
        let mut d = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        d.write_all(&text).unwrap();
        let zs = zstd::encode_all(&text[..], 1).unwrap();
        for (enc, raw) in [
            ("deflate", z.finish().unwrap()),
            ("deflate", d.finish().unwrap()),
            ("zstd", zs),
        ] {
            let (body, cut, h) = decode(enc, raw, 1 << 20);
            assert_eq!(body, text, "{enc}");
            assert!(!cut);
            assert!(h.get(header::CONTENT_ENCODING).is_none());
            assert!(h.get(header::CONTENT_LENGTH).is_none());
        }
        // 未压缩元数据块的 brotli 流："hello"
        let (body, _, _) = decode(
            "br",
            vec![0x0b, 0x02, 0x80, b'h', b'e', b'l', b'l', b'o', 0x03],
            100,
        );
        assert_eq!(body, b"hello");
    }

    #[test]
    fn caps_decoded_size_and_keeps_unknown_encodings() {
        let mut g = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        g.write_all(&[b'a'; 100_000]).unwrap();
        let (body, cut, _) = decode("gzip", g.finish().unwrap(), 1000);
        assert_eq!(body.len(), 1000);
        assert!(cut, "decompression bombs stop at the body limit");

        let (body, cut, h) = decode("compress", b"raw".to_vec(), 100);
        assert_eq!(body, b"raw");
        assert!(!cut);
        assert!(h.get(header::CONTENT_ENCODING).is_some());
    }
}
