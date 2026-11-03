//! Jina Reader 兼容：`/r/` 的请求头逐条（jina-ai/reader README “Using request headers”）和 `/s/`。
//! 真实引擎抓本机站点。这里不启动浏览器：需要浏览器的头在真实 Chrome 上的行为见 `actions.rs`，
//! 这里只测它们没有浏览器可用时明确报错。

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::HeaderMap;
use axum::response::Html;
use axum::routing::get;
use pagesift::{Scraper, ScraperConfig};

const PAGE: &str = "<html><head><title>Jina fixture</title><meta name='description' content='A fixture page'></head><body>\
    <nav><a href='/home'>Home nav</a></nav><article><h1>Main title</h1>\
    <p>Read the <a href='/docs'>documentation</a> and the <a href='/docs'>docs again</a> for details about this fixture page.</p>\
    <p><img src='/logo.png' alt='Logo image'> Watch <a href='https://youtu.be/abc123'>the clip</a> about it.</p>\
    <div class='ad'>Buy now advertisement block</div>\
    <h2>Second section</h2><p>More text in the second section with enough words to be kept by extraction.</p>\
    <h2>Third section</h2><p>Even more text in the third section, also long enough to be kept by extraction.</p>\
    </article><footer><p>Footer text</p></footer></body></html>";

struct Site {
    base: String,
    api: Router,
}

struct OneHit(String);

impl pagesift_search::SearchBackend for OneHit {
    fn name(&self) -> &'static str {
        "fixture"
    }
    fn search<'a>(
        &'a self,
        q: &'a pagesift_search::SearchQuery,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = pagesift_search::Result<Vec<pagesift_search::SearchResult>>,
                > + Send
                + 'a,
        >,
    > {
        let url = self.0.clone();
        let query = q.query.clone();
        Box::pin(async move {
            Ok(vec![pagesift_search::SearchResult {
                url,
                title: format!("Result for {query}"),
                description: "fixture hit".into(),
                position: 1,
            }])
        })
    }
}

