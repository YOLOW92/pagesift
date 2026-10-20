//! 带 SSRF 校验的 DNS 解析：连接用的正是校验过的地址，DNS 重绑定无从下手。

use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

use crate::ssrf::{Blocked, SsrfPolicy};

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
}

impl GuardedResolver {
    pub async fn resolve_checked(&self, host: &str) -> Result<Vec<IpAddr>, ResolveError> {
        let ips = self.lookup.lookup(host).await.map_err(ResolveError::Io)?;
        if ips.is_empty() {
            return Err(ResolveError::Blocked(Blocked::NoAddress(host.to_string())));
        }
        if !self.exempt.iter().any(|e| e.eq_ignore_ascii_case(host)) {
            // 只要有一个地址不合格就整体拒绝。
            for ip in &ips {
                self.policy.check_ip(*ip).map_err(ResolveError::Blocked)?;
            }
        }
        Ok(ips)
    }
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
