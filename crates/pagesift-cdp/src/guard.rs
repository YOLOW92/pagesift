//! 子请求拦截：每个经过 CDP Fetch 域的请求都要过 [`RequestGuard`]。

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::pin::Pin;

use url::{Host, Url};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 一个被拦下等待放行的请求（包括主文档和每一跳重定向）。
#[derive(Debug, Clone)]
pub struct SubRequest {
    pub url: String,
    pub method: String,
    /// CDP 的 `Network.ResourceType`，如 `Document`、`Script`、`Image`、`Font`、`Media`。
    pub resource_type: String,
    /// 是否是页面主框架的导航请求。
    pub is_main_document: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Block(String),
}

/// 由调用方实现。网络层的 SSRF 检查以后接到这里。
pub trait RequestGuard: Send + Sync + 'static {
    fn check<'a>(&'a self, req: &'a SubRequest) -> BoxFuture<'a, Verdict>;
}

/// 简单实现：只放行 http(s)、data、blob；拒绝解析到非公网地址的主机。
///
/// 浏览器会自己再解析一次 DNS，所以这里挡不住 DNS 重绑定；
/// 完整的 SSRF 防护要等网络层提供解析结果固定的出口再接进来。
#[derive(Debug, Default, Clone)]
pub struct PrivateNetworkGuard;

impl RequestGuard for PrivateNetworkGuard {
    fn check<'a>(&'a self, req: &'a SubRequest) -> BoxFuture<'a, Verdict> {
        Box::pin(async move { check_public_url(&req.url).await })
    }
}

pub async fn check_public_url(raw: &str) -> Verdict {
    let Ok(url) = Url::parse(raw) else {
        return Verdict::Block("invalid url".into());
    };
    match url.scheme() {
        "http" | "https" => {}
        "data" | "blob" | "about" => return Verdict::Allow,
        s => return Verdict::Block(format!("scheme `{s}` not allowed")),
    }
    let port = url.port_or_known_default().unwrap_or(80);
    let ips: Vec<IpAddr> = match url.host() {
        None => return Verdict::Block("missing host".into()),
        Some(Host::Ipv4(ip)) => vec![IpAddr::V4(ip)],
        Some(Host::Ipv6(ip)) => vec![IpAddr::V6(ip)],
        Some(Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            if d == "localhost" || d.ends_with(".localhost") {
                return Verdict::Block("localhost".into());
            }
            match tokio::net::lookup_host((d.as_str(), port)).await {
                Ok(addrs) => addrs.map(|a| a.ip()).collect(),
                Err(_) => return Verdict::Block("dns resolution failed".into()),
            }
        }
    };
    if ips.is_empty() {
        return Verdict::Block("dns resolution returned no addresses".into());
    }
    // 只要有一个地址不是公网就整体拒绝。
    match ips.iter().find(|ip| !is_public_ip(**ip)) {
        Some(ip) => Verdict::Block(format!("non-public address {ip}")),
        None => Verdict::Allow,
    }
}

pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            is_public_v6(v6)
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..128).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..32).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    // NAT64 和 6to4 里嵌着 IPv4，按内嵌地址判断。
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        return is_public_v4(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            s[6] as u8,
            (s[7] >> 8) as u8,
            s[7] as u8,
        ));
    }
    if s[0] == 0x2002 {
        return is_public_v4(Ipv4Addr::new(
            (s[1] >> 8) as u8,
            s[1] as u8,
            (s[2] >> 8) as u8,
            s[2] as u8,
        ));
    }
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00
        || (s[0] & 0xffc0) == 0xfe80
        || (s[0] & 0xffc0) == 0xfec0
        || (s[0] == 0x2001 && s[1] == 0x0db8)
        || (s[0] == 0x0100 && s[1..4] == [0, 0, 0])
        || s[0..6] == [0, 0, 0, 0, 0, 0])
}

/// 可选屏蔽的资源类别。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockResources {
    pub ads: bool,
    pub fonts: bool,
    pub media: bool,
    pub images: bool,
}