async fn site() -> Site {
    let app = Router::new()
        .route("/page", get(|| async { Html(PAGE) }))
        .route(
            "/cookie",
            get(|h: HeaderMap| async move {
                let c = h.get("cookie").and_then(|v| v.to_str().ok()).unwrap_or("none").to_string();
                Html(format!("<html><body><article><h1>Cookie echo</h1><p>The request carried cookie {c} and this sentence pads the paragraph.</p></article></body></html>"))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut cfg = ScraperConfig::default();
    cfg.fetcher.env_proxy = false;
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    cfg.search = Some(Arc::new(OneHit(format!("{base}/page"))));
    // 不启动真实浏览器：这些测试只测请求头映射；路由销毁时不会关闭浏览器池，起了的浏览器会留下来
    cfg.browser.source =
        pagesift_cdp::BrowserSource::Executable("/nonexistent/pagesift-no-browser".into());
    cfg.browser.args_override = Some(vec!["--headless".into()]);
    let scraper = Arc::new(Scraper::new(cfg).unwrap());
    let api = pagesift_api::router(scraper, pagesift_api::Config::default());
    Site { base, api }
}

/// 发一个请求给 API 路由（不经网络）：返回状态码、内容类型、`x-pagesift-warnings`、正文。
async fn call(api: &Router, uri: &str, headers: &[(&str, &str)]) -> (u16, String, String, String) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let mut req = axum::http::Request::builder().uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = api
        .clone()
        .oneshot(req.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let h = |n: &str| {
        resp.headers()
            .get(n)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    let (ct, warn) = (h("content-type"), h("x-pagesift-warnings"));
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        ct,
        warn,
        String::from_utf8_lossy(&body).into_owned(),
    )
}

/// `/r/<page>` with headers → (status, content-type, x-pagesift-warnings, body).
async fn read(path: &str, headers: &[(&str, &str)]) -> (u16, String, String, String) {
    let s = site().await;
    call(&s.api, &format!("/r/{}{path}", s.base), headers).await
}

macro_rules! header_case {
    ($name:ident, $path:expr, [$(($k:expr, $v:expr)),*], |$st:ident, $ct:ident, $w:ident, $b:ident| $check:block) => {
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn $name() {
            let ($st, $ct, $w, $b) = read($path, &[$(($k, $v)),*]).await;
            let _ = (&$st, &$ct, &$w, &$b);
            $check
        }
    };
}

header_case!(
    respond_with_markdown_is_the_whole_page,
    "/page",
    [("x-respond-with", "markdown")],
    |st, ct, w, b| {
        assert_eq!(st, 200);
        assert!(ct.starts_with("text/markdown"));
        assert!(b.contains("Home nav") && b.contains("Main title"), "{b}");
    }
);
header_case!(
    respond_with_html,
    "/page",
    [("x-respond-with", "html")],
    |st, ct, w, b| {
        assert_eq!(st, 200);
        assert!(
            ct.starts_with("text/html") && b.contains("<article>"),
            "{b}"
        );
    }
);
header_case!(
    respond_with_text,
    "/page",
    [("x-respond-with", "text")],
    |st, ct, w, b| {
        assert!(ct.starts_with("text/plain"));
        assert!(!b.contains("](") && !b.contains("# "), "{b}");
        assert!(b.contains("documentation"));
    }
);
header_case!(
    respond_with_frontmatter,
    "/page",
    [("x-respond-with", "frontmatter")],
    |st, ct, w, b| {
        assert!(
            b.starts_with("---\ntitle: \"Jina fixture\"\ndescription: \"A fixture page\"\nurl: "),
            "{b}"
        );
        assert!(!b.contains("Home nav"), "main content only: {b}");
    }
);
header_case!(
    respond_with_markdown_frontmatter,
    "/page",
    [("x-respond-with", "markdown+frontmatter")],
    |st, ct, w, b| {
        assert!(b.starts_with("---\n") && b.contains("Home nav"), "{b}");
    }
);
header_case!(
    respond_with_screenshot_without_a_working_browser_is_an_error,
    "/page",
    [("x-respond-with", "screenshot")],
    |st, ct, w, b| {
        // 这里的浏览器是个不存在的文件：明确报错，不返回没有截图的页面
        assert_eq!(st, 502, "{b}");
        assert!(b.contains("UPSTREAM_ERROR"), "{b}");
    }
);
header_case!(
    engine_curl_and_unknown_engine,
    "/page",
    [("x-engine", "curl")],
    |st, ct, w, b| {
        assert_eq!(st, 200);
        let (st2, _, _, b2) = read("/page", &[("x-engine", "cf-browser-rendering")]).await;
        assert_eq!(st2, 400, "{b2}");
    }
);
header_case!(
    target_selector,
    "/page",
    [("x-target-selector", "h2 + p")],
    |st, ct, w, b| {
        assert!(
            b.contains("second section") && !b.contains("Main title"),
            "{b}"
        );
    }
);
header_case!(
    remove_selector,
    "/page",
    [("x-remove-selector", ".ad")],
    |st, ct, w, b| {
        assert!(
            !b.contains("advertisement") && b.contains("Main title"),
            "{b}"
        );
    }
);
header_case!(
    wait_for_selector_without_a_working_browser_is_an_error,
    "/page",
    [("x-wait-for-selector", "h1")],
    |st, ct, w, b| {
        assert_eq!(st, 502, "{b}");
        assert!(b.contains("UPSTREAM_ERROR"), "{b}");
    }
);
header_case!(
    wait_for_selector_cannot_use_the_http_engine,
    "/page",
    [("x-wait-for-selector", "h1"), ("x-engine", "curl")],
    |st, ct, w, b| {
        assert_eq!(st, 400, "{b}");
        assert!(b.contains("INVALID_REQUEST"), "{b}");
    }
);
header_case!(
    timeout_seconds,
    "/page",
    [("x-timeout", "5")],
    |st, ct, w, b| {
        assert_eq!(st, 200);
        let (st2, ..) = read("/page", &[("x-timeout", "soon")]).await;
        assert_eq!(st2, 400);
    }
);
header_case!(
    respond_timing_html_and_unknown,
    "/page",
    [("x-respond-timing", "html")],
    |st, ct, w, b| {
        assert_eq!(st, 200);
        let (st2, ..) = read("/page", &[("x-respond-timing", "eventually")]).await;
        assert_eq!(st2, 400);
    }
);
header_case!(
    max_tokens_truncates,
    "/page",
    [("x-max-tokens", "8")],
    |st, ct, w, b| {
        assert_eq!(st, 200);
        assert!(w.starts_with("output_truncated: 8 of "), "{w}");
        assert!(b.len() < 80, "{b}");
    }
);
header_case!(
    token_budget_refuses,
    "/page",
    [("x-token-budget", "5")],
    |st, ct, w, b| {
        assert_eq!(st, 413, "{b}");
        assert!(b.contains("x-token-budget"), "{b}");
    }
);
header_case!(
    retain_images_none_and_alt,
    "/page",
    [("x-retain-images", "none")],
    |st, ct, w, b| {
        assert!(!b.contains("logo.png") && !b.contains("Logo image"), "{b}");
        let (.., alt) = read("/page", &[("x-retain-images", "alt")]).await;
        assert!(
            alt.contains("Logo image") && !alt.contains("logo.png"),
            "{alt}"
        );
    }
);
header_case!(
    retain_links_text,
    "/page",
    [("x-retain-links", "text")],
    |st, ct, w, b| {
        assert!(b.contains("documentation") && !b.contains("/docs)"), "{b}");
    }
);
header_case!(
    retain_media_none,
    "/page",
    [("x-retain-media", "none")],
    |st, ct, w, b| {
        assert!(!b.contains("youtu.be") && !b.contains("the clip"), "{b}");
        assert!(b.contains("/docs)"), "other links stay: {b}");
    }
);
header_case!(
    links_summary,
    "/page",
    [("x-with-links-summary", "true")],
    |st, ct, w, b| {
        let tail = b.split("Links/Buttons:").nth(1).expect("summary");
        assert_eq!(tail.matches("/docs)").count(), 1, "deduplicated: {tail}");
        let (.., all) = read("/page", &[("x-with-links-summary", "all")]).await;
        assert_eq!(
            all.split("Links/Buttons:")
                .nth(1)
                .unwrap()
                .matches("/docs)")
                .count(),
            2
        );
    }
);
header_case!(
    images_summary,
    "/page",
    [("x-with-images-summary", "true")],
    |st, ct, w, b| {
        assert!(
            b.split("Images:")
                .nth(1)
                .expect("summary")
                .contains("logo.png"),
            "{b}"
        );
    }
);
header_case!(
    markdown_chunking_headings_json,
    "/page",
    [
        ("x-markdown-chunking", "h2"),
        ("accept", "application/json")
    ],
    |st, ct, w, b| {
        let v: serde_json::Value = serde_json::from_str(&b).unwrap();
        let chunks = v["data"]["chunks"].as_array().expect("chunks");
        assert!(chunks.len() >= 3, "{v}");
        assert!(
            chunks
                .iter()
                .any(|c| c.as_str().unwrap().starts_with("## Third section"))
        );
    }
);
header_case!(
    preset_index_drops_urls,
    "/page",
    [("x-preset", "index")],
    |st, ct, w, b| {
        assert_eq!(st, 200);
        assert!(!b.contains("](") && b.contains("documentation"), "{b}");
    }
);
header_case!(
    no_cache_and_cache_tolerance,
    "/page",
    [("x-no-cache", "true")],
    |st, ct, w, b| {
        assert_eq!(st, 200);
        let (st2, ..) = read("/page", &[("x-cache-tolerance", "60")]).await;
        assert_eq!(st2, 200);
    }
);
header_case!(
    set_cookie_is_forwarded,
    "/cookie",
    [("x-set-cookie", "session=abc; Path=/")],
    |st, ct, w, b| {
        assert!(b.contains("cookie session=abc"), "{b}");
    }
);
header_case!(
    proxy_url_only_accepts_configured_egress_names,
    "/page",
    [("x-proxy-url", "http://10.0.0.1:3128")],
    |st, ct, w, b| {
        assert_eq!(st, 400, "{b}");
        let (st2, _, _, b2) = read("/page", &[("x-proxy-url", "office")]).await;
        assert_eq!(st2, 400, "unknown egress name: {b2}");
    }
);
header_case!(
    generated_alt_is_unsupported,
    "/page",
    [("x-with-generated-alt", "true")],
    |st, ct, w, b| {
        assert_eq!(st, 400);
        assert!(b.contains("UNSUPPORTED_OPTION"), "{b}");
    }
);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_fetches_and_converts_the_results() {
    let s = site().await;
    let (_, _, _, text) = call(&s.api, "/s/fixture%20query?site=example.org", &[]).await;
    assert!(
        text.starts_with("[1] Title: Result for fixture query (site:example.org)"),
        "{text}"
    );
    assert!(
        text.contains("[1] Markdown Content:\n# Main title"),
        "{text}"
    );
    let (_, _, _, body) = call(
        &s.api,
        "/s/fixture",
        &[("accept", "application/json"), ("x-retain-links", "none")],
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let c = v["data"][0]["content"].as_str().unwrap();
    assert!(!c.contains("/docs)") && c.contains("Main title"), "{c}");
}

// 预设、做不到的头、SSE 输出参数
header_case!(
    preset_reader_works_with_a_warning,
    "/page",
    [("x-preset", "reader")],
    |st, ct, w, b| {
        assert_eq!(st, 200, "{b}");
        assert!(b.starts_with("---\n"), "{b}");
        assert!(w.contains("preset_partial"), "{w}");
    }
);
header_case!(
    return_format_is_an_alias,
    "/page",
    [("x-return-format", "frontmatter")],
    |st, ct, w, b| {
        assert_eq!(st, 200, "{b}");
        assert!(b.starts_with("---\n"), "{b}");
    }
);
header_case!(
    assert_status_rejects_a_mismatch,
    "/page",
    [("x-assert-status-code", "201")],
    |st, ct, w, b| {
        assert_eq!(st, 422, "{b}");
        assert!(b.contains("ASSERTION_FAILED"), "{b}");
    }
);
header_case!(
    md_defaults_are_accepted,
    "/page",
    [
        ("x-md-heading-style", "atx"),
        ("x-md-bullet-list-marker", "-"),
        ("x-md-link-style", "inlined")
    ],
    |st, ct, w, b| {
        assert_eq!(st, 200, "{b}");
    }
);
header_case!(
    md_link_discarded_keeps_the_text,
    "/page",
    [("x-md-link-style", "discarded")],
    |st, ct, w, b| {
        assert_eq!(st, 200, "{b}");
        // 视频链接归 x-retain-media，这里只看普通链接
        assert!(b.contains("Read the documentation and"), "{b}");
    }
);
header_case!(
    iframe_false_does_not_need_a_browser,
    "/page",
    [("x-with-iframe", "false"), ("x-with-shadow-dom", "false")],
    |st, ct, w, b| {
        assert_eq!(st, 200, "{b}");
    }
);

#[tokio::test]
async fn headers_it_cannot_honour_are_refused() {
    for (k, v) in [
        ("x-md-heading-style", "setext"),
        ("x-md-bullet-list-marker", "*"),
        ("x-md-em-delimiter", "_"),
        ("x-md-strong-delimiter", "__"),
        ("x-md-hr", "***"),
        ("x-md-link-style", "referenced"),
        ("x-md-link-reference-style", "full"),
        ("x-no-gfm", "true"),
        ("x-keep-img-data-url", "true"),
        ("x-remove-overlay", "true"),
        ("x-page", "2"),
        ("x-base", "initial"),
        ("x-robots-txt", "Googlebot"),
    ] {
        let (st, _, _, b) = read("/page", &[(k, v)]).await;
        assert_eq!(st, 400, "{k}: {b}");
        assert!(b.contains("UNSUPPORTED_OPTION"), "{k}: {b}");
    }
}

#[tokio::test]
async fn sse_applies_the_output_headers() {
    let (st, _, _, b) = read(
        "/page",
        &[("accept", "text/event-stream"), ("x-respond-with", "html")],
    )
    .await;
    assert_eq!(st, 200, "{b}");
    assert!(b.contains("event: content"), "{b}");
    assert!(
        b.contains("<html") || b.contains("<body") || b.contains("<p"),
        "{b}"
    );
    let (_, _, _, b) = read(
        "/page",
        &[("accept", "text/event-stream"), ("x-max-tokens", "3")],
    )
    .await;
    assert!(b.contains("output_truncated"), "{b}");
    let (_, _, _, b) = read(
        "/page",
        &[("accept", "text/event-stream"), ("x-token-budget", "1")],
    )
    .await;
    assert!(b.contains("event: error") && b.contains("TOO_LARGE"), "{b}");
}
