//! 带 SSRF 校验的 DNS 解析：连接用的正是校验过的地址，DNS 重绑定无从下手。

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::{Instant, sleep};

use crate::ssrf::{Blocked, SsrfPolicy};
use crate::stages::{NetworkStage, StageController, StageOutcome};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 底层域名查询。默认用系统解析器；测试或自定义解析时可以替换。
pub trait DnsLookup: Send + Sync + 'static {
    fn lookup<'a>(&'a self, host: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>>;
}

/// 系统解析器（getaddrinfo）。
#[derive(Debug, Default)]
pub struct SystemLookup;

impl DnsLookup for SystemLookup {
    fn lookup<'a>(&'a self, host: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host, 0)).await?;
            Ok(addrs.map(|a| a.ip()).collect())
        })
    }
}

pub(crate) struct GuardedResolver {
    pub lookup: Arc<dyn DnsLookup>,
    pub policy: SsrfPolicy,
    /// 管理员配置的代理主机，不受内网限制。
    pub exempt: Vec<String>,
    pub positive_ttl: Duration,
    pub negative_ttl: Duration,
    pub transient_retries: usize,
    pub stages: StageController,
    cache: Mutex<HashMap<String, Arc<AsyncMutex<Option<CachedAnswer>>>>>,
}

impl GuardedResolver {
    pub(crate) fn new(
        lookup: Arc<dyn DnsLookup>,
        policy: SsrfPolicy,
        exempt: Vec<String>,
        positive_ttl: Duration,
        negative_ttl: Duration,
        transient_retries: usize,
        stages: StageController,
    ) -> Self {
        Self {
            lookup,
            policy,
            exempt,
            positive_ttl,
            negative_ttl,
            transient_retries,
            stages,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub async fn resolve_checked(&self, host: &str) -> Result<Vec<IpAddr>, ResolveError> {
        let permit = self.stages.acquire(NetworkStage::Dns).await;
        // localhost 和 *.localhost 是回环地址，不查系统解析器（它可能不认子域名，或者没有 IPv6）
        if let Some(ips) = crate::localhost::loopback(host) {
            return self.finish_checked(host, Ok(ips), permit);
        }
        let slot = {
            let mut cache = self.cache.lock().expect("DNS cache lock poisoned");
            if cache.len() >= 16_384 && !cache.contains_key(host) {
                cache.retain(|_, slot| {
                    Arc::strong_count(slot) > 1
                        || slot.try_lock().map_or(true, |entry| {
                            entry.as_ref().is_some_and(|e| e.expires > Instant::now())
                        })
                });
            }
            cache.entry(host.to_ascii_lowercase()).or_default().clone()
        };
        let mut cached = slot.lock().await;
        if let Some(answer) = cached
            .as_ref()
            .filter(|answer| answer.expires > Instant::now())
        {
            let result = answer.result();
            return self.finish_checked(host, result, permit);
        }

        let mut attempt = 0;
        let lookup = loop {
            match self.lookup.lookup(host).await {
                Ok(ips) => break Ok(ips),
                Err(error) if is_transient(&error) && attempt < self.transient_retries => {
                    attempt += 1;
                    sleep(Duration::from_millis(25 * (1_u64 << attempt.min(4)))).await;
                }
                Err(error) => break Err(error),
            }
        };
        let ttl = if lookup.is_ok() {
            self.positive_ttl
        } else {
            self.negative_ttl
        };
        *cached = Some(CachedAnswer::from_result(&lookup, Instant::now() + ttl));
        self.finish_checked(host, lookup, permit)
    }

    fn finish_checked(
        &self,
        host: &str,
        result: io::Result<Vec<IpAddr>>,
        permit: crate::stages::StagePermit,
    ) -> Result<Vec<IpAddr>, ResolveError> {
        let ips = match result {
            Ok(ips) => ips,
            Err(error) => {
                let outcome = if is_transient(&error) {
                    StageOutcome::TransientFailure
                } else {
                    StageOutcome::DeterministicFailure
                };
                permit.finish(outcome);
                return Err(ResolveError::Io(error));
            }
        };
        if ips.is_empty() {
            permit.finish(StageOutcome::DeterministicFailure);
            return Err(ResolveError::Blocked(Blocked::NoAddress(host.to_string())));
        }
        if !self.exempt.iter().any(|e| e.eq_ignore_ascii_case(host)) {
            // 只要有一个地址不合格就整体拒绝。
            for ip in &ips {
                if let Err(error) = self.policy.check_ip(*ip) {
                    permit.finish(StageOutcome::PolicyRejected);
                    return Err(ResolveError::Blocked(error));
                }
            }
        }
        permit.finish(StageOutcome::Success);
        Ok(ips)
    }
}

#[derive(Debug, Clone)]
struct CachedAnswer {
    expires: Instant,
    value: CachedValue,
}

#[derive(Debug, Clone)]
enum CachedValue {
    Addresses(Vec<IpAddr>),
    Error(io::ErrorKind, String),
}

impl CachedAnswer {
    fn from_result(result: &io::Result<Vec<IpAddr>>, expires: Instant) -> Self {
        let value = match result {
            Ok(ips) => CachedValue::Addresses(ips.clone()),
            Err(error) => CachedValue::Error(error.kind(), error.to_string()),
        };
        Self { expires, value }
    }

    fn result(&self) -> io::Result<Vec<IpAddr>> {
        match &self.value {
            CachedValue::Addresses(ips) => Ok(ips.clone()),
            CachedValue::Error(kind, message) => Err(io::Error::new(*kind, message.clone())),
        }
    }
}

fn is_transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
            | io::ErrorKind::NetworkDown
            | io::ErrorKind::NetworkUnreachable
            | io::ErrorKind::HostUnreachable
    )
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ResolveError {
    #[error(transparent)]
    Blocked(Blocked),
    #[error("DNS lookup failed: {0}")]
    Io(io::Error),
}

pub(crate) struct ReqwestResolver(pub Arc<GuardedResolver>);

impl Resolve for ReqwestResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let inner = self.0.clone();
        Box::pin(async move {
            let ips = inner.resolve_checked(name.as_str()).await?;
            let addrs: Addrs = Box::new(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}
