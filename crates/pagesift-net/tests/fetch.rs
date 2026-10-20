mod common;

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use common::{FakeDns, ok, redirect, response, serve};
use pagesift_net::{
    Blocked, BoxError, BoxFuture, DnsLookup, Error, FetchRequest, Fetcher, FetcherConfig,
    HostCredentials, RedirectGuard, SsrfPolicy, TimeoutPhase, Url,
};

/// 只放行测试服务器所在的 127.0.0.1，其余内网地址照常拦截。
fn config(dns: Arc<dyn DnsLookup>) -> FetcherConfig {
    FetcherConfig {
        ssrf: SsrfPolicy {
            allow_ips: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ..Default::default()
        },
        dns,
        ..Default::default()
    }
}

fn fetcher() -> Fetcher {
    Fetcher::new(config(FakeDns::new(&[]))).unwrap()
}

#[tokio::test]
async fn fetches_and_decodes() {
    let srv = serve(|_| {
        let gbk = encoding_rs::GBK.encode("<p>你好</p>").0.into_owned();
        Some(response(
            200,
            &[
                ("Content-Type", "text/html; charset=gbk"),
                ("ETag", "\"v1\""),
            ],
            &gbk,
        ))
    })
    .await;
    let r = fetcher()
        .fetch(FetchRequest::new(srv.url("/a")))
        .await
        .unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.text().text, "<p>你好</p>");
    assert_eq!(r.etag(), Some("\"v1\""));
    assert!(!r.truncated && !r.not_modified && r.redirect_chain.is_empty());
    assert!(r.timings.total >= r.timings.first_byte);
    let ua = srv.seen()[0].header("user-agent").unwrap().to_string();
    assert!(ua.starts_with("pagesift/"), "{ua}");
}

#[tokio::test]
async fn decompresses_gzip() {
    let srv = serve(|s| {
        assert!(s.header("accept-encoding").unwrap().contains("gzip"));
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(b"hello gzip").unwrap();
        let body = enc.finish().unwrap();
        Some(response(200, &[("Content-Encoding", "gzip")], &body))
    })
    .await;
    let r = fetcher()
        .fetch(FetchRequest::new(srv.url("/")))
        .await
        .unwrap();
    assert_eq!(r.body, b"hello gzip");
}

#[tokio::test]
async fn truncates_large_bodies() {
    let srv = serve(|_| Some(ok(&"x".repeat(100_000)))).await;
    let mut req = FetchRequest::new(srv.url("/"));
    req.max_body_bytes = Some(1000);
    let r = fetcher().fetch(req).await.unwrap();
    assert!(r.truncated);
    assert_eq!(r.body.len(), 1000);
}

#[tokio::test]
async fn conditional_request_reports_not_modified() {
    let srv = serve(|s| {
        assert_eq!(s.header("if-none-match"), Some("\"v1\""));
        assert!(s.header("if-modified-since").unwrap().ends_with("GMT"));
        Some(response(304, &[("ETag", "\"v1\"")], b""))
    })
    .await;
    let req = FetchRequest::new(srv.url("/"))
        .if_none_match("\"v1\"")
        .if_modified_since(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000));
    let r = fetcher().fetch(req).await.unwrap();
    assert!(r.not_modified);
    assert_eq!(r.status, 304);
}

#[tokio::test]
async fn follows_and_records_redirects() {
    let srv = serve(|s| {
        Some(match s.path.as_str() {
            "/1" => redirect(301, "/2"),
            "/2" => redirect(302, "/3"),
            _ => ok("done"),
        })
    })
    .await;
    let r = fetcher()
        .fetch(FetchRequest::new(srv.url("/1")))
        .await
        .unwrap();
    assert_eq!(r.final_url.path(), "/3");
    let chain: Vec<_> = r
        .redirect_chain
        .iter()
        .map(|h| (h.url.path(), h.status))
        .collect();
    assert_eq!(chain, [("/1", 301), ("/2", 302)]);
}

#[tokio::test]
async fn caps_redirects() {
    let srv = serve(|_| Some(redirect(302, "/loop"))).await;
    let err = fetcher()
        .fetch(FetchRequest::new(srv.url("/")))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::TooManyRedirects(5)), "{err}");
    assert_eq!(srv.seen().len(), 6);
}

