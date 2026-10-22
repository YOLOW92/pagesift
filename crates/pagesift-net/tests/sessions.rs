//! Sessions share the production HTTP, redirect and raw-stream paths.
mod common;
use common::{FakeDns, ok, redirect, response, serve};
use pagesift_net::{
    CookieSession, Error, FetchRequest, Fetcher, FetcherConfig, HostCredentials, SessionConfig,
    SessionError, Url,
};
use std::sync::Arc;

fn fetcher() -> Fetcher {
    let mut config = FetcherConfig::default();
    config.ssrf.allow_private_network = true;
    Fetcher::new(config).unwrap()
}

#[tokio::test]
async fn cookies_survive_redirects_but_do_not_cross_hosts_or_pollute_stateless_requests() {
    let srv = serve(|s| {
        let host = s.header("host").unwrap();
        let port = host.rsplit(':').next().unwrap();
        Some(match s.path.as_str() {
            "/start" => response(
                302,
                &[
                    ("Location", &format!("http://b.test:{port}/foreign")),
                    ("Set-Cookie", "sid=ALICE; Path=/; HttpOnly"),
                ],
                b"",
            ),
            "/foreign" => {
                assert!(s.header("cookie").is_none());
                response(
                    302,
                    &[
                        ("Location", &format!("http://a.test:{port}/finish")),
                        ("Set-Cookie", "sid=FOREIGN; Path=/"),
                    ],
                    b"",
                )
            }
            "/finish" => {
                assert_eq!(s.header("cookie"), Some("sid=ALICE"));
                ok("account")
            }
            _ => {
                assert!(s.header("cookie").is_none());
                ok("stateless")
            }
        })
    })
    .await;
    let origin: Url = format!("http://a.test:{}/start", srv.addr.port())
        .parse()
        .unwrap();
    let session = Arc::new(CookieSession::new(&origin, Default::default()).unwrap());
    let mut config = FetcherConfig::default();
    config.ssrf.allow_private_network = true;
    config.dns = FakeDns::new(&[("a.test", &["127.0.0.1"]), ("b.test", &["127.0.0.1"])]);
    let f = Fetcher::new(config).unwrap();
    let r = f
        .fetch(FetchRequest::new(origin.as_str()).with_session(session.clone()))
        .await
        .unwrap();
    assert_eq!(r.body, b"account");
    assert_eq!(session.stats().uses, 1);
    f.fetch(FetchRequest::new(format!(
        "http://a.test:{}/plain",
        srv.addr.port()
    )))
    .await
    .unwrap();
    assert_eq!(session.stats().cookies, 1);
}

#[tokio::test]
async fn identity_jars_are_isolated_and_static_cookie_credentials_do_not_merge() {
    let srv = serve(|s| Some(ok(s.header("cookie").unwrap_or("NONE")))).await;
    let origin: Url = srv.url("/").parse().unwrap();
    let alice = Arc::new(CookieSession::new(&origin, Default::default()).unwrap());
    let bob = Arc::new(CookieSession::new(&origin, Default::default()).unwrap());
    alice.set_cookie(&origin, "id=ALICE; Path=/");
    bob.set_cookie(&origin, "id=BOB; Path=/");
    let mut config = FetcherConfig::default();
    config.ssrf.allow_private_network = true;
    config.credentials = vec![HostCredentials::new("127.0.0.1").cookie("legacy", "SHARED")];
    let f = Fetcher::new(config).unwrap();
    for (identity, expected) in [(alice.clone(), "id=ALICE"), (bob, "id=BOB")] {
        let r = f
            .fetch(FetchRequest::new(origin.as_str()).with_session(identity))
            .await
            .unwrap();
        assert_eq!(r.body, expected.as_bytes());
    }
    let invalid = f
        .fetch(
            FetchRequest::new(origin.as_str())
                .with_session(alice)
                .header("Cookie", "manual=conflict"),
        )
        .await;
    assert!(matches!(invalid, Err(Error::Header(_))));
    assert_eq!(srv.seen().len(), 2);
}

#[tokio::test]
async fn different_port_rejects_direct_use_and_strips_on_redirect() {
    let other = serve(|s| {
        assert!(s.header("cookie").is_none());
        assert!(s.header("authorization").is_none());
        assert!(s.header("x-private").is_none());
        Some(response(200, &[("Set-Cookie", "sid=evil")], b"foreign"))
    })
    .await;
    let foreign = other.url("/foreign");
    let destination = foreign.clone();
    let srv = serve(move |_| Some(redirect(302, &destination))).await;
    let origin: Url = srv.url("/").parse().unwrap();
    let session = Arc::new(CookieSession::new(&origin, Default::default()).unwrap());
    session.set_cookie(&origin, "sid=local");
    let f = fetcher();
    f.fetch(
        FetchRequest::new(origin.as_str())
            .with_session(session.clone())
            .header("Authorization", "Bearer ORIGIN_ONLY")
            .header("X-Private", "ORIGIN_ONLY"),
    )
    .await
    .unwrap();
    assert!(matches!(
        f.fetch(FetchRequest::new(foreign).with_session(session))
            .await,
        Err(Error::Session(SessionError::OriginMismatch))
    ));
    assert_eq!(other.seen().len(), 1);
}

#[tokio::test]
async fn health_accounts_for_http_errors_body_failure_and_stream_completion() {
    let srv = serve(|s| {
        Some(match s.path.as_str() {
            "/deny" => response(401, &[], b"denied"),
            "/broken" => {
                b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nshort".to_vec()
            }
            _ => response(200, &[("Set-Cookie", "id=OK; Path=/")], b"success"),
        })
    })
    .await;
    let origin: Url = srv.url("/").parse().unwrap();
    let session = Arc::new(
        CookieSession::new(
            &origin,
            SessionConfig {
                max_consecutive_errors: 2,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let f = fetcher();
    f.fetch(FetchRequest::new(srv.url("/deny")).with_session(session.clone()))
        .await
        .unwrap();
    assert_eq!(session.stats().consecutive_errors, 1);
    let mut stream = f
        .fetch_stream(FetchRequest::new(srv.url("/good")).with_session(session.clone()))
        .await
        .unwrap();
    while stream.chunk().await.unwrap().is_some() {}
    assert_eq!(session.stats().consecutive_errors, 0);
    assert!(
        f.fetch(FetchRequest::new(srv.url("/broken")).with_session(session.clone()))
            .await
            .is_err()
    );
    assert_eq!(session.stats().consecutive_errors, 1);
    f.fetch(FetchRequest::new(srv.url("/deny")).with_session(session.clone()))
        .await
        .unwrap();
    assert!(session.stats().retired);
    let seen = srv.seen().len();
    assert!(
        f.fetch(FetchRequest::new(srv.url("/good")).with_session(session.clone()))
            .await
            .is_err()
    );
    assert_eq!(seen, srv.seen().len());
    let saved = session.snapshot().unwrap();
    let restored = CookieSession::restore(&origin, Default::default(), saved.as_bytes()).unwrap();
    assert!(restored.stats().retired);
}

#[tokio::test]
async fn concurrent_admission_honors_a_single_use_budget() {
    let srv = serve(|_| Some(ok("one"))).await;
    let origin: Url = srv.url("/").parse().unwrap();
    let s = Arc::new(
        CookieSession::new(
            &origin,
            SessionConfig {
                max_uses: 1,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let f = fetcher();
    let (a, b) = tokio::join!(
        f.fetch(FetchRequest::new(origin.as_str()).with_session(s.clone())),
        f.fetch(FetchRequest::new(origin.as_str()).with_session(s.clone()))
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(srv.seen().len(), 1);
}
