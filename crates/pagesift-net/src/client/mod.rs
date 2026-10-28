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

mod connect;
mod decode;
mod send;
#[cfg(test)]
mod tests;

use connect::*;
pub(crate) use decode::*;

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
    /// 源站的“慢”阈值到点的时刻：读正文到这时还没完就让出热连接名额（不换阶段）。
    slow_at: Option<Instant>,
}

impl Response {
    /// 下一段正文；读完返回 `None`，此时连接回到池里。
    pub async fn chunk(&mut self) -> Result<Option<Bytes>> {
        loop {
            let frame = if self.in_tail {
                self.body.frame().await
            } else {
                let tail_at = self.body_started + self.tail_threshold;
                let park_at = self.slow_at.filter(|&t| t < tail_at);
                match timeout_at(park_at.unwrap_or(tail_at), self.body.frame()).await {
                    Ok(frame) => frame,
                    Err(_) if park_at.is_some() => {
                        self.slow_at = None;
                        if let Some(permit) = self.lifecycle.as_mut() {
                            permit.park();
                        }
                        continue;
                    }
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
        if let Some(permit) = self.lifecycle.as_mut() {
            permit.park();
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
