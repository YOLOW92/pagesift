//! SSRF 防护：URL 校验和 IP 地址分类。
//!
//! 域名的解析结果由 [`crate::dns`] 在连接前逐个校验；IP 字面量不走 DNS，
//! 所以在这里直接校验。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

/// 请求被 SSRF 防护拦截的原因。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Blocked {
    #[error("invalid URL")]
    InvalidUrl,
    #[error("scheme `{0}` is not allowed, only http and https")]
    Scheme(String),
    /// 十进制、八进制、十六进制、省略段等非标准写法的 IPv4 字面量。
    #[error("non-canonical IP literal `{0}`")]
    ObfuscatedIp(String),
    #[error("host `{0}` is not allowed")]
    Hostname(String),
    #[error("address {0} is not a public address")]
    Address(IpAddr),
    #[error("DNS returned no address for `{0}`")]
    NoAddress(String),
}

/// SSRF 策略。默认拒绝一切非公网地址。
#[derive(Debug, Clone, Default)]
pub struct SsrfPolicy {
    /// 放开内网、回环等地址（PRD 6.11 的 `allowPrivateNetwork`）。
    pub allow_private_network: bool,
    /// 单独放行的地址，例如内网里的某台服务；其余内网地址仍然拒绝。
    pub allow_ips: Vec<IpAddr>,
    /// 单独放行的 CIDR 网段，例如 `["10.0.0.0/8", "192.168.1.0/24", "198.18.0.0/15"]`。
    pub allow_cidrs: Vec<IpCidr>,
    /// 单独放行的主机名（不区分大小写，完全匹配），解析到内网地址也放行，例如内部服务。
    pub allow_hosts: Vec<String>,
}

/// 一个 CIDR 网段（如 `10.0.0.0/8`、`198.18.0.0/15` 或单个 IP `127.0.0.1`）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IpCidr {
    V4 { net: Ipv4Addr, prefix: u8 },
    V6 { net: Ipv6Addr, prefix: u8 },
}

impl IpCidr {
    pub fn new_v4(net: Ipv4Addr, prefix: u8) -> Result<Self, String> {
        if prefix > 32 {
            return Err(format!("invalid IPv4 prefix: {prefix} (must be <= 32)"));
        }
        Ok(Self::V4 { net, prefix })
    }

    pub fn new_v6(net: Ipv6Addr, prefix: u8) -> Result<Self, String> {
        if prefix > 128 {
            return Err(format!("invalid IPv6 prefix: {prefix} (must be <= 128)"));
        }
        Ok(Self::V6 { net, prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self, ip) {
            (IpCidr::V4 { net, prefix }, IpAddr::V4(v4)) => {
                in_v4(v4, net.octets(), *prefix as u32)
            }
            (IpCidr::V6 { net, prefix }, IpAddr::V6(v6)) => {
                in_v6(v6, net.octets(), *prefix as u32)
            }
            _ => false,
        }
    }
}

impl std::str::FromStr for IpCidr {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if let Some((ip_str, prefix_str)) = s.split_once('/') {
            let prefix: u8 = prefix_str
                .parse()
                .map_err(|_| format!("invalid CIDR prefix in {s:?}"))?;
            if let Ok(v4) = ip_str.parse::<Ipv4Addr>() {
                Self::new_v4(v4, prefix)
            } else if let Ok(v6) = ip_str.parse::<Ipv6Addr>() {
                Self::new_v6(v6, prefix)
            } else {
                Err(format!("invalid IP address in CIDR {s:?}"))
            }
        } else if let Ok(v4) = s.parse::<Ipv4Addr>() {
            Self::new_v4(v4, 32)
        } else if let Ok(v6) = s.parse::<Ipv6Addr>() {
            Self::new_v6(v6, 128)
        } else {
            Err(format!("invalid CIDR or IP literal: {s:?}"))
        }
    }
}

impl SsrfPolicy {
    pub fn check_url(&self, url: &str) -> Result<Url, Blocked> {
        let parsed = Url::parse(url).map_err(|_| Blocked::InvalidUrl)?;
        if let Some(raw) = raw_host(url)
            && let Some(Host::Ipv4(ip)) = parsed.host()
        {
            let raw = raw.to_ascii_lowercase();
            if raw != ip.to_string() {
                return Err(Blocked::ObfuscatedIp(raw));
            }
        }
        self.check_parsed(&parsed)?;
        Ok(parsed)
    }

    /// 校验已解析的 URL（不检查原始写法，重定向的 Location 也先走 [`Self::check_url`]）。
    pub fn check_parsed(&self, url: &Url) -> Result<(), Blocked> {
        match url.scheme() {
            "http" | "https" => {}
            other => return Err(Blocked::Scheme(other.to_string())),
        }
        match url.host() {
            None => Err(Blocked::InvalidUrl),
            Some(Host::Ipv4(ip)) => self.check_ip(IpAddr::V4(ip)),
            Some(Host::Ipv6(ip)) => self.check_ip(IpAddr::V6(ip)),
            Some(Host::Domain(name)) => {
                if self.allow_private_network || self.host_allowed(name) || !is_internal_name(name)
                {
                    Ok(())
                } else {
                    Err(Blocked::Hostname(name.to_string()))
                }
            }
        }
    }

