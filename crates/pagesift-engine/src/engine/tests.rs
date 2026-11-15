use super::*;

fn content(md: &str, published: bool, og_article: bool) -> pagesift_content::Document {
    let mut d = pagesift_content::process("<p>x</p>", &Default::default());
    d.markdown = md.to_string();
    d.metadata.published_time = published.then(|| "2026-10-21T00:00:00Z".into());
    if og_article {
        d.metadata.extra.push(("og:type".into(), "article".into()));
    }
    d
}

#[test]
fn static_learning_cannot_suppress_client_code() {
    for html in [
        "<script>hydrate()</script>",
        "<script src='/app.js'></script>",
        "<SCRIPT TYPE='module' src='/view.js'></SCRIPT>",
        "<body onload='hydrate()'>",
        "<button onclick='hydrate()'>Open</button>",
    ] {
        assert!(has_client_code(html), "{html}");
    }
    for html in [
        "<article><p>Short but complete.</p></article>",
        "<script type='application/ld+json'>{}</script>",
        "<script type='APPLICATION/JSON; charset=utf-8'>{}</script>",
        "<script type='text/plain'>inert sample</script>",
    ] {
        assert!(!has_client_code(html), "{html}");
    }
}

#[test]
fn render_rule_weights_cjk_and_trusts_short_articles() {
    assert_eq!(text_weight("ab c\n"), 3);
    assert_eq!(text_weight("日本語のニュース"), 8 * CJK_WEIGHT);
    // 500 个汉字按 1500 算，不用渲染；500 个拉丁字母要渲染
    assert!(!needs_render(&content(&"字".repeat(500), false, false)));
    assert!(needs_render(&content(&"a".repeat(500), false, false)));
    // 认出是文章且有 300 以上：不渲染；太短的文章照样渲染
    assert!(!needs_render(&content(&"a".repeat(400), true, false)));
    assert!(!needs_render(&content(&"a".repeat(400), false, true)));
    assert!(needs_render(&content(&"a".repeat(100), true, true)));
}

#[test]
fn render_errors_are_classified() {
    use pagesift_cdp::Error as E;
    for (e, code) in [
        (
            E::Action {
                index: 0,
                message: "no element".into(),
            },
            ErrorCode::InvalidRequest,
        ),
        (E::InvalidOptions("x".into()), ErrorCode::InvalidRequest),
        (E::ExecuteJavascriptDisabled, ErrorCode::UnsupportedOption),
        (E::Timeout, ErrorCode::Timeout),
        (E::Busy, ErrorCode::Busy),
        (
            E::Navigation("net::ERR_NAME_NOT_RESOLVED".into()),
            ErrorCode::DnsFailure,
        ),
        (
            E::Navigation("net::ERR_CONNECTION_RESET".into()),
            ErrorCode::NetworkError,
        ),
        (
            E::Navigation("net::ERR_CERT_DATE_INVALID".into()),
            ErrorCode::TlsCertExpired,
        ),
        (
            E::Navigation("net::ERR_CERT_COMMON_NAME_INVALID".into()),
            ErrorCode::TlsHostnameMismatch,
        ),
        (
            E::Navigation("net::ERR_CERT_AUTHORITY_INVALID".into()),
            ErrorCode::TlsUntrustedRoot,
        ),
        (
            E::Navigation("net::ERR_CERT_REVOKED".into()),
            ErrorCode::TlsError,
        ),
        (E::ConnectionClosed, ErrorCode::UpstreamError),
    ] {
        assert_eq!(render_error_code(&e), code, "{e}");
    }
}

#[test]
fn finds_only_visible_same_origin_and_srcdoc_iframes() {
    let base = Url::parse("https://a.test/page").unwrap();
    let html = r#"<body>
        <iframe src="https://www.googletagmanager.com/ns.html"></iframe>
        <iframe src="/pixel" style="display: none"></iframe>
        <iframe src="/hidden" aria-hidden="true"></iframe>
        <iframe src="/tiny" width="1" height="1"></iframe>
        <iframe src="about:blank"></iframe>
        <iframe src="/comments?id=1&amp;x=2" height="160"></iframe>
        <iframe srcdoc="&lt;p&gt;inline&lt;/p&gt;"></iframe>
        <iframe srcdoc="&lt;p&gt;x&lt;/p&gt;" style="visibility:hidden"></iframe>
    </body>"#;
    let frames = content_iframes(html, &base);
    assert_eq!(frames.len(), 2, "{frames:?}");
    assert_eq!(frames[0].0, "/comments?id=1&x=2");
    assert_eq!(
        frames[0].1.as_ref().map(Url::as_str),
        Some("https://a.test/comments?id=1&x=2")
    );
    assert_eq!(frames[1], ("<p>inline</p>".to_string(), None));
    assert!(content_iframes("<p>no frames</p>", &base).is_empty());
}

#[test]
fn replaces_the_matching_iframe_element() {
    let mut html =
        r#"<p>a</p><IFRAME src="/x" hidden></IFRAME><iframe src="/x">fallback</iframe><p>b</p>"#
            .to_string();
    assert!(replace_iframe(&mut html, "<div>X</div>", |el| {
        !hidden_frame(el) && src_is(el, "/x")
    }));
    assert_eq!(
        html,
        r#"<p>a</p><IFRAME src="/x" hidden></IFRAME><div>X</div><p>b</p>"#
    );
    assert!(!replace_iframe(&mut html, "", |el| src_is(el, "/missing")));
}

#[test]
fn takes_the_body_of_an_iframe_document() {
    assert_eq!(
        body_inner("<html><head><title>t</title></head><BODY class=x><p>hi</p></body></html>"),
        "<p>hi</p>"
    );
    assert_eq!(body_inner("<p>fragment</p>"), "<p>fragment</p>");
}

#[test]
fn meta_keys_follow_firecrawl_style() {
    assert_eq!(pagesift_sink::meta::key("og:image"), "ogImage");
    assert_eq!(pagesift_sink::meta::key("og:site_name"), "ogSiteName");
    assert_eq!(
        pagesift_sink::meta::key("article:published_time"),
        "articlePublishedTime"
    );
    assert_eq!(pagesift_sink::meta::key("dc.title"), "dcTitle");
}
