use super::*;
use std::net::Ipv4Addr;

fn policy() -> SsrfPolicy {
    SsrfPolicy::default()
}

#[test]
fn no_proxy_parses_hosts_ports_and_cidrs() {
    let bypass = ProxyBypass::from_raw(
        Some("localhost, .internal.example, 10.0.0.0/8, 127.0.0.1, [::1]:8080, *.corp.example"),
        &policy(),
    );
    assert!(bypass.host_listed("localhost"));
    assert!(bypass.host_listed("api.internal.example"));
    assert!(!bypass.host_listed("notinternal.example"));
    assert!(bypass.host_listed("api.corp.example"));
    assert!(bypass.ip_allowed("10.1.2.3".parse().unwrap()));
    assert!(bypass.ip_allowed("127.0.0.1".parse().unwrap()));
    assert!(bypass.ip_allowed("::1".parse().unwrap()));
    assert!(!bypass.ip_allowed("8.8.8.8".parse().unwrap()));
}

#[test]
fn wildcard_no_proxy_bypasses_everything() {
    let bypass = ProxyBypass::from_raw(Some("*"), &policy());
    assert!(bypass.all);
}

#[test]
fn explicit_allow_lists_are_bypassed() {
    let ssrf = SsrfPolicy {
        allow_hosts: vec!["docs.internal".into()],
        allow_ips: vec![IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10))],
        allow_cidrs: vec!["198.18.0.0/15".parse().unwrap()],
        ..Default::default()
    };
    let bypass = ProxyBypass::from_raw(None, &ssrf);
    assert!(bypass.host_listed("docs.internal"));
    assert!(bypass.ip_allowed("192.0.2.10".parse().unwrap()));
    assert!(bypass.ip_allowed("198.18.1.2".parse().unwrap()));
    assert!(!bypass.ip_allowed("198.20.1.2".parse().unwrap()));
}

fn url(s: &str) -> Url {
    Url::parse(s).unwrap()
}

#[test]
fn proxied_route_only_bypasses_ip_literals_and_listed_hosts() {
    let ssrf = SsrfPolicy {
        allow_private_network: true,
        allow_hosts: vec!["docs.internal".into()],
        allow_cidrs: vec!["198.18.0.0/15".parse().unwrap()],
        ..Default::default()
    };
    let bypass = ProxyBypass::from_raw(Some(".corp.example"), &ssrf);
    // IP 字面量照常在本地判定
    assert!(bypass.applies(&url("http://10.0.0.5/")));
    assert!(bypass.applies(&url("http://[fd00::1]/")));
    assert!(bypass.applies(&url("http://198.18.1.2/")));
    assert!(!bypass.applies(&url("http://8.8.8.8/")));
    // 名单里的主机名直连
    assert!(bypass.applies(&url("http://docs.internal/a")));
    assert!(bypass.applies(&url("https://git.corp.example/")));
    // 其他主机名不按名单直连；配了 allow_cidrs，所以要本地解析再判断
    assert!(!bypass.applies(&url("http://intranet.example/")));
    assert_eq!(
        bypass.resolution_target(&url("http://intranet.example/")),
        Some("intranet.example")
    );
    assert_eq!(bypass.resolution_target(&url("http://8.8.8.8/")), None);
    assert!(bypass.resolved_allowed(&["198.18.3.4".parse().unwrap()]));
    // allow_private_network 总开关不让解析结果直连
    assert!(!bypass.resolved_allowed(&["10.0.0.5".parse().unwrap()]));
}

#[test]
fn private_network_switch_alone_never_resolves_hosts() {
    let ssrf = SsrfPolicy {
        allow_private_network: true,
        ..Default::default()
    };
    let bypass = ProxyBypass::from_raw(Some("10.0.0.0/8"), &ssrf);
    assert!(bypass.applies(&url("http://192.168.1.1/")));
    assert!(!bypass.applies(&url("http://intranet.example/")));
    assert_eq!(
        bypass.resolution_target(&url("http://intranet.example/")),
        None
    );
    let none = ProxyBypass::from_raw(None, &policy());
    assert_eq!(none.resolution_target(&url("http://example.com/")), None);
}

#[test]
fn no_proxy_rules_still_apply_without_resolution() {
    let bypass = ProxyBypass::from_raw(Some("localhost,10.0.0.0/8"), &policy());
    assert!(bypass.applies(&url("http://localhost:8080/")));
    assert!(bypass.applies(&url("http://10.9.9.9/")));
    assert!(!bypass.applies(&url("http://example.com/")));
    let all = ProxyBypass::from_raw(Some("*"), &policy());
    assert!(all.applies(&url("http://example.com/")));
}

#[test]
fn location_bytes_are_read_as_utf8_or_latin1() {
    let base = url("http://example.com/dir/start");
    let utf8 = location_text("/目标 页面?q=中文#片段".as_bytes());
    let next = base.join(&utf8).unwrap();
    assert_eq!(
        next.as_str(),
        "http://example.com/%E7%9B%AE%E6%A0%87%20%E9%A1%B5%E9%9D%A2?q=%E4%B8%AD%E6%96%87#%E7%89%87%E6%AE%B5"
    );
    // latin-1 字节保留原值
    let latin1 = location_text(b"caf\xe9.html");
    assert_eq!(latin1, "caf%E9.html");
    assert_eq!(
        base.join(&latin1).unwrap().as_str(),
        "http://example.com/dir/caf%E9.html"
    );
    // 普通 ASCII 不变
    assert_eq!(location_text(b"/plain?a=1"), "/plain?a=1");
}

#[test]
fn missing_no_proxy_is_empty() {
    let bypass = ProxyBypass::from_raw(None, &policy());
    assert!(!bypass.all);
    assert!(!bypass.host_listed("example.com"));
    assert!(!bypass.ip_allowed("127.0.0.1".parse().unwrap()));
}