    /// 主机名在 `allow_hosts` 里。
    pub fn host_allowed(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.');
        self.allow_hosts
            .iter()
            .any(|h| h.eq_ignore_ascii_case(host))
    }

    pub fn check_ip(&self, ip: IpAddr) -> Result<(), Blocked> {
        if self.allow_private_network
            || self.allow_ips.contains(&ip)
            || self.allow_cidrs.iter().any(|c| c.contains(ip))
            || is_public(ip)
        {
            Ok(())
        } else {
            Err(Blocked::Address(ip))
        }
    }
}

/// 按默认策略（不放开内网）校验 URL，给浏览器渲染的子请求拦截复用。
pub fn check_url(url: &str) -> Result<(), Blocked> {
    SsrfPolicy::default().check_url(url).map(|_| ())
}

/// 取出 URL 里原样书写的主机部分，用来识别被 URL 解析器规范化掉的混淆写法。
fn raw_host(url: &str) -> Option<&str> {
    let rest = &url[url.find("://")? + 3..];
    let end = rest.find(['/', '\\', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if host.starts_with('[') {
        return None;
    }
    Some(host.rsplit_once(':').map_or(host, |(h, _)| h))
}

fn is_internal_name(name: &str) -> bool {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    const SUFFIXES: [&str; 4] = ["localhost", "local", "internal", "home.arpa"];
    SUFFIXES
        .iter()
        .any(|s| name == *s || name.ends_with(&format!(".{s}")))
}

/// 是否公网单播地址。
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn in_v4(ip: Ipv4Addr, net: [u8; 4], prefix: u32) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    u32::from(ip) & mask == u32::from(Ipv4Addr::from(net)) & mask
}

fn in_v6(ip: Ipv6Addr, net: [u8; 16], prefix: u32) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    };
    u128::from(ip) & mask == u128::from(Ipv6Addr::from(net)) & mask
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    const DENY: [([u8; 4], u32); 16] = [
        ([0, 0, 0, 0], 8),       // 本网络
        ([10, 0, 0, 0], 8),      // 私网
        ([100, 64, 0, 0], 10),   // 运营商 NAT（含阿里云元数据 100.100.100.200）
        ([127, 0, 0, 0], 8),     // 回环
        ([169, 254, 0, 0], 16),  // 链路本地（含云元数据 169.254.169.254）
        ([172, 16, 0, 0], 12),   // 私网
        ([192, 0, 0, 0], 24),    // IETF 协议分配
        ([192, 0, 2, 0], 24),    // 文档
        ([192, 31, 196, 0], 24), // AS112
        ([192, 52, 193, 0], 24), // AMT
        ([192, 88, 99, 0], 24),  // 6to4 中继
        ([192, 168, 0, 0], 16),  // 私网
        ([198, 18, 0, 0], 15),   // 基准测试
        ([198, 51, 100, 0], 24), // 文档
        ([203, 0, 113, 0], 24),  // 文档
        ([224, 0, 0, 0], 3),     // 组播、保留、广播
    ];
    !DENY.iter().any(|&(net, p)| in_v4(ip, net, p))
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let seg = ip.segments();
    // 公网单播都在 2000::/3 里；NAT64 例外，按内嵌的 IPv4 判断。
    if seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return is_public_v4(embedded_v4(seg[6], seg[7]));
    }
    if seg[0] & 0xe000 != 0x2000 {
        return false;
    }
    match seg[0] {
        // IETF 协议分配（含 Teredo 2001::/32）和文档地址
        0x2001 if seg[1] < 0x0200 || seg[1] == 0x0db8 => false,
        // 6to4：按内嵌的 IPv4 判断
        0x2002 => is_public_v4(embedded_v4(seg[1], seg[2])),
        // 文档 3fff::/20
        0x3fff if seg[1] < 0x1000 => false,
        _ => true,
    }
}

