//! `localhost` 和 `*.localhost` 不查 DNS，直接是回环地址（RFC 6761 §6.3）。
//!
//! 系统解析器对 `foo.localhost` 的处理各不相同：systemd-resolved 和 glibc 的 myhostname 模块会
//! 给回环地址，纯 `files dns` 的 nsswitch（容器、不少服务器）则查不到，只有 IPv4 的机器更不会有
//! `::1`。本地源站、测试、多站点基准都靠这类名字，按标准自己认出来，哪种机器上都一样。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// `host` 是 `localhost` 或它的子域名（大小写、末尾的点都不计）时，给出回环地址：
/// 先 IPv4 再 IPv6，只有 IPv4 的机器直接连上，双栈机器上连不上 `127.0.0.1` 时再试 `::1`。
pub(crate) fn loopback(host: &str) -> Option<Vec<IpAddr>> {
    is_localhost_name(host).then(|| {
        vec![
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ]
    })
}

pub(crate) fn is_localhost_name(host: &str) -> bool {
    let name = host.trim_end_matches('.').as_bytes();
    let Some(at) = name.len().checked_sub(b"localhost".len()) else {
        return false;
    };
    name[at..].eq_ignore_ascii_case(b"localhost") && (at == 0 || name[at - 1] == b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localhost_and_its_subdomains_are_loopback() {
        for host in [
            "localhost",
            "LOCALHOST",
            "localhost.",
            "a.localhost",
            "a.root.localhost.",
            "s1.LocalHost",
        ] {
            assert_eq!(
                loopback(host),
                Some(vec![
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    IpAddr::V6(Ipv6Addr::LOCALHOST)
                ]),
                "{host}"
            );
        }
    }

    #[test]
    fn look_alikes_go_to_dns() {
        for host in [
            "localhost.example.com",
            "notlocalhost",
            "xlocalhost",
            "localhostx",
            "local",
            "",
            "ocalhost",
        ] {
            assert_eq!(loopback(host), None, "{host}");
        }
    }
}
