use super::lease::invalidated;
use super::*;
fn url(p: &str) -> Url {
    Url::parse(&format!("https://one.test{p}")).unwrap()
}
fn text(s: &CookieSession, p: &str) -> String {
    s.cookie_header(&url(p))
        .unwrap()
        .map(|v| v.to_str().unwrap().into())
        .unwrap_or_default()
}
#[test]
fn path_defaults_order_replacement_and_deletion() {
    let s = CookieSession::new(&url("/a/login"), SessionConfig::default()).unwrap();
    assert!(s.set_cookie(&url("/a/login"), "id=base; Path=/"));
    assert!(s.set_cookie(&url("/a/login"), "id=specific; HttpOnly"));
    assert_eq!(text(&s, "/a/view"), "id=specific; id=base");
    assert_eq!(text(&s, "/apple"), "id=base");
    assert!(s.set_cookie(&url("/a/login"), "id=replaced; Path=/a"));
    assert_eq!(text(&s, "/a"), "id=replaced; id=base");
    assert!(s.set_cookie(
        &url("/a/login"),
        "id=gone; Path=/a; Max-Age=0; Expires=Wed, 09 Jun 2099 10:18:14 GMT"
    ));
    assert_eq!(text(&s, "/a/view"), "id=base");
}
#[test]
fn secure_prefixes_and_exact_origin_reject_cross_scope() {
    let s = CookieSession::new(&url("/"), SessionConfig::default()).unwrap();
    assert!(s.set_cookie(&url("/"), "__Host-id=secret; Path=/; Secure; HttpOnly"));
    for v in [
        "__Host-no-path=bad; Secure",
        "__Host-domain=bad; Secure; Path=/; Domain=one.test",
        "__Secure-id=bad",
        "id=bad; Domain=test",
        "id=bad; Domain=other.test",
        "bad name=x",
        "id=bad\\value",
        "id=x\r\nInjected:y",
    ] {
        assert!(!s.set_cookie(&url("/"), v), "{v}");
    }
    for other in [
        "http://one.test/",
        "https://one.test:444/",
        "https://sub.one.test/",
        "https://other.test/",
    ] {
        let u = Url::parse(other).unwrap();
        assert!(s.cookie_header(&u).unwrap().is_none());
        assert!(!s.set_cookie(&u, "other=bad"));
    }
}
#[test]
fn age_expiry_and_resource_limits() {
    let config = SessionConfig {
        max_cookies: 2,
        ..Default::default()
    };
    let s = CookieSession::new_at(&url("/"), config, 100).unwrap();
    assert!(s.store_at(&url("/"), "id=x; Max-Age=5", 100));
    assert!(s.header_at(&url("/"), 104).unwrap().is_some());
    assert!(s.header_at(&url("/"), 105).unwrap().is_none());
    assert!(s.store_at(&url("/"), "one=x", 105));
    assert!(s.store_at(&url("/"), "two=y", 105));
    assert!(!s.store_at(&url("/"), "three=z", 105));
    assert!(!s.store_at(&url("/"), &format!("big={}", "x".repeat(5000)), 105));
    assert!(s.store_at(&url("/"), "one=new", 105));
}
#[test]
fn session_age_header_budget_and_invalid_last_path_are_bounded() {
    let s = CookieSession::new_at(
        &url("/a/login"),
        SessionConfig {
            max_age: Duration::from_secs(10),
            max_header_bytes: 3,
            ..Default::default()
        },
        100,
    )
    .unwrap();
    assert!(s.store_at(&url("/a/login"), "id=value; Path=/; Path=invalid", 100));
    assert!(s.header_at(&url("/b"), 101).unwrap().is_none());
    assert!(matches!(
        s.header_at(&url("/a/view"), 101),
        Err(SessionError::HeaderLimit)
    ));
    assert!(matches!(
        s.header_at(&url("/a/view"), 110),
        Err(SessionError::Expired)
    ));
}
#[test]
fn snapshot_roundtrip_keeps_secrets_redacted_and_scope_fixed() {
    let origin = url("/");
    let s = CookieSession::new(&origin, SessionConfig::default()).unwrap();
    s.set_cookie(&origin, "id=VERY_SECRET_TOKEN; Secure; HttpOnly; Path=/");
    let snapshot = s.snapshot().unwrap();
    assert!(!format!("{s:?} {snapshot:?}").contains("VERY_SECRET"));
    let restored =
        CookieSession::restore(&origin, SessionConfig::default(), snapshot.as_bytes()).unwrap();
    assert_eq!(text(&restored, "/"), "id=VERY_SECRET_TOKEN");
    assert!(
        CookieSession::restore(
            &Url::parse("https://other.test").unwrap(),
            SessionConfig::default(),
            snapshot.as_bytes()
        )
        .is_err()
    );
    let future = String::from_utf8(snapshot.as_bytes().to_vec())
        .unwrap()
        .replace("\"version\":1", "\"version\":2");
    assert!(CookieSession::restore(&origin, SessionConfig::default(), future.as_bytes()).is_err());
}
#[test]
fn retirement_is_an_sprt_against_the_session_baseline() {
    // 冷启动：连续 3 次失败判失效（0.81 的行为）
    assert!(!invalidated(2, 0, 0, 3));
    assert!(invalidated(3, 0, 0, 3));
    // 历史干净：仍然是 3 次（不少于配置值）
    assert!(invalidated(3, 500, 0, 3));
    assert!(!invalidated(2, 500, 0, 3));
    // 历史上 60% 的使用失败（部分页面本来就拒绝）：(1/0.6)^k ≥ 19 → k ≥ 6
    assert!(!invalidated(5, 400, 600, 3));
    assert!(invalidated(6, 400, 600, 3));
    // 配置值 2 时的冷启动也恰好是 2
    assert!(invalidated(2, 0, 0, 2) && !invalidated(1, 0, 0, 2));
}

#[test]
fn leases_count_errors_retirement_and_use_budget() {
    let config = SessionConfig {
        max_uses: 4,
        max_consecutive_errors: 2,
        ..Default::default()
    };
    let s = Arc::new(CookieSession::new(&url("/"), config).unwrap());
    drop(s.begin(&url("/")).unwrap());
    assert_eq!(s.stats().consecutive_errors, 1);
    s.begin(&url("/")).unwrap().finish(200);
    assert_eq!(s.stats().consecutive_errors, 0);
    s.begin(&url("/")).unwrap().finish(401);
    s.begin(&url("/")).unwrap().finish(403);
    assert!(s.stats().retired);
    assert!(s.begin(&url("/")).is_err());
    let s = Arc::new(
        CookieSession::new(
            &url("/"),
            SessionConfig {
                max_uses: 1,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    s.begin(&url("/")).unwrap().finish(200);
    assert!(matches!(s.begin(&url("/")), Err(SessionError::Exhausted)));
}
