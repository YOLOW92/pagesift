mod common;

use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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

struct CountingDns {
    calls: AtomicUsize,
    ip: IpAddr,
    transient_failures: usize,
}

impl DnsLookup for CountingDns {
    fn lookup<'a>(&'a self, _host: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            if call < self.transient_failures {
                Err(io::Error::new(io::ErrorKind::TimedOut, "transient DNS"))
            } else {
                Ok(vec![self.ip])
            }
        })
    }
}

#[tokio::test]
async fn dns_singleflight_cache_and_transient_retry_are_bounded() {
    let srv = serve(|_| Some(ok("cached"))).await;
    let dns = Arc::new(CountingDns {
        calls: AtomicUsize::new(0),
        ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
        transient_failures: 1,
    });
    let fetcher = Arc::new(
        Fetcher::new(FetcherConfig {
            dns: dns.clone(),
            dns_transient_retries: 1,
            ssrf: SsrfPolicy {
                allow_hosts: vec!["singleflight.test".into()],
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap(),
    );
    let mut tasks = tokio::task::JoinSet::new();
    for n in 0..12 {
        let fetcher = fetcher.clone();
        let url = format!("http://singleflight.test:{}/page/{n}", srv.addr.port());
        tasks.spawn(async move { fetcher.fetch(FetchRequest::new(url)).await });
    }
    while let Some(result) = tasks.join_next().await {
        assert_eq!(result.unwrap().unwrap().body, b"cached");
    }
    assert_eq!(
        dns.calls.load(Ordering::SeqCst),
        2,
        "one retry, then cached"
    );
    let dns_stage = fetcher
        .stage_snapshots()
        .into_iter()
        .find(|snapshot| snapshot.stage == pagesift_net::NetworkStage::Dns)
        .unwrap();
    assert_eq!(dns_stage.completed, 12);
    assert_eq!(dns_stage.transient_failures, 0);
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

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

#[tokio::test]
async fn decompresses_stacked_gzip() {
    let srv = serve(|_| {
        let body = gzip(&gzip(b"<p>double</p>"));
        Some(response(200, &[("Content-Encoding", "gzip, gzip")], &body))
    })
    .await;
    let r = fetcher()
        .fetch(FetchRequest::new(srv.url("/")))
        .await
        .unwrap();
    assert_eq!(r.body, b"<p>double</p>");
    assert!(!r.truncated);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert!(r.header("content-encoding").is_none());
}

#[tokio::test]
async fn plaintext_labelled_gzip_is_used_as_is() {
    let srv = serve(|_| {
        Some(response(
            200,
            &[("Content-Encoding", "gzip"), ("Content-Type", "text/html")],
            b"<html>plain</html>",
        ))
    })
    .await;
    let r = fetcher()
        .fetch(FetchRequest::new(srv.url("/")))
        .await
        .unwrap();
    assert_eq!(r.body, b"<html>plain</html>");
    assert!(!r.truncated, "not a size-limit problem");
    assert_eq!(
        r.warnings,
        ["content_encoding_mismatch: declared gzip but body is not gzip; used as-is"]
    );
}

#[tokio::test]
async fn follows_non_ascii_location() {
    let srv = serve(|s| {
        Some(match s.path.as_str() {
            "/start" => redirect(302, "/目标 页面?q=中文#片段"),
            "/%E7%9B%AE%E6%A0%87%20%E9%A1%B5%E9%9D%A2?q=%E4%B8%AD%E6%96%87" => ok("arrived"),
            other => response(404, &[], other.as_bytes()),
        })
    })
    .await;
    let r = fetcher()
        .fetch(FetchRequest::new(srv.url("/start")))
        .await
        .unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.body, b"arrived");
    assert_eq!(r.final_url.fragment(), Some("%E7%89%87%E6%AE%B5"));
    assert_eq!(r.redirect_chain.len(), 1);
}

#[tokio::test]
async fn non_ascii_location_keeps_ssrf_and_redirect_limits() {
    let srv = serve(|_| Some(redirect(302, "http://10.0.0.1/内网"))).await;
    let err = fetcher()
        .fetch(FetchRequest::new(srv.url("/")))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Blocked(_)), "{err}");
    assert_eq!(srv.seen().len(), 1);

    let srv = serve(|_| Some(redirect(302, "/循环 页面"))).await;
    let err = fetcher()
        .fetch(FetchRequest::new(srv.url("/")))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::TooManyRedirects(5)), "{err}");
    assert_eq!(srv.seen().len(), 6);
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
async fn allowlisted_host_may_resolve_to_a_private_address() {
    let srv = serve(|_| Some(ok("<p>internal</p>"))).await;
    let port = Url::parse(&srv.url("/")).unwrap().port().unwrap();
    let dns = FakeDns::new(&[("svc.test", &["127.0.0.1"]), ("other.test", &["127.0.0.1"])]);
    let f = Fetcher::new(FetcherConfig {
        ssrf: SsrfPolicy {
            allow_hosts: vec!["svc.test".into()],
            ..Default::default()
        },
        dns,
        ..Default::default()
    })
    .unwrap();
    let resp = f
        .fetch(FetchRequest::new(format!("http://svc.test:{port}/")))
        .await
        .unwrap();
    assert_eq!(resp.status, 200);
    let err = f
        .fetch(FetchRequest::new(format!("http://other.test:{port}/")))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Blocked(Blocked::Address(_))), "{err}");
}