const AD_HOSTS: &[&str] = &[
    "doubleclick.net",
    "googlesyndication.com",
    "googleadservices.com",
    "googletagservices.com",
    "adservice.google.com",
    "amazon-adsystem.com",
    "adnxs.com",
    "criteo.com",
    "criteo.net",
    "taboola.com",
    "outbrain.com",
    "pubmatic.com",
    "rubiconproject.com",
    "openx.net",
    "casalemedia.com",
    "moatads.com",
    "adsrvr.org",
    "scorecardresearch.com",
    "quantserve.com",
    "pos.baidu.com",
    "cpro.baidustatic.com",
    "tanx.com",
    "mmstat.com",
];

impl BlockResources {
    pub(crate) fn reason(&self, req: &SubRequest) -> Option<&'static str> {
        if req.is_main_document {
            return None;
        }
        match req.resource_type.as_str() {
            "Font" if self.fonts => return Some("font blocked"),
            "Media" if self.media => return Some("media blocked"),
            "Image" if self.images => return Some("image blocked"),
            _ => {}
        }
        if self.ads && is_ad_url(&req.url) {
            return Some("ad blocked");
        }
        None
    }
}

fn is_ad_url(raw: &str) -> bool {
    let Some(host) = Url::parse(raw)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
    else {
        return false;
    };
    AD_HOSTS
        .iter()
        .any(|h| host == *h || host.strip_suffix(h).is_some_and(|p| p.ends_with('.')))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(url: &str, ty: &str) -> SubRequest {
        SubRequest {
            url: url.into(),
            method: "GET".into(),
            resource_type: ty.into(),
            is_main_document: false,
        }
    }

    #[test]
    fn private_ranges() {
        for ip in [
            "10.0.0.1",
            "127.0.0.1",
            "172.16.5.4",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "198.18.0.1",
            "::1",
            "::",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
            "::ffff:10.1.2.3",
            "64:ff9b::a00:1",
            "2002:c0a8:0101::1",
            "2001:db8::1",
        ] {
            assert!(!is_public_ip(ip.parse().unwrap()), "{ip} should be private");
        }
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "172.32.0.1",
            "100.128.0.1",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
        ] {
            assert!(is_public_ip(ip.parse().unwrap()), "{ip} should be public");
        }
    }

    #[tokio::test]
    async fn blocks_private_urls() {
        for url in [
            "http://127.0.0.1/",
            "http://localhost:8080/x",
            "http://foo.localhost/",
            "http://[::1]/",
            "http://[::ffff:192.168.0.1]/",
            "http://2130706433/",
            "http://0177.0.0.1/",
            "http://0x7f.1/",
            "http://169.254.169.254/latest/meta-data/",
            "file:///etc/passwd",
            "ftp://8.8.8.8/",
            "chrome://settings",
        ] {
            assert!(
                matches!(check_public_url(url).await, Verdict::Block(_)),
                "{url} should be blocked"
            );
        }
        assert_eq!(check_public_url("http://8.8.8.8/").await, Verdict::Allow);
        assert_eq!(check_public_url("data:text/plain,hi").await, Verdict::Allow);
        let g = PrivateNetworkGuard;
        assert!(matches!(
            g.check(&req("http://10.0.0.8/a.js", "Script")).await,
            Verdict::Block(_)
        ));
    }

    #[test]
    fn resource_blocking() {
        let b = BlockResources {
            ads: true,
            fonts: true,
            media: false,
            images: false,
        };
        assert!(
            b.reason(&req("https://example.com/a.woff2", "Font"))
                .is_some()
        );
        assert!(
            b.reason(&req("https://example.com/a.mp4", "Media"))
                .is_none()
        );
        assert!(
            b.reason(&req(
                "https://securepubads.g.doubleclick.net/tag.js",
                "Script"
            ))
            .is_some()
        );
        assert!(
            b.reason(&req("https://notdoubleclick.net/x.js", "Script"))
                .is_none()
        );
        let mut main = req("https://doubleclick.net/", "Document");
        main.is_main_document = true;
        assert!(b.reason(&main).is_none());
    }
}
