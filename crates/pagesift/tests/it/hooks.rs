//! 扩展点：每种钩子一个最小示例（真实引擎抓本机站点），以及 `[[rules]]` 编译出的规则。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::http::HeaderMap;
use axum::response::Html;
use axum::routing::get;
use pagesift::api::types::{Document, ScrapeOptions};
use pagesift::hooks::{
    DocumentHook, Flow, Hooks, Request, RequestHook, ResponseHook, SinkHook, Verdict,
};
use pagesift::{Scraper, ScraperConfig};

async fn site(hits: Arc<AtomicUsize>) -> String {
    let app = Router::new().route(
        "/{*p}",
        get(move |h: HeaderMap| {
            let n = hits.fetch_add(1, Ordering::SeqCst);
            let lang = h.get("accept-language").and_then(|v| v.to_str().ok()).unwrap_or("none").to_string();
            async move {
                Html(format!("<html><body><article><h1>Hook page</h1><p>Request number {n} with language {lang}, long enough to keep.</p></article></body></html>"))
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    base
}

fn cfg() -> ScraperConfig {
    let mut c = ScraperConfig::default();
    c.fetcher.env_proxy = false;
    c.fetcher.ssrf.allow_private_network = true;
    c.policy.rate.default_interval = Duration::from_millis(1);
    c
}

fn opts() -> ScrapeOptions {
    serde_json::from_value(
        serde_json::json!({"engine": "http", "ignoreRobotsTxt": true, "maxAge": 0}),
    )
    .unwrap()
}

struct Lang;
impl RequestHook for Lang {
    fn before(&self, r: &Request) -> Flow<Request> {
        let mut r = r.clone();
        r.options
            .headers
            .insert("Accept-Language".into(), "fr".into());
        Flow::Replace(r)
    }
    fn skip(&self, r: &Request) -> Option<String> {
        r.url.ends_with("/private").then(|| "private".into())
    }
}
struct RetryFirst;
impl ResponseHook for RetryFirst {
    fn after(&self, _: &str, d: &Document) -> Flow<Verdict> {
        match d.markdown.as_deref() {
            Some(m) if m.contains("number 0") => Flow::Replace(Verdict::Retry),
            Some(m) if m.contains("number 5") => Flow::Replace(Verdict::Drop("five".into())),
            _ => Flow::Pass,
        }
    }
}
struct Upper;
impl DocumentHook for Upper {
    fn document(&self, _: &str, d: &Document) -> Flow<Document> {
        let mut d = d.clone();
        d.markdown = d.markdown.map(|m| m.to_uppercase());
        Flow::Replace(d)
    }
}
struct DropAll;
impl SinkHook for DropAll {
    fn emit(&self, _: &Document) -> Flow<Option<Document>> {
        Flow::Replace(None)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_hook_kind_changes_the_pipeline() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = site(hits.clone()).await;
    let mut h = Hooks::default();
    h.request(0, Arc::new(Lang))
        .response(0, Arc::new(RetryFirst))
        .document(0, Arc::new(Upper))
        .sink(0, Arc::new(DropAll));
    let s = Scraper::builder(cfg()).hook(&h).build().unwrap();
    // 请求钩子加了语言头；响应钩子让第一次（number 0）重试；文档钩子改成大写
    let d = s.scrape(&format!("{base}/a"), &opts()).await.unwrap();
    let md = d.markdown.unwrap();
    assert!(md.contains("NUMBER 1 WITH LANGUAGE FR"), "{md}");
    // 跳过
    let e = s
        .scrape(&format!("{base}/private"), &opts())
        .await
        .unwrap_err();
    assert!(
        e.message.contains("skipped by a request hook"),
        "{}",
        e.message
    );
    // 输出钩子由写结果的一方调用
    assert!(s.hooks().run_sink(Document::default()).is_none());
    for _ in 0..3 {
        let _ = s.scrape(&format!("{base}/b"), &opts()).await;
    }
    let e = s.scrape(&format!("{base}/c"), &opts()).await.unwrap_err();
    assert!(
        e.message.contains("dropped by a response hook: five"),
        "{}",
        e.message
    );
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rules_from_toml_set_headers_and_drop_fields() {
    let base = site(Arc::new(AtomicUsize::new(0))).await;
    let host = base
        .trim_start_matches("http://")
        .split(':')
        .next()
        .unwrap()
        .to_string();
    let cfg_text = format!(
        "[[rules]]\nmatch = {{ host = \"{host}\", path_glob = \"/docs/**\" }}\nheaders = {{ \"Accept-Language\" = \"de\" }}\ndrop_fields = [\"links\"]\n"
    );
    let cfg: pagesift::config::Config = toml::from_str(&cfg_text).unwrap();
    let mut sc = cfg.scraper(false).unwrap();
    sc.fetcher.env_proxy = false;
    sc.fetcher.ssrf.allow_private_network = true;
    let s = Scraper::new(sc).unwrap();
    let mut o = opts();
    o.formats.push(pagesift::api::types::Format::new("links"));
    let d = s.scrape(&format!("{base}/docs/x/y"), &o).await.unwrap();
    assert!(d.markdown.unwrap().contains("language de"));
    assert!(d.links.is_none());
    let other = s.scrape(&format!("{base}/blog"), &o).await.unwrap();
    assert!(!other.markdown.unwrap().contains("language de"));
    for bad in [
        "[[rules]]\ninclude_tags = [\"a[\"]",
        "[[rules]]\nschema = { name = \"t\", baseSelector = \"li\", fields = [{ name = \"a\", selector = \"b\", type = \"nope\" }] }",
    ] {
        let config: pagesift::config::Config = toml::from_str(bad).unwrap();
        assert!(config.scraper(false).is_err(), "{bad}");
    }
    assert!(toml::from_str::<pagesift::config::Config>("[[rules]]\nheadrs = {}").is_err());
    s.shutdown().await;
}
