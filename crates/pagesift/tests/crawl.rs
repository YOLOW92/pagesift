//! 整站爬取和 map 的端到端测试：本地起一个小站点（带 robots.txt、sitemap、外链、深层页面），
//! 用真实的 Scraper 去爬。为了能访问本机地址，测试里放开了私网限制；限速调快。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::response::Html;
use axum::routing::get;
use pagesift::api::types::{CrawlRequest, Document, MapRequest};
use pagesift::api::{ApiError, BoxFuture, CrawlResume, CrawlSink};
use pagesift::{Scraper, ScraperConfig};
use serde_json::json;

fn page(title: &str, links: &[&str]) -> Html<String> {
    let links: String = links
        .iter()
        .map(|l| format!("<p>See <a href=\"{l}\">{l}</a> for more about this topic.</p>"))
        .collect();
    Html(format!(
        "<html><head><title>{title}</title></head><body><article><h1>{title}</h1>\
         <p>{title} has enough text to be the main content of the page, with several clauses.</p>{links}</article></body></html>"
    ))
}

async fn site() -> String {
    let app = Router::new()
        .route(
            "/robots.txt",
            get(|| async { "User-agent: *\nDisallow: /docs/secret\nSitemap: /sitemap.xml\n" }),
        )
        .route(
            "/sitemap.xml",
            get(|| async {
                "<urlset><url><loc>http://HOST/docs/from-sitemap</loc></url></urlset>"
            }),
        )
        .route(
            "/docs/",
            get(|| async {
                page(
                    "Docs home",
                    &[
                        "/docs/a",
                        "/docs/b?x=1#frag",
                        "/blog/out-of-scope",
                        "https://external.example/x",
                    ],
                )
            }),
        )
        .route(
            "/docs/a",
            get(|| async { page("Page A", &["/docs/a/deep", "/docs/secret", "/docs/b?x=1"]) }),
        )
        .route("/docs/b", get(|| async { page("Page B", &["/docs/"]) }))
        .route(
            "/docs/a/deep",
            get(|| async { page("Deep page", &["/docs/a/deeper"]) }),
        )
        .route("/docs/a/deeper", get(|| async { page("Deeper page", &[]) }))
        .route(
            "/docs/from-sitemap",
            get(|| async { page("From sitemap", &[]) }),
        )
        .route("/docs/secret", get(|| async { page("Secret", &[]) }))
        .route("/blog/out-of-scope", get(|| async { page("Blog", &[]) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn scraper() -> Scraper {
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    cfg.policy.rate.per_host_concurrency = 8;
    Scraper::new(cfg).unwrap()
}

#[derive(Default)]
struct Collect {
    pages: Mutex<Vec<Document>>,
    errors: Mutex<Vec<(String, String)>>,
}

impl CrawlSink for Collect {
    fn page(&self, doc: Document) -> BoxFuture<'_, ()> {
        self.pages.lock().unwrap().push(doc);
        Box::pin(async {})
    }
    fn error<'a>(&'a self, url: &'a str, err: ApiError) -> BoxFuture<'a, ()> {
        self.errors
            .lock()
            .unwrap()
            .push((url.to_string(), err.code.as_str().to_string()));
        Box::pin(async {})
    }
    fn total(&self, _: usize) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn cancelled(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { false })
    }
}

fn crawl_req(base: &str, extra: serde_json::Value) -> CrawlRequest {
    let mut v = json!({ "url": format!("{base}/docs/"), "scrapeOptions": { "formats": ["markdown"], "engine": "http" } });
    v.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(v).unwrap()
}

fn titles(c: &Collect) -> Vec<String> {
    let mut t: Vec<String> = c
        .pages
        .lock()
        .unwrap()
        .iter()
        .filter_map(|d| d.metadata.title.clone())
        .collect();
    t.sort();
    t
}

#[tokio::test]
async fn crawl_follows_links_within_scope_and_respects_robots() {
    let base = site().await;
    let s = scraper();
    let sink = Arc::new(Collect::default());
    s.crawl(
        &crawl_req(&base, json!({ "sitemap": "skip" })),
        sink.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        titles(&sink),
        ["Deep page", "Deeper page", "Docs home", "Page A", "Page B"]
    );
    let errors = sink.errors.lock().unwrap().clone();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].0.ends_with("/docs/secret") && errors[0].1 == "ROBOTS_DISALLOWED");
    assert!(
        sink.pages.lock().unwrap().iter().all(|d| d.links.is_none()),
        "links not requested"
    );
}