#[tokio::test]
async fn redirect_to_internal_address_is_blocked() {
    for target in [
        "http://169.254.169.254/latest/meta-data/",
        "http://10.0.0.1/",
        "http://[::ffff:127.0.0.1]/",
        "http://2130706433/",
        "http://localhost/",
        "file:///etc/passwd",
    ] {
        let t = target.to_string();
        let srv = serve(move |_| Some(redirect(302, &t))).await;
        let err = fetcher()
            .fetch(FetchRequest::new(srv.url("/")))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Blocked(_)), "{target}: {err}");
        assert_eq!(srv.seen().len(), 1);
    }
}

#[tokio::test]
async fn redirect_to_name_resolving_private_is_blocked() {
    let srv = serve(|_| Some(redirect(302, "http://evil.test/"))).await;
    let dns = FakeDns::new(&[("evil.test", &["192.168.1.10"])]);
    let f = Fetcher::new(config(dns)).unwrap();
    let err = f.fetch(FetchRequest::new(srv.url("/"))).await.unwrap_err();
    assert!(
        matches!(err, Error::Blocked(Blocked::Address(ip)) if ip.to_string() == "192.168.1.10"),
        "{err}"
    );
}

#[tokio::test]
async fn dns_rebinding_with_one_private_answer_is_blocked() {
    let dns = FakeDns::new(&[
        ("rebind.test", &["93.184.216.34", "127.0.0.2"]),
        ("v6.test", &["2606:4700::1111", "::ffff:10.0.0.1"]),
    ]);
    let f = Fetcher::new(config(dns)).unwrap();
    for url in ["http://rebind.test/", "http://v6.test/"] {
        let err = f.fetch(FetchRequest::new(url)).await.unwrap_err();
        assert!(
            matches!(err, Error::Blocked(Blocked::Address(_))),
            "{url}: {err}"
        );
    }
}

#[tokio::test]
async fn rejects_ip_literals_and_schemes_up_front() {
    let f = fetcher();
    for url in [
        "http://0x7f000001/",
        "http://017700000001/",
        "http://10.0.0.1/",
        "ftp://example.com/",
    ] {
        let err = f.fetch(FetchRequest::new(url)).await.unwrap_err();
        assert!(matches!(err, Error::Blocked(_)), "{url}: {err}");
    }
    assert!(matches!(
        f.fetch(FetchRequest::new("not a url")).await,
        Err(Error::InvalidUrl(_))
    ));
}

#[tokio::test]
async fn credentials_stay_on_their_host() {
    let other = serve(|_| Some(ok("other"))).await;
    let port = other.addr.port();
    let origin = serve(move |s| {
        Some(match s.path.as_str() {
            "/start" => redirect(302, "/same"),
            "/same" => redirect(302, &format!("http://other.test:{port}/x")),
            _ => ok("?"),
        })
    })
    .await;
    let mut cfg = config(FakeDns::new(&[("other.test", &["127.0.0.1"])]));
    cfg.credentials = vec![
        HostCredentials::new("127.0.0.1")
            .header("Authorization", "Bearer secret-token")
            .cookie("sid", "s3cr3t"),
    ];
    let f = Fetcher::new(cfg).unwrap();
    let req = FetchRequest::new(origin.url("/start"))
        .header("X-Api-Key", "k-123")
        .header("Cookie", "pref=1");
    let r = f.fetch(req).await.unwrap();
    assert_eq!(r.body, b"other");

    for s in origin.seen() {
        assert_eq!(s.header("authorization"), Some("Bearer secret-token"));
        assert_eq!(s.header("x-api-key"), Some("k-123"));
        assert_eq!(s.header("cookie"), Some("sid=s3cr3t; pref=1"));
    }
    let leaked = &other.seen()[0];
    for h in ["authorization", "x-api-key", "cookie"] {
        assert_eq!(leaked.header(h), None, "{h} leaked");
    }
}

