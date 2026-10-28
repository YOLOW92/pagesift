//! 单页抓取的端到端行为：错误分类、总时限、命名出口、作者字段、渲染判定。
//! 用真实的 Scraper 抓本机的测试站点，不需要浏览器（`engine: auto` 时没有浏览器就照常返回 HTTP 结果）。

use std::time::{Duration, Instant};

use axum::Router;
use axum::response::Html;
use axum::routing::get;
use pagesift::api::ErrorCode;
use pagesift::api::types::ScrapeOptions;
use pagesift::{Scraper, ScraperConfig};
use serde_json::json;

async fn site() -> String {
    let app = Router::new()
        .route(
            "/article",
            get(|| async {
                Html(
                    r#"<html lang="ja"><head><title>短いニュース</title>
                    <meta property="og:type" content="article">
                    <meta name="author" content="山田 太郎">
                    <meta property="article:published_time" content="2026-10-21T01:00:00Z">
                    </head><body><article><h1>短いニュース</h1>
                    <p>東京で新しい研究施設が開設された。施設は人工知能の基礎研究を担い、国内外の研究者が集まる予定だ。</p>
                    <p>開所式には大学や企業の関係者が出席し、今後五年間で百人規模の研究チームを編成する計画が説明された。地域経済への波及効果も期待されている。</p>
                    </article></body></html>"#,
                )
            }),
        )
        .route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Html("<p>late</p>")
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn scraper(egress: Vec<(String, String)>) -> Scraper {
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::ZERO;
    cfg.egress = egress;
    Scraper::new(cfg).unwrap()
}

fn opts(v: serde_json::Value) -> ScrapeOptions {
    serde_json::from_value(v).unwrap()
}

#[tokio::test]
async fn short_cjk_article_is_kept_from_http_with_author() {
    // 正文约 110 个字：按字符数远不到 1200，加权后也不到，但认出是文章、超过 300，不该渲染
    let base = site().await;
    let s = scraper(Vec::new());
    let doc = s
        .scrape(
            &format!("{base}/article"),
            &opts(json!({ "ignoreRobotsTxt": true })),
        )
        .await
        .unwrap();
    let meta = &doc.metadata;
    assert_eq!(meta.author.as_deref(), Some("山田 太郎"));
    let ext = meta.pagesift.as_ref().unwrap();
    assert_eq!(ext.engine.as_deref(), Some("http"));
    // 认出是文章、加权字数够：不升级到浏览器，也就没有“渲染不可用”的警告
    assert!(
        !ext.warnings.iter().any(|w| w.starts_with("render_")),
        "{:?}",
        ext.warnings
    );
}

#[tokio::test]
async fn per_host_wait_is_reported_separately_from_fetch_time() {
    let base = site().await;
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(80);
    cfg.policy.rate.per_host_concurrency = 2;
    let s = Scraper::new(cfg).unwrap();
    let o = opts(json!({ "engine": "http", "ignoreRobotsTxt": true }));
    let url = format!("{base}/article");

    let (a, b) = tokio::join!(s.scrape(&url, &o), s.scrape(&url, &o));
    let docs = [a.unwrap(), b.unwrap()];
    let queued = docs
        .iter()
        .map(|d| d.metadata.pagesift.as_ref().unwrap().timings["queue"])
        .collect::<Vec<_>>();
    assert!(queued.iter().any(|ms| *ms >= 60.0), "{queued:?}");
    let waited = docs.iter().any(|d| {
        d.metadata
            .pagesift
            .as_ref()
            .unwrap()
            .warnings
            .iter()
            .any(|w| {
                w.starts_with("request waited ")
                    && w.ends_with(" in the rate-limit/admission queue")
            })
    });
    assert!(waited, "queue wait should be visible in warnings");
}

#[tokio::test]
async fn timeout_is_a_total_deadline() {
    let base = site().await;
    let s = scraper(Vec::new());
    let t = Instant::now();
    let err = s
        .scrape(
            &format!("{base}/slow"),
            &opts(json!({ "timeout": 1000, "ignoreRobotsTxt": true })),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Timeout, "{err}");
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
}

#[tokio::test]
async fn dns_failures_and_unknown_egress_are_distinct_errors() {
    let s = scraper(Vec::new());
    let err = s
        .scrape(
            "http://pagesift-test.invalid/",
            &opts(json!({ "engine": "http", "ignoreRobotsTxt": true })),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::DnsFailure, "{err}");

    let base = site().await;
    let err = s
        .scrape(
            &format!("{base}/article"),
            &opts(json!({ "egress": "nope", "ignoreRobotsTxt": true })),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidRequest, "{err}");
}

#[tokio::test]
async fn egress_is_used_for_http_and_refused_for_browser() {
    let base = site().await;
    // 出口指向一个不存在的代理：请求确实走了它，所以连不上
    let s = scraper(vec![("dead".into(), "http://127.0.0.1:9".into())]);
    let err = s
        .scrape(
            &format!("{base}/article"),
            &opts(json!({ "egress": "dead", "engine": "http", "ignoreRobotsTxt": true })),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err.code, ErrorCode::NetworkError | ErrorCode::UpstreamError),
        "{err}"
    );
    let err = s
        .scrape(
            &format!("{base}/article"),
            &opts(json!({ "egress": "dead", "engine": "browser" })),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::UnsupportedOption, "{err}");
}

#[tokio::test]
async fn an_engine_that_cannot_launch_is_skipped_after_the_first_failure() {
    let base = site().await;
    // 假浏览器：像沙箱不可用的 Chrome 一样，报错退出；每次启动记一行，数启动了几次
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("chrome");
    let launches = dir.path().join("launches");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\necho x >> '{}'\necho 'FATAL: No usable sandbox!' >&2\nexit 1\n",
            launches.display()
        ),
    )
    .unwrap();
    let launched = || {
        std::fs::read_to_string(&launches)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    };
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::ZERO;
    cfg.browser.source = pagesift_cdp::BrowserSource::Executable(fake);
    // 自定义参数时内核链只有它自己，不会回退到本机的其他浏览器
    cfg.browser.args_override = Some(vec!["--headless".into()]);
    let s = Scraper::new(cfg).unwrap();
    let o = opts(json!({ "engine": "browser", "ignoreRobotsTxt": true, "timeout": 10000 }));
    let url = format!("{base}/article");

    let err = s.scrape(&url, &o).await.unwrap_err();
    assert!(err.message.contains("sandbox"), "{err}");
    // root 下不启动就直接报沙箱错误，这时是 0 次；否则启动过一次
    let first = launched();
    // 第二次不再去启动它
    let err = s.scrape(&url, &o).await.unwrap_err();
    assert_eq!(launched(), first, "the failed engine was launched again");
    assert_eq!(err.code, ErrorCode::UpstreamError, "{err}");
}