#[tokio::test]
async fn unresolvable_host_is_a_dns_failure_not_an_ssrf_block() {
    let f = fetcher();
    let err = f
        .fetch(FetchRequest::new("http://nowhere.test/"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Dns(_)), "{err}");
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
fn rejects_invalid_proxy_schemes() {
    for p in ["socks4://proxy:1080", "ftp://proxy"] {
        let cfg = FetcherConfig {
            proxy: Some(p.into()),
            ..Default::default()
        };
        assert!(matches!(Fetcher::new(cfg), Err(Error::Config(_))), "{p}");
    }
    for p in [
        "http://proxy.corp:3128",
        "socks5://10.0.0.5:1080",
        "socks5h://proxy.corp:1080",
    ] {
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

/// 记录每次被查询的主机名；`hang` 里的主机永不返回（模拟只能经代理出网的环境）。
struct RecordingDns {
    asked: std::sync::Mutex<Vec<String>>,
    answers: Vec<(&'static str, IpAddr)>,
    hang: Vec<&'static str>,
}

impl RecordingDns {
    fn new(answers: &[(&'static str, IpAddr)], hang: &[&'static str]) -> Arc<Self> {
        let mut answers = answers.to_vec();
        answers.push(("proxy.test", IpAddr::V4(Ipv4Addr::LOCALHOST)));
        Arc::new(Self {
            asked: Default::default(),
            answers,
            hang: hang.to_vec(),
        })
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl DnsLookup for RecordingDns {
    fn lookup<'a>(&'a self, host: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        Box::pin(async move {
            self.asked.lock().unwrap().push(host.to_string());
            if self.hang.contains(&host) {
                std::future::pending::<()>().await;
            }
            match self.answers.iter().find(|(h, _)| *h == host) {
                Some((_, ip)) => Ok(vec![*ip]),
                None => Err(io::Error::new(io::ErrorKind::NotFound, "no such host")),
            }
        })
    }
}

/// 代理服务器：只接受绝对形式的请求行，回 "via proxy"。
async fn proxy_server() -> common::Server {
    serve(|s| {
        assert!(s.path.starts_with("http://"), "{}", s.path);
        Some(ok("via proxy"))
    })
    .await
}

fn proxied_config(
    proxy: &common::Server,
    dns: Arc<RecordingDns>,
    ssrf: SsrfPolicy,
) -> FetcherConfig {
    FetcherConfig {
        proxy: Some(format!("http://proxy.test:{}", proxy.addr.port())),
        dns,
        ssrf,
        ..Default::default()
    }
}

#[tokio::test]
async fn proxy_with_private_network_switch_does_not_resolve_targets_locally() {
    let proxy = proxy_server().await;
    let dns = RecordingDns::new(&[], &[]);
    let ssrf = SsrfPolicy {
        allow_private_network: true,
        ..Default::default()
    };
    let f = Fetcher::new(proxied_config(&proxy, dns.clone(), ssrf)).unwrap();
    let r = f
        .fetch(FetchRequest::new("http://example.com/page"))
        .await
        .unwrap();
    assert_eq!(r.body, b"via proxy");
    let asked = dns.asked();
    assert!(
        !asked.iter().any(|h| h == "example.com"),
        "target must be resolved by the proxy, not locally: {asked:?}"
    );
}

#[tokio::test]
async fn proxy_with_allow_cidr_goes_direct_when_host_resolves_inside() {
    let proxy = proxy_server().await;
    let target = serve(|s| {
        assert_eq!(s.path, "/page", "direct request uses origin-form");
        Some(ok("direct"))
    })
    .await;
    let dns = RecordingDns::new(&[("wiki.corp", IpAddr::V4(Ipv4Addr::LOCALHOST))], &[]);
    let ssrf = SsrfPolicy {
        allow_cidrs: vec!["127.0.0.0/8".parse().unwrap()],
        ..Default::default()
    };
    let f = Fetcher::new(proxied_config(&proxy, dns, ssrf)).unwrap();
    let r = f
        .fetch(FetchRequest::new(format!(
            "http://wiki.corp:{}/page",
            target.addr.port()
        )))
        .await
        .unwrap();
    assert_eq!(r.body, b"direct");
    assert!(proxy.seen().is_empty());
}

#[tokio::test]
async fn proxy_with_allow_cidr_falls_back_to_proxy_when_resolution_fails() {
    let proxy = proxy_server().await;
    let dns = RecordingDns::new(&[], &["slow.example"]);
    let ssrf = SsrfPolicy {
        allow_cidrs: vec!["10.0.0.0/8".parse().unwrap()],
        ..Default::default()
    };
    let mut cfg = proxied_config(&proxy, dns.clone(), ssrf);
    cfg.dns_timeout = Duration::from_millis(200);
    let f = Fetcher::new(cfg).unwrap();
    // 解析失败：交给代理，不报错
    let r = f
        .fetch(FetchRequest::new("http://missing.example/"))
        .await
        .unwrap();
    assert_eq!(r.body, b"via proxy");
    assert!(dns.asked().iter().any(|h| h == "missing.example"));
    // 解析超时：最多等 min(dns_timeout, 2s)，然后交给代理
    // wall-clock: the DNS timeout is 200 ms; the bug waits for the 2 s cap, which is the bound
    let started = std::time::Instant::now();
    let r = f
        .fetch(FetchRequest::new("http://slow.example/"))
        .await
        .unwrap();
    assert_eq!(r.body, b"via proxy");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(proxy.seen().len(), 2);
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

#[tokio::test]
async fn body_idle_timeout_is_distinct_from_total_timeout() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0_u8; 4096];
        let _ = socket.read(&mut buf).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    let fetcher = Fetcher::new(FetcherConfig {
        body_idle_timeout: Duration::from_millis(50),
        total_timeout: Duration::from_secs(1),
        ssrf: SsrfPolicy {
            allow_ips: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();
    let error = fetcher
        .fetch(FetchRequest::new(format!("http://{addr}/idle")))
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::Timeout(TimeoutPhase::BodyIdle)),
        "{error}"
    );
}

#[tokio::test]
async fn slow_ttfb_transfers_to_bounded_tail_stage_until_body_completion() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0_u8; 4096];
        let _ = socket.read(&mut buf).await;
        tokio::time::sleep(Duration::from_millis(40)).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .unwrap();
    });
    let fetcher = Fetcher::new(FetcherConfig {
        tail_threshold: Duration::from_millis(10),
        ssrf: SsrfPolicy {
            allow_ips: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();
    let response = fetcher
        .fetch(FetchRequest::new(format!("http://{addr}/slow")))
        .await
        .unwrap();
    assert_eq!(response.body, b"ok");
    let snapshots = fetcher.stage_snapshots();
    let http = snapshots
        .iter()
        .find(|snapshot| snapshot.stage == pagesift_net::NetworkStage::Http)
        .unwrap();
    let tail = snapshots
        .iter()
        .find(|snapshot| snapshot.stage == pagesift_net::NetworkStage::Tail)
        .unwrap();
    assert_eq!(http.completed, 1);
    assert_eq!(tail.completed, 1);
    assert_eq!(tail.active, 0);
    let ttfb = fetcher
        .phase_snapshots()
        .into_iter()
        .find(|snapshot| snapshot.phase == pagesift_net::TransportPhase::Ttfb)
        .unwrap();
    assert_eq!(ttfb.samples, 1);
    assert!(ttfb.p50 >= Duration::from_millis(30));
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

#[tokio::test]
async fn reuses_keep_alive_connections() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // 支持 keep-alive 的最小服务器：一条连接上按顺序回应多个请求，统计接受了几条连接
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = accepted.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            count.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        }
                    }
                    let end = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                    buf.drain(..end);
                    let body = "<p>kept alive</p>";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    if sock.write_all(resp.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    let f = fetcher();
    for i in 0..3 {
        let r = f
            .fetch(FetchRequest::new(format!("http://{addr}/{i}")))
            .await
            .unwrap();
        assert_eq!(r.body, b"<p>kept alive</p>");
    }
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "one connection for three requests"
    );
}

#[tokio::test]
async fn adaptive_hot_watermarks_are_isolated_per_origin() {
    let fast = serve(|_| Some(ok("fast"))).await;
    let overloaded = serve(|_| Some(response(503, &[], b"busy"))).await;
    let fetcher = Arc::new(
        Fetcher::new(FetcherConfig {
            max_connections_per_host: 8,
            ..config(FakeDns::new(&[]))
        })
        .unwrap(),
    );

    let mut tasks = tokio::task::JoinSet::new();
    for n in 0..64 {
        let fetcher = fetcher.clone();
        let url = fast.url(&format!("/{n}"));
        tasks.spawn(async move { fetcher.fetch(FetchRequest::new(url)).await });
    }
    while let Some(result) = tasks.join_next().await {
        assert_eq!(result.unwrap().unwrap().status, 200);
    }

    let response = fetcher
        .fetch(FetchRequest::new(overloaded.url("/busy")))
        .await
        .unwrap();
    assert_eq!(response.status, 503);

    let stats = fetcher.host_connection_stats();
    let fast_stats = stats
        .iter()
        .find(|s| s.origin.ends_with(&fast.addr.to_string()))
        .unwrap();
    let slow_stats = stats
        .iter()
        .find(|s| s.origin.ends_with(&overloaded.addr.to_string()))
        .unwrap();
    assert!(fast_stats.hot_target > 2, "{fast_stats:?}");
    assert_eq!(fast_stats.failed, 0);
    assert_eq!(slow_stats.hot_target, 1, "{slow_stats:?}");
    assert_eq!(slow_stats.overloaded, 1);
}

#[tokio::test]
async fn raw_stream_uses_identity_and_keeps_redirect_guards() {
    let srv = serve(|s| {
        Some(match s.path.as_str() {
            "/redirect" => redirect(302, "/file"),
            _ => {
                assert_eq!(s.header("accept-encoding"), Some("identity"));
                response(200, &[("ETag", "\"stream\"")], &[b'x'; 8192])
            }
        })
    })
    .await;
    let mut req = FetchRequest::new(srv.url("/redirect"));
    req.max_body_bytes = Some(1);
    let mut stream = fetcher().fetch_stream(req).await.unwrap();
    assert_eq!(stream.final_url.path(), "/file");
    assert_eq!(stream.redirect_chain.len(), 1);
    let mut bytes = 0;
    while let Some(chunk) = stream.chunk().await.unwrap() {
        bytes += chunk.len();
    }
    assert_eq!(bytes, 8192);
}

#[tokio::test]
async fn raw_stream_rejects_unexpected_content_encoding() {
    let srv = serve(|_| Some(response(200, &[("Content-Encoding", "gzip")], b"encoded"))).await;
    assert!(
        fetcher()
            .fetch_stream(FetchRequest::new(srv.url("/")))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn proxy_raw_stream_preserves_byte_representation_and_rejects_encoding() {
    let proxy = serve(|s| {
        assert_eq!(s.header("accept-encoding"), Some("identity"));
        Some(if s.path.ends_with("/encoded") {
            response(200, &[("Content-Encoding", "gzip")], b"encoded")
        } else {
            response(200, &[("ETag", "\"raw\"")], b"raw bytes")
        })
    })
    .await;
    let f = Fetcher::new(FetcherConfig {
        proxy: Some(format!("http://proxy.test:{}", proxy.addr.port())),
        dns: FakeDns::new(&[("proxy.test", &["127.0.0.1"])]),
        ..Default::default()
    })
    .unwrap();
    let mut stream = f
        .fetch_stream(FetchRequest::new("http://example.com/raw"))
        .await
        .unwrap();
    let mut body = Vec::new();
    while let Some(chunk) = stream.chunk().await.unwrap() {
        body.extend_from_slice(&chunk);
    }
    assert_eq!(body, b"raw bytes");
    assert!(
        f.fetch_stream(FetchRequest::new("http://example.com/encoded"))
            .await
            .is_err()
    );
}
