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
use crate::wait::{HostWait, Phase, WaitTable};

mod connect;
mod decode;
mod send;
mod shaped;
#[cfg(test)]
mod tests;

use connect::*;
pub(crate) use decode::*;
pub(crate) use shaped::FingerprintUse;

/// HTTP/1 每次读取的固定大小，也就是每条连接读缓冲的常驻大小。
/// [协议] TLS 记录明文的最大长度 2^14 = 16 KiB（RFC 8446 §5.1、RFC 5246 §6.2.1）：rustls 一次
/// 解出一个记录，缓冲再大也不会让一次读拿到更多；更小则一个满记录要分几次读。明文 HTTP 沿用同一值。
/// 不按正文大小分布调：HTTP/1 的正文是流式读出交给调用方的，缓冲大小只影响读的次数，
/// 不影响能读多少；常驻内存（每条空闲连接一份）也以这个上界为准。
const READ_BUF: usize = 16 * 1024;
/// [策略] 先验包 v0：0.81 的现值。空闲连接总数第一次到这个数时顺手清掉过期的；
/// 之后的门槛按清理后存活数的两倍（摊还 O(1)，见 `wait::next_sweep`）
const SWEEP_AT: usize = 4096;
/// [策略] 先验包 v0：0.81 的现值。HTTP/1 池的主机数第一次到这个数时清理；之后同上翻倍
const SWEEP_KEYS_AT: usize = 512;
const ACCEPT_ENCODING: &str = "gzip, br, zstd, deflate";

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    https: bool,
    host: String,
    port: u16,
}

type H1 = http1::SendRequest<Empty<Bytes>>;
type H2 = http2::SendRequest<Empty<Bytes>>;

struct Pools {
    h1: HashMap<Key, Vec<(H1, Instant)>>,
    h2: HashMap<Key, H2>,
    idle: usize,
    /// 下次清理的门槛（空闲连接数、主机数）。
    sweep_idle: usize,
    sweep_keys: usize,
}

impl Default for Pools {
    fn default() -> Self {
        Self {
            h1: HashMap::new(),
            h2: HashMap::new(),
            idle: 0,
            sweep_idle: SWEEP_AT,
            sweep_keys: SWEEP_KEYS_AT,
        }
    }
}

struct Inner {
    resolver: Arc<GuardedResolver>,
    connect_timeout: Duration,
    dns_timeout: Duration,
    tls_timeout: Duration,
    first_byte_timeout: Duration,
    /// 每（主机，阶段）的等待统计与上限。
    waits: Arc<WaitTable>,
    idle_timeout: Duration,
    max_idle: usize,
    adaptive_connections: bool,
    max_hot_per_host: usize,
    user_agent: HeaderValue,
    pools: Mutex<Pools>,
    hosts: Mutex<HashMap<Key, Arc<HostConnectionControl>>>,
    stages: StageController,
    /// 开了指纹模拟时的档案和状态（默认 `None`）。
    fingerprint: Option<shaped::FingerprintState>,
}

#[derive(Clone)]
pub(crate) struct Client(Arc<Inner>);

pub(crate) struct Response {
    pub status: u16,
    pub headers: HeaderMap,
    pub ttfb: Duration,
    pub fingerprint: FingerprintUse,
    body: Incoming,
    /// HTTP/1 连接：正文读完后放回池里。
    release: Option<(Arc<Inner>, Key, H1)>,
    lifecycle: Option<HostPermit>,
    stage: Option<StagePermit>,
    stages: StageController,
    body_started: Instant,
    body_recorded: bool,
    /// 源站的“慢”阈值到点的时刻：读正文到这时还没完就让出热连接名额（不换阶段）。
    slow_at: Option<Instant>,
}

impl Response {
    /// 下一段正文；读完返回 `None`，此时连接回到池里。
    pub async fn chunk(&mut self) -> Result<Option<Bytes>> {
        loop {
            let frame = match self.slow_at {
                Some(at) => match timeout_at(at, self.body.frame()).await {
                    Ok(frame) => frame,
                    Err(_) => {
                        self.slow_at = None;
                        if let Some(permit) = self.lifecycle.as_mut() {
                            permit.park();
                        }
                        continue;
                    }
                },
                None => self.body.frame().await,
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