#[test]
fn debug_output_redacts_secrets() {
    let mut cfg = FetcherConfig {
        proxy: Some("http://user:hunter2@proxy.corp:3128".into()),
        ..Default::default()
    };
    cfg.credentials = vec![HostCredentials::new("a.com").header("Authorization", "Bearer zzz")];
    let req = FetchRequest::new("https://u:pw@a.com/").header("X-Api-Key", "k-123");
    let s = format!("{cfg:?} {req:?}");
    for secret in ["hunter2", "zzz", "k-123", ":pw@"] {
        assert!(!s.contains(secret), "{secret} in {s}");
    }
}

#[tokio::test]
async fn rejects_browser_user_agents() {
    let cfg = FetcherConfig {
        user_agent: "Mozilla/5.0 (X11; Linux x86_64) Chrome/120".into(),
        ..Default::default()
    };
    assert!(matches!(Fetcher::new(cfg), Err(Error::UserAgent(_))));
    let err = fetcher()
        .fetch(FetchRequest::new("https://example.com/").header("User-Agent", "Mozilla/5.0"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::UserAgent(_)));
    let err = fetcher()
        .fetch(FetchRequest::new("https://example.com/").header("Host", "internal"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Header(_)));
}

#[test]
fn rejects_remote_dns_proxies() {
    for p in ["socks5h://proxy:1080", "socks4://proxy:1080", "ftp://proxy"] {
        let cfg = FetcherConfig {
            proxy: Some(p.into()),
            ..Default::default()
        };
        assert!(matches!(Fetcher::new(cfg), Err(Error::Config(_))), "{p}");
    }
    for p in ["http://proxy.corp:3128", "socks5://10.0.0.5:1080"] {
        let cfg = FetcherConfig {
            proxy: Some(p.into()),
            ..Default::default()
        };
        Fetcher::new(cfg).unwrap();
    }
}

#[tokio::test]
async fn http_proxy_on_private_address_is_allowed() {
    // 代理本身在内网是正常的企业环境；目标地址仍按 URL 校验。
    let proxy = serve(|s| {
        assert!(s.path.starts_with("http://example.com/"), "{}", s.path);
        Some(ok("via proxy"))
    })
    .await;
    let cfg = FetcherConfig {
        proxy: Some(format!("http://proxy.test:{}", proxy.addr.port())),
        dns: FakeDns::new(&[("proxy.test", &["127.0.0.1"])]),
        ..Default::default()
    };
    let f = Fetcher::new(cfg).unwrap();
    let r = f
        .fetch(FetchRequest::new("http://example.com/page"))
        .await
        .unwrap();
    assert_eq!(r.body, b"via proxy");
    let err = f
        .fetch(FetchRequest::new("http://10.1.1.1/"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Blocked(_)));
}

#[tokio::test]
async fn first_byte_timeout() {
    let srv = serve(|_| None).await;
    let cfg = FetcherConfig {
        first_byte_timeout: Duration::from_millis(200),
        ..config(FakeDns::new(&[]))
    };
    let err = Fetcher::new(cfg)
        .unwrap()
        .fetch(FetchRequest::new(srv.url("/")))
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Timeout(TimeoutPhase::FirstByte)),
        "{err}"
    );

    let mut req = FetchRequest::new(srv.url("/"));
    req.timeout = Some(Duration::from_millis(100));
    let err = fetcher().fetch(req).await.unwrap_err();
    assert!(matches!(err, Error::Timeout(TimeoutPhase::Total)), "{err}");
}

struct DenyPath(&'static str);

impl RedirectGuard for DenyPath {
    fn check<'a>(&'a self, _: &'a Url, to: &'a Url) -> BoxFuture<'a, Result<(), BoxError>> {
        Box::pin(async move {
            if to.path() == self.0 {
                Err("denied by test".into())
            } else {
                Ok(())
            }
        })
    }
}

#[tokio::test]
async fn redirect_guard_can_reject_a_hop() {
    let srv = serve(|s| {
        Some(match s.path.as_str() {
            "/" => redirect(307, "/private"),
            _ => ok("no"),
        })
    })
    .await;
    let err = fetcher()
        .fetch_with(FetchRequest::new(srv.url("/")), &DenyPath("/private"))
        .await
        .unwrap_err();
    match err {
        Error::RedirectRejected { url, source } => {
            assert!(url.ends_with("/private"));
            assert_eq!(source.to_string(), "denied by test");
        }
        e => panic!("{e}"),
    }
    assert_eq!(srv.seen().len(), 1);
}