/// 持久化任务的最小模型：记下入队顺序，可以从给定的进度恢复。
#[derive(Default)]
struct Resumable {
    collect: Collect,
    frontier: Mutex<Vec<(String, usize)>>,
    resume: Mutex<Option<CrawlResume>>,
}

impl Resumable {
    fn from(resume: CrawlResume) -> Arc<Self> {
        Arc::new(Self {
            resume: Mutex::new(Some(resume)),
            ..Default::default()
        })
    }

    /// 抓过的 URL（成功的按 sourceURL，失败的按出错的 URL）。
    fn visited(&self) -> std::collections::HashSet<String> {
        let c = &self.collect;
        let pages = c.pages.lock().unwrap();
        let errors = c.errors.lock().unwrap();
        pages
            .iter()
            .filter_map(|d| d.metadata.source_url.clone())
            .chain(errors.iter().map(|(u, _)| u.clone()))
            .collect()
    }
}

impl CrawlSink for Resumable {
    fn page(&self, doc: Document) -> BoxFuture<'_, ()> {
        self.collect.page(doc)
    }
    fn error<'a>(&'a self, url: &'a str, err: ApiError) -> BoxFuture<'a, ()> {
        self.collect.error(url, err)
    }
    fn total(&self, _: usize) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn cancelled(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { false })
    }
    fn resume(&self) -> Option<CrawlResume> {
        self.resume.lock().unwrap().take()
    }
    fn tracks_frontier(&self) -> bool {
        true
    }
    fn enqueued(&self, urls: Vec<(String, usize)>) -> BoxFuture<'_, ()> {
        self.frontier.lock().unwrap().extend(urls);
        Box::pin(async {})
    }
}

#[tokio::test]
async fn crawl_reports_its_frontier_and_resumes_from_it() {
    let base = site().await;
    let s = scraper();
    let req = crawl_req(&base, json!({ "sitemap": "skip" }));
    let first = Arc::new(Resumable::default());
    s.crawl(&req, first.clone()).await.unwrap();
    let frontier = first.frontier.lock().unwrap().clone();
    assert_eq!(frontier[0], (format!("{base}/docs/"), 0));
    let urls: Vec<String> = frontier.iter().map(|(u, _)| u.clone()).collect();
    assert_eq!(
        urls.iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>(),
        first.visited(),
        "every page that was fetched went through the reported queue"
    );
    assert_eq!(urls.len(), 6, "{urls:?}");

    // 假装前两页做完了：只抓剩下的，不重抓、不重新发现
    let done = 2;
    let resume = CrawlResume {
        enqueued: urls.clone(),
        pending: frontier[done..].to_vec(),
        done,
    };
    let second = Resumable::from(resume.clone());
    s.crawl(&req, second.clone()).await.unwrap();
    assert_eq!(
        second.visited(),
        urls[done..].iter().cloned().collect(),
        "only the unfinished pages are fetched"
    );
    assert!(
        second.frontier.lock().unwrap().is_empty(),
        "every link was already seen before the restart"
    );

    // 已完成的页数计入 limit
    let third = Resumable::from(resume);
    s.crawl(
        &crawl_req(&base, json!({ "sitemap": "skip", "limit": done + 1 })),
        third.clone(),
    )
    .await
    .unwrap();
    assert_eq!(third.visited().len(), 1);
}

#[tokio::test]
async fn crawl_honours_depth_limit_and_sitemap() {
    let base = site().await;
    let s = scraper();
    let sink = Arc::new(Collect::default());
    // sitemap 里的地址写的是 HOST 占位符，不可解析的条目会被跳过，所以这里只验证深度和 limit
    s.crawl(
        &crawl_req(&base, json!({ "maxDiscoveryDepth": 1, "limit": 3 })),
        sink.clone(),
    )
    .await
    .unwrap();
    let t = titles(&sink);
    assert_eq!(t.len(), 3, "{t:?}");
    assert!(t.contains(&"Docs home".to_string()));
    assert!(!t.contains(&"Deeper page".to_string()));
}

#[tokio::test]
async fn crawl_rejects_bad_patterns_and_unauthorised_robots_override() {
    let base = site().await;
    let s = scraper();
    let sink = Arc::new(Collect::default());
    let err = s
        .crawl(
            &crawl_req(&base, json!({ "includePaths": ["("] })),
            sink.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code.as_str(), "INVALID_REQUEST");
    let err = s
        .crawl(&crawl_req(&base, json!({ "ignoreRobotsTxt": true })), sink)
        .await
        .unwrap_err();
    assert_eq!(err.code.as_str(), "UNSUPPORTED_OPTION");
}

