mod common;

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use common::{ok, redirect, response, serve};
use pagesift_net::{FetchRequest, Fetcher, FetcherConfig, SsrfPolicy};
use pagesift_policy::{ErrorCode, Policy, PolicyConfig, RateLimitConfig, RobotsMode};

fn policy() -> Policy {
    let fetcher = Fetcher::new(FetcherConfig {
        ssrf: SsrfPolicy {
            allow_ips: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();
    Policy::new(
        Arc::new(fetcher),
        PolicyConfig {
            rate: RateLimitConfig {
                default_interval: Duration::ZERO,
                ..Default::default()
            },
            ..Default::default()
        },
    )
}

fn code(e: pagesift_policy::Error) -> &'static str {
    e.code().as_str()
}

#[tokio::test]
async fn robots_disallow_blocks_and_is_cached() {
    let srv = serve(|s| {
        Some(match s.path.as_str() {
            "/robots.txt" => {
                ok("User-agent: pagesift\nDisallow: /private\n\nUser-agent: *\nDisallow: /")
            }
            _ => ok("page"),
        })
    })
    .await;
    let p = policy();
    let r = p
        .fetch(FetchRequest::new(srv.url("/public")), RobotsMode::Respect)
        .await
        .unwrap();
    assert_eq!(r.body, b"page");
    let err = p
        .fetch(
            FetchRequest::new(srv.url("/private/x")),
            RobotsMode::Respect,
        )
        .await
        .unwrap_err();
    assert_eq!(code(err), "ROBOTS_DISALLOWED");
    // 忽略 robots 时照常抓取。
    p.fetch(FetchRequest::new(srv.url("/private/x")), RobotsMode::Ignore)
        .await
        .unwrap();

    let robots_hits = srv
        .seen()
        .iter()
        .filter(|s| s.path == "/robots.txt")
        .count();
    assert_eq!(robots_hits, 1);
}

#[tokio::test]
async fn expired_robots_cache_revalidates_conditionally_without_bypassing_rules() {
    let srv = serve(|request| {
        Some(match request.path.as_str() {
            "/robots.txt" if request.header("if-none-match") == Some("\"robots-v1\"") => {
                response(304, &[("ETag", "\"robots-v1\"")], b"")
            }
            "/robots.txt" => response(
                200,
                &[("ETag", "\"robots-v1\"")],
                b"User-agent: *\nDisallow: /private\n",
            ),
            _ => ok("page"),
        })
    })
    .await;
    let fetcher = Fetcher::new(FetcherConfig {
        ssrf: SsrfPolicy {
            allow_ips: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();
    let policy = Policy::new(
        Arc::new(fetcher),
        PolicyConfig {
            robots_ttl: Duration::from_millis(1),
            rate: RateLimitConfig {
                default_interval: Duration::ZERO,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    policy
        .fetch(FetchRequest::new(srv.url("/public")), RobotsMode::Respect)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    let error = policy
        .fetch(
            FetchRequest::new(srv.url("/private/secret")),
            RobotsMode::Respect,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::RobotsDisallowed);
    let robots = srv
        .seen()
        .into_iter()
        .filter(|request| request.path == "/robots.txt")
        .collect::<Vec<_>>();
    assert_eq!(robots.len(), 2);
    assert_eq!(robots[1].header("if-none-match"), Some("\"robots-v1\""));
}

#[tokio::test]
async fn robots_5xx_disallows_and_4xx_allows() {
    let down = serve(|s| {
        Some(if s.path == "/robots.txt" {
            response(500, &[], b"")
        } else {
            ok("x")
        })
    })
    .await;
    let missing = serve(|s| {
        Some(if s.path == "/robots.txt" {
            response(404, &[], b"")
        } else {
            ok("x")
        })
    })
    .await;
    let p = policy();
    let err = p
        .fetch(FetchRequest::new(down.url("/a")), RobotsMode::Respect)
        .await
        .unwrap_err();
    assert_eq!(code(err), "ROBOTS_DISALLOWED");
    assert_eq!(down.seen().len(), 1, "page itself is never requested");
    p.fetch(FetchRequest::new(missing.url("/a")), RobotsMode::Respect)
        .await
        .unwrap();
}

#[tokio::test]
async fn redirect_into_disallowed_path_is_rejected() {
    let srv = serve(|s| {
        Some(match s.path.as_str() {
            "/robots.txt" => ok("User-agent: *\nDisallow: /secret\n"),
            "/go" => redirect(302, "/secret/page"),
            _ => ok("leak"),
        })
    })
    .await;
    let err = policy()
        .fetch(FetchRequest::new(srv.url("/go")), RobotsMode::Respect)
        .await
        .unwrap_err();
    assert_eq!(code(err), "ROBOTS_DISALLOWED");
    assert!(srv.seen().iter().all(|s| s.path != "/secret/page"));
}

#[tokio::test]
async fn redirect_to_internal_address_is_ssrf_blocked() {
    let srv = serve(|_| Some(redirect(302, "http://169.254.169.254/latest/meta-data/"))).await;
    let err = policy()
        .fetch(FetchRequest::new(srv.url("/")), RobotsMode::Ignore)
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::SsrfBlocked);
    assert_eq!(err.http_status(), 400);
}

#[tokio::test]
async fn too_many_429_puts_host_in_cooldown() {
    let srv = serve(|_| Some(response(429, &[("Retry-After", "120")], b""))).await;
    let p = policy();
    let err = p
        .fetch(FetchRequest::new(srv.url("/")), RobotsMode::Ignore)
        .await
        .unwrap_err();
    assert_eq!(err.retry_after(), Some(Duration::from_secs(120)));
    assert_eq!(code(err), "RATE_LIMITED");
    let err = p
        .fetch(FetchRequest::new(srv.url("/")), RobotsMode::Ignore)
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::HostCooldown);
    assert_eq!(err.http_status(), 503);
    let left = err.retry_after().unwrap();
    assert!(
        left > Duration::from_secs(100) && left <= Duration::from_secs(120),
        "{left:?}"
    );
    assert_eq!(srv.seen().len(), 1, "no request is sent during cooldown");
}

#[tokio::test]
async fn server_errors_are_returned_and_counted() {
    let srv = serve(|_| Some(response(500, &[], b"oops"))).await;
    let p = policy();
    for _ in 0..3 {
        let r = p
            .fetch(FetchRequest::new(srv.url("/")), RobotsMode::Ignore)
            .await
            .unwrap();
        assert_eq!(r.status, 500);
    }
    let err = p
        .fetch(FetchRequest::new(srv.url("/")), RobotsMode::Ignore)
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::HostCooldown);
}

#[tokio::test]
async fn invalid_and_blocked_urls() {
    let p = policy();
    let err = p
        .fetch(FetchRequest::new("nope"), RobotsMode::Respect)
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRequest);
    let err = p
        .fetch(FetchRequest::new("http://0x7f000001/"), RobotsMode::Respect)
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::SsrfBlocked);
}
