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
use crate::wait::{Phase, WaitTable};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 底层域名查询。默认用系统解析器；测试或自定义解析时可以替换。
pub trait DnsLookup: Send + Sync + 'static {
    fn lookup<'a>(&'a self, host: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>>;

    /// 带缓存时长的查询：能拿到记录 TTL 的解析器（自带 DNS 报文解析的实现）覆盖它，
    /// 肯定回答返回记录 TTL 的最小值（RFC 1035 §3.2.1、RFC 2181 §5.2），否定回答返回
    /// SOA 的 `min(TTL, MINIMUM)`（RFC 2308 §5），见 [`negative_ttl`]。
    /// 默认（系统解析器 getaddrinfo 不给 TTL）返回 `None`，由配置的回退值决定缓存时长。
    fn lookup_ttl<'a>(
        &'a self,
        host: &'a str,
    ) -> BoxFuture<'a, (io::Result<Vec<IpAddr>>, Option<Duration>)> {
        Box::pin(async move { (self.lookup(host).await, None) })
    }
}

/// RFC 2308 §5：否定回答的缓存时长取 SOA 记录自身 TTL 与 MINIMUM 字段中较小的一个。
pub fn negative_ttl(soa_ttl: u32, soa_minimum: u32) -> Duration {
    Duration::from_secs(u64::from(soa_ttl.min(soa_minimum)))
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
    waits: Arc<WaitTable>,
    cache: Mutex<DnsCache>,
}

/// [策略] 先验包 v0：0.81 的现值。缓存项第一次到这个数时清掉过期项；之后门槛按清理后
/// 存活数的两倍（摊还 O(1)，`wait::next_sweep`），缓存不超过存活项的两倍
const DNS_SWEEP_AT: usize = 16_384;

struct DnsCache {
    map: HashMap<String, Arc<AsyncMutex<Option<CachedAnswer>>>>,
    sweep_at: usize,
}

impl GuardedResolver {
    #[allow(clippy::too_many_arguments)] // 构造时一次性给齐策略与共享统计，只在 Fetcher 里调用一处
    pub(crate) fn new(
        lookup: Arc<dyn DnsLookup>,
        policy: SsrfPolicy,
        exempt: Vec<String>,
        positive_ttl: Duration,
        negative_ttl: Duration,
        transient_retries: usize,
        stages: StageController,
        waits: Arc<WaitTable>,
    ) -> Self {
        Self {
            lookup,
            policy,
            exempt,
            positive_ttl,
            negative_ttl,
            transient_retries,
            stages,
            waits,
            cache: Mutex::new(DnsCache {
                map: HashMap::new(),
                sweep_at: DNS_SWEEP_AT,
            }),
        }
    }

    pub(crate) fn is_exempt(&self, host: &str) -> bool {
        let host = host.trim_matches(['[', ']']);
        self.exempt.iter().any(|e| e.eq_ignore_ascii_case(host))
    }

    /// 解析主机（含 IP 字面量）并按当前 `SsrfPolicy` / `exempt` 检查地址；
    /// `proxy_exempt = true` 仅用于运维显式配置的出口代理连接本身，不放行目标请求。
    pub(crate) async fn resolve_host_for_proxy(
        &self,
        host: &str,
        proxy_exempt: bool,
    ) -> Result<Vec<IpAddr>, ResolveError> {
        let bare = host.trim_matches(['[', ']']);
        if let Ok(ip) = bare.parse::<IpAddr>() {
            if !proxy_exempt
                && !self.is_exempt(bare)
                && let Err(error) = self.policy.check_ip(ip)
            {
                return Err(ResolveError::Blocked(error));
            }
            return Ok(vec![ip]);
        }
        if proxy_exempt {
            if let Some(ips) = crate::localhost::loopback(host) {
                return Ok(ips);
            }
            let permit = self.stages.acquire(NetworkStage::Dns).await;
            let (result, _) = self.lookup.lookup_ttl(host).await;
            return match result {
                Ok(ips) if ips.is_empty() => {
                    permit.finish(StageOutcome::DeterministicFailure);
                    Err(ResolveError::Blocked(Blocked::NoAddress(host.to_string())))
                }
                Ok(ips) => {
                    permit.finish(StageOutcome::Success);
                    Ok(ips)
                }
                Err(error) => {
                    let outcome = if is_transient(&error) {
                        StageOutcome::TransientFailure
                    } else {
                        StageOutcome::DeterministicFailure
                    };
                    permit.finish(outcome);
                    Err(ResolveError::Io(error))
                }
            };
        }
        self.resolve_checked(host).await
    }

    pub async fn resolve_checked(&self, host: &str) -> Result<Vec<IpAddr>, ResolveError> {
        let permit = self.stages.acquire(NetworkStage::Dns).await;
        // localhost 和 *.localhost 是回环地址，不查系统解析器（它可能不认子域名，或者没有 IPv6）
        if let Some(ips) = crate::localhost::loopback(host) {
            return self.finish_checked(host, Ok(ips), permit);
        }
        let slot = {
            let mut cache = self.cache.lock().expect("DNS cache lock poisoned");
            if cache.map.len() >= cache.sweep_at && !cache.map.contains_key(host) {
                cache.map.retain(|_, slot| {
                    Arc::strong_count(slot) > 1
                        || slot.try_lock().map_or(true, |entry| {
                            entry.as_ref().is_some_and(|e| e.expires > Instant::now())
                        })
                });
                cache.sweep_at = crate::wait::next_sweep(cache.map.len(), DNS_SWEEP_AT);
            }
            cache
                .map
                .entry(host.to_ascii_lowercase())
                .or_default()
                .clone()
        };
        let mut cached = slot.lock().await;
        if let Some(answer) = cached
            .as_ref()
            .filter(|answer| answer.expires > Instant::now())
        {
            let result = answer.result();
            return self.finish_checked(host, result, permit);
        }

        // 瞬时失败的重试：次数按恢复时长分布（先验是配置值），第 k 次前等 底数·2^k，
        // 底数是解析器 RTT 的中位数（先验 25 ms，0.81 的现值）。总时长受调用方的 DNS 上限约束
        let net = &self.waits.net;
        let retries = net.dns_retries(self.transient_retries);
        let started = Instant::now();
        let mut first_failure = None;
        let mut attempt: u32 = 0;
        let (lookup, record_ttl) = loop {
            let tried = Instant::now();
            match self.lookup.lookup_ttl(host).await {
                (Ok(ips), ttl) => {
                    net.dns_answered(host, tried.elapsed());
                    if let Some(failed) = first_failure {
                        net.dns_recovery(host, Instant::now() - failed, true);
                    }
                    break (Ok(ips), ttl);
                }
                (Err(error), _) if is_transient(&error) && (attempt as usize) < retries => {
                    first_failure.get_or_insert(tried);
                    attempt += 1;
                    sleep(
                        net.dns_backoff_base()
                            .saturating_mul(2u32.saturating_pow(attempt)),
                    )
                    .await;
                }
                (Err(error), ttl) => {
                    if let Some(failed) = first_failure {
                        net.dns_recovery(host, Instant::now() - failed, false);
                    }
                    break (Err(error), ttl);
                }
            }
        };
        // 真正查过一次（不是缓存命中）：这台主机 DNS 阶段的一个完成样本
        self.waits.host(host).done(Phase::Dns, started.elapsed());
        let ttl = record_ttl.unwrap_or(if lookup.is_ok() {
            self.positive_ttl
        } else {
            self.negative_ttl
        });
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
        if !self.is_exempt(host) {
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