#[tokio::test]
async fn map_lists_site_urls_with_search_ranking() {
    let base = site().await;
    let s = scraper();
    let req: MapRequest =
        serde_json::from_value(json!({ "url": format!("{base}/docs/"), "search": "page-b b" }))
            .unwrap();
    let links: Vec<String> = s
        .map(&req)
        .await
        .unwrap()
        .into_iter()
        .map(|l| l.url)
        .collect();
    assert!(links.contains(&format!("{base}/docs/a")), "{links:?}");
    assert!(
        links
            .iter()
            .any(|l| l.starts_with(&format!("{base}/docs/b"))),
        "{links:?}"
    );
    assert!(
        !links.iter().any(|l| l.contains("external.example")),
        "{links:?}"
    );
    assert!(
        links[0].contains("/docs/b"),
        "search ranking puts matches first: {links:?}"
    );
}

#[tokio::test]
async fn cache_hits_skip_the_network() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let app = Router::new().route(
        "/p",
        get(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            async { page("Cached page", &[]) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/p", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    cfg.store = Some(pagesift_store::Store::open_dir(dir.path()).unwrap());
    let s = Scraper::new(cfg).unwrap();
    let opts = |extra: serde_json::Value| -> pagesift::api::types::ScrapeOptions {
        let mut v = json!({ "engine": "http", "ignoreRobotsTxt": true });
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(v).unwrap()
    };

    let first = s.scrape(&url, &opts(json!({}))).await.unwrap();
    assert_eq!(first.metadata.cache_state.as_deref(), Some("miss"));
    let second = s.scrape(&url, &opts(json!({}))).await.unwrap();
    assert_eq!(second.metadata.cache_state.as_deref(), Some("hit"));
    assert!(second.metadata.cached_at.is_some());
    assert_eq!(second.markdown, first.markdown);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "second scrape served from cache"
    );

    s.scrape(&url, &opts(json!({ "maxAge": 0 }))).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 2, "maxAge 0 always refetches");
    s.scrape(&url, &opts(json!({ "headers": { "x-test": "1" } })))
        .await
        .unwrap();
    assert_eq!(
        hits.load(Ordering::SeqCst),
        3,
        "custom headers bypass the cache"
    );

    let other = format!("{url}?never-cached");
    let err = s
        .scrape(&other, &opts(json!({ "minAge": 1 })))
        .await
        .unwrap_err();
    assert_eq!(err.code.as_str(), "NOT_FOUND");
}

#[tokio::test]
async fn discovery_candidates_keep_robots_scope_and_read_only_semantics() {
    let visits = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = visits.clone();
    let app = Router::new().fallback(get(move |uri: axum::http::Uri| {
        let observed = observed.clone();
        async move {
            let path = uri.path().to_string();
            observed.lock().unwrap().push(path.clone());
            if path == "/robots.txt" {
                return ([ ("content-type", "text/plain"), ("link", "") ], "User-agent: *\nDisallow: /docs/blocked\n".to_string());
            }
            let body = if path == "/docs/" {
                r#"<article><h1>Documents</h1><iframe src="/docs/frame"></iframe>
                <div hx-get="/docs/fragment">Read fragment</div>
                <div hx-get="/docs/blocked">Disallowed</div><div hx-get="/outside">Outside scope</div>
                <button hx-post="/docs/write">Write</button><link rel="preload" href="/docs/resource">
                <script>const route='/docs/script';</script></article>"#.to_string()
            } else { format!("<article><h1>{path}</h1><p>Independent useful document content.</p></article>") };
            ([ ("content-type", "text/html"), ("link", if path == "/docs/" { "</docs/next>; rel=next, </docs/preload>; rel=preload" } else { "" }) ], body)
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let sink = Arc::new(Collect::default());
    let req = crawl_req(
        &base,
        json!({"sitemap":"skip", "limit":20, "maxDiscoveryDepth":2}),
    );
    scraper().crawl(&req, sink).await.unwrap();
    let seen = visits.lock().unwrap().clone();
    for path in ["/docs/", "/docs/frame", "/docs/fragment", "/docs/next"] {
        assert!(seen.contains(&path.to_string()), "missing {path}: {seen:?}");
    }
    for path in [
        "/docs/blocked",
        "/outside",
        "/docs/write",
        "/docs/resource",
        "/docs/preload",
        "/docs/script",
    ] {
        assert!(
            !seen.contains(&path.to_string()),
            "unexpected {path}: {seen:?}"
        );
    }
    server.abort();
}
