//! 代理探测与 NO_PROXY 绕行名单。

use super::*;

pub(super) fn detect_env_proxy() -> Option<String> {
    for var in [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ] {
        if let Ok(val) = std::env::var(var) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// 哪些目标应该绕过代理直连。
///
/// 来源：标准 `NO_PROXY`/`no_proxy` 环境变量，加上配置里的 `allow_hosts`、`allow_ips`
/// 和 `allow_cidrs`。`allow_private_network` 打开时，非公网 IP 字面量也直连。
/// 域名先按名单匹配；只有显式配置了 `allow_cidrs`/`allow_ips` 时才在本地解析、
/// 看结果是否落在这些网段里（见 [`Fetcher::direct_route`]）。
#[derive(Debug, Default, Clone)]
pub(super) struct ProxyBypass {
    pub(super) all: bool,
    pub(super) raw: Option<String>,
    pub(super) hosts: Vec<String>,
    pub(super) ips: Vec<IpAddr>,
    pub(super) cidrs: Vec<IpCidr>,
    pub(super) allow_private_network: bool,
    /// 用户显式配置了 allow_cidrs / allow_ips：未列名的域名需要本地解析一次，
    /// 解析到这些网段就直连（`--allow-cidr 10.0.0.0/8` 抓 `http://wiki.corp`）。
    pub(super) resolve_hosts: bool,
}

impl ProxyBypass {
    pub(super) fn from_env(ssrf: &SsrfPolicy) -> Self {
        let raw = std::env::var("NO_PROXY")
            .ok()
            .or_else(|| std::env::var("no_proxy").ok());
        Self::from_raw(raw.as_deref(), ssrf)
    }

    pub(super) fn from_raw(raw: Option<&str>, ssrf: &SsrfPolicy) -> Self {
        let mut bypass = Self {
            raw: raw.map(str::to_string),
            allow_private_network: ssrf.allow_private_network,
            ips: ssrf.allow_ips.clone(),
            cidrs: ssrf.allow_cidrs.clone(),
            resolve_hosts: !ssrf.allow_cidrs.is_empty() || !ssrf.allow_ips.is_empty(),
            ..Default::default()
        };
        if let Some(raw) = raw {
            for entry in raw.split([',', ';', ' ', '\t', '\n']) {
                let entry = entry.trim();
                if entry.is_empty() {
                    continue;
                }
                if entry == "*" {
                    bypass.all = true;
                    continue;
                }
                if let Ok(cidr) = entry.parse::<IpCidr>() {
                    push_unique(&mut bypass.cidrs, cidr, |a, b| a == b);
                    continue;
                }
                let host = host_part(entry);
                let host = host.trim_start_matches("*.").trim_start_matches('.');
                if host.is_empty() {
                    continue;
                }
                if let Ok(ip) = host.parse::<IpAddr>() {
                    push_unique(&mut bypass.ips, ip, |a, b| a == b);
                } else {
                    push_unique(&mut bypass.hosts, host.to_ascii_lowercase(), |a, b| a == b);
                }
            }
        }
        for host in &ssrf.allow_hosts {
            let host = host_part(host)
                .trim_start_matches("*.")
                .trim_start_matches('.');
            if !host.is_empty() {
                push_unique(&mut bypass.hosts, host.to_ascii_lowercase(), |a, b| a == b);
            }
        }
        bypass
    }

    /// 只看 URL 本身判断是否直连，不做 DNS 解析：IP 字面量按放行规则判断，
    /// 域名按 NO_PROXY / allow_hosts 名单匹配。
    pub(super) fn applies(&self, url: &Url) -> bool {
        if self.all {
            return true;
        }
        match url.host() {
            Some(Host::Ipv4(ip)) => self.ip_allowed(IpAddr::V4(ip)),
            Some(Host::Ipv6(ip)) => self.ip_allowed(IpAddr::V6(ip)),
            Some(Host::Domain(host)) => self.host_listed(host),
            None => false,
        }
    }

    /// [`Self::applies`] 判定走代理后，是否还要在本地解析这个域名再判断一次。
    ///
    /// 只有显式配置了 allow_cidrs / allow_ips 才需要；只开 `allow_private_network`
    /// 或什么都没配时域名直接交给代理：socks5h 用户不能在本地泄露 DNS 查询，
    /// 只能经代理出网的环境也不该每个请求都等本地 DNS。
    pub(super) fn resolution_target<'a>(&self, url: &'a Url) -> Option<&'a str> {
        if !self.resolve_hosts {
            return None;
        }
        match url.host() {
            Some(Host::Domain(host)) => Some(host),
            _ => None,
        }
    }

    /// 解析结果是否落在显式放行的地址/网段里（不含 allow_private_network 总开关）。
    pub(super) fn resolved_allowed(&self, ips: &[IpAddr]) -> bool {
        ips.iter()
            .any(|ip| self.ips.contains(ip) || self.cidrs.iter().any(|c| c.contains(*ip)))
    }

    pub(super) fn ip_allowed(&self, ip: IpAddr) -> bool {
        self.ips.contains(&ip)
            || self.cidrs.iter().any(|cidr| cidr.contains(ip))
            || (self.allow_private_network && !is_public(ip))
    }

    pub(super) fn host_listed(&self, host: &str) -> bool {
        let host = host
            .trim_matches(['[', ']'])
            .trim_end_matches('.')
            .to_ascii_lowercase();
        self.hosts
            .iter()
            .any(|rule| host == *rule || host.ends_with(&format!(".{rule}")))
    }

    pub(super) fn reqwest_no_proxy(&self) -> Option<reqwest::NoProxy> {
        let mut parts = Vec::new();
        if let Some(raw) = &self.raw
            && !raw.trim().is_empty()
        {
            parts.push(raw.clone());
        }
        parts.extend(self.hosts.iter().cloned());
        parts.extend(self.ips.iter().map(ToString::to_string));
        parts.extend(self.cidrs.iter().map(cidr_entry));
        if parts.is_empty() {
            return None;
        }
        reqwest::NoProxy::from_string(&parts.join(","))
    }
}

pub(super) fn push_unique<T>(items: &mut Vec<T>, value: T, same: impl Fn(&T, &T) -> bool) {
    if !items.iter().any(|existing| same(existing, &value)) {
        items.push(value);
    }
}

/// `NO_PROXY` 条目可能带协议、路径或端口；只取主机部分。IPv6 用方括号包着。
pub(super) fn host_part(entry: &str) -> &str {
    let entry = entry.split_once("://").map_or(entry, |(_, rest)| rest);
    let entry = entry.split(['/', '?']).next().unwrap_or(entry);
    if let Some(rest) = entry.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(entry);
    }
    if entry.parse::<IpAddr>().is_ok() {
        return entry;
    }
    match entry.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() => host,
        _ => entry,
    }
}

pub(super) fn cidr_entry(cidr: &IpCidr) -> String {
    match cidr {
        IpCidr::V4 { net, prefix } => format!("{net}/{prefix}"),
        IpCidr::V6 { net, prefix } => format!("{net}/{prefix}"),
    }
}