fn embedded_v4(hi: u16, lo: u16) -> Ipv4Addr {
    Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked(url: &str) -> bool {
        check_url(url).is_err()
    }

    #[test]
    fn rejects_non_http_schemes() {
        for url in [
            "file:///etc/passwd",
            "ftp://example.com/",
            "gopher://example.com/",
            "javascript:alert(1)",
            "data:text/html,hi",
        ] {
            assert!(blocked(url), "{url}");
        }
        assert!(!blocked("https://example.com/"));
        assert!(!blocked("http://example.com:8080/a?b#c"));
    }

    #[test]
    fn rejects_obfuscated_ipv4_literals() {
        for url in [
            "http://2130706433/",       // 十进制
            "http://017700000001/",     // 八进制
            "http://0177.0.0.1/",       // 八进制分段
            "http://0x7f000001/",       // 十六进制
            "http://0x7f.0x0.0x0.0x1/", // 十六进制分段
            "http://127.1/",            // 省略段
            "http://127.0.0.1./",       // 末尾点
            "http://%31%32%37.0.0.1/",  // 百分号编码
            "http://134744072/",        // 公网地址 8.8.8.8 的十进制写法也拒绝
        ] {
            assert!(
                matches!(
                    SsrfPolicy::default().check_url(url),
                    Err(Blocked::ObfuscatedIp(_))
                ),
                "{url}"
            );
        }
    }

    #[test]
    fn rejects_private_ipv4() {
        for url in [
            "http://127.0.0.1/",
            "http://10.1.2.3/",
            "http://172.16.0.1/",
            "http://172.31.255.255/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://100.64.0.1/",
            "http://100.100.100.200/",
            "http://0.0.0.0/",
            "http://255.255.255.255/",
            "http://224.0.0.1/",
            "http://user:pw@127.0.0.1:8080/",
        ] {
            assert!(blocked(url), "{url}");
        }
        assert!(!blocked("http://8.8.8.8/"));
        assert!(!blocked("http://172.32.0.1/"));
    }

    #[test]
    fn rejects_private_ipv6() {
        for url in [
            "http://[::1]/",
            "http://[::]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[::ffff:8.8.8.8]/", // 映射地址一律拒绝
            "http://[::127.0.0.1]/",
            "http://[fe80::1]/",
            "http://[fc00::1]/",
            "http://[fd00:ec2::254]/", // AWS IPv6 元数据
            "http://[ff02::1]/",
            "http://[2001:db8::1]/",
            "http://[2001::1]/", // Teredo
            "http://[2002:7f00:1::]/",
            "http://[64:ff9b::a00:1]/",
        ] {
            assert!(blocked(url), "{url}");
        }
        assert!(!blocked("http://[2606:4700::1111]/"));
        assert!(!blocked("http://[64:ff9b::808:808]/"));
        assert!(!blocked("http://[2002:808:808::]/"));
    }

    #[test]
    fn rejects_internal_names() {
        for url in [
            "http://localhost/",
            "http://LOCALHOST./",
            "http://app.localhost/",
            "http://metadata.google.internal/",
            "http://printer.local/",
        ] {
            assert!(blocked(url), "{url}");
        }
        assert!(!blocked("http://localhost.example.com/"));
    }

    #[test]
    fn allow_private_network_opens_ranges_but_not_obfuscation() {
        let p = SsrfPolicy {
            allow_private_network: true,
            ..Default::default()
        };
        assert!(p.check_url("http://127.0.0.1/").is_ok());
        assert!(p.check_url("http://localhost/").is_ok());
        assert!(p.check_url("http://2130706433/").is_err());
        assert!(p.check_url("file:///etc/passwd").is_err());

        let one = SsrfPolicy {
            allow_ips: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ..Default::default()
        };
        assert!(one.check_url("http://127.0.0.1:9/").is_ok());
        assert!(one.check_url("http://127.0.0.2/").is_err());
    }

    #[test]
    fn allow_hosts_opens_only_the_named_hosts() {
        let p = SsrfPolicy {
            allow_hosts: vec!["LocalHost".into()],
            ..Default::default()
        };
        assert!(p.check_url("http://localhost:8080/x").is_ok());
        assert!(p.check_url("http://localhost./x").is_ok());
        assert!(p.host_allowed("localhost"));
        assert!(!p.host_allowed("other.localhost"));
        assert!(p.check_url("http://127.0.0.1/").is_err());
        assert!(
            SsrfPolicy::default()
                .check_url("http://localhost/")
                .is_err()
        );
    }

    #[test]
    fn allow_cidrs_opens_subnets_and_fake_ip() {
        let p = SsrfPolicy {
            allow_cidrs: vec![
                "10.50.0.0/16".parse().unwrap(),
                "198.18.0.0/15".parse().unwrap(),
                "fd00::/8".parse().unwrap(),
            ],
            ..Default::default()
        };
        // Allowed subnets
        assert!(p.check_url("http://10.50.1.2:8080/").is_ok());
        assert!(p.check_url("http://198.18.0.1/").is_ok()); // Fake-IP
        assert!(p.check_url("http://198.19.255.254/").is_ok()); // Fake-IP /15
        assert!(p.check_url("http://[fd00::1]/").is_ok());

        // Still blocked: 127.0.0.1, 169.254.169.254, other 10.x subnets
        assert!(p.check_url("http://127.0.0.1/").is_err());
        assert!(p.check_url("http://169.254.169.254/").is_err());
        assert!(p.check_url("http://10.60.0.1/").is_err());
    }
}
