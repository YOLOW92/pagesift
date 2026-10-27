//! 0.80.2 聚焦爬取在"大学官网"式站点上的端到端测试：导航里的链接按 DOM 锚文本打分、
//! 子域名默认在范围内、家具页（新闻、关于）排在内容页后面、错误状态页不算页面、
//! 结果里带调度轨迹（深度、父页、分数）、相关页面上的 PDF 作为附件单独收割。
//! 站点按 Host 路由（`uni.localhost` 和 `iss.uni.localhost` 都指向本机），并发 1，抓取顺序就是调度顺序。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use pagesift::api::types::{CrawlRequest, CrawlSummary, Document};
use pagesift::api::{ApiError, BoxFuture, CrawlSink};
use pagesift::{Scraper, ScraperConfig};
use serde_json::json;

const FILLER: &str = "This page of the university describes the campus, the people who work here and the things they do every day.";

/// 一个带导航栏的模板页：导航在 `<article>` 外面，正文里只有段落和正文链接。
fn render(
    title: &str,
    nav: &[(&str, &str)],
    paragraphs: &[&str],
    links: &[(&str, &str)],
) -> String {
    let privacy = nav[0].0.trim_end_matches('/').to_string() + "/privacy/";
    let mut nav_html = String::from("<nav><ul>");
    for (href, text) in nav {
        nav_html.push_str(&format!("<li><a href=\"{href}\">{text}</a></li>"));
    }
    nav_html.push_str("</ul></nav>");
    let mut body = format!("<h1>{title}</h1><p>{title}. {FILLER}</p>");
    for p in paragraphs {
        body.push_str(&format!("<p>{p}</p>"));
    }
    for (href, text) in links {
        body.push_str(&format!("<p>See also: <a href=\"{href}\">{text}</a>.</p>"));
    }
    format!(
        "<html><head><title>{title}</title></head><body>{nav_html}<main><article>{body}</article></main><footer><a href=\"{privacy}\">Privacy</a></footer></body></html>"
    )
}

/// 一页 PDF，正文提到查询里的词。
fn pdf(title: &str) -> Vec<u8> {
    let content = format!(
        "BT /F1 18 Tf 72 720 Td ({title}) Tj ET BT /F1 11 Tf 72 690 Td (Tuition fees and scholarship deadlines for international students.) Tj ET"
    );
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Count 1 /Kids [3 0 R] >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

async fn serve(headers: HeaderMap, uri: Uri) -> Response {
    let host_port = headers[header::HOST]
        .to_str()
        .unwrap_or_default()
        .to_string();
    let (host, port) = host_port.split_once(':').unwrap_or((&host_port, "80"));
    let path = uri.path();
    if path == "/robots.txt" {
        return "User-agent: *\nAllow: /\n".into_response();
    }
    // 导航链接写成绝对地址：子域名上的页面也用同一份导航
    let www = format!("http://uni.localhost:{port}");
    let abs = |p: &str| format!("{www}{p}");
    let (home, news, about, adm, intl) = (
        abs("/"),
        abs("/news/"),
        abs("/about/"),
        abs("/admissions/"),
        abs("/admissions/international/"),
    );
    let iss = format!("http://iss.uni.localhost:{port}/");
    let nav: &[(&str, &str)] = &[
        (&home, "Home"),
        (&news, "News"),
        (&about, "About"),
        (&adm, "Admissions"),
        (&intl, "International students"),
        (&iss, "International Student Services"),
    ];
    let page = |title: &str, paragraphs: &[&str], links: &[(&str, &str)]| {
        Html(render(title, nav, paragraphs, links)).into_response()
    };
    if host.starts_with("iss.") {
        return match path {
            "/" => page(
                "International Student Services",
                &["Visa advice, orientation and support for international students."],
                &[],
            ),
            _ => (
                StatusCode::NOT_FOUND,
                Html(render("Not found", nav, &[], &[])),
            )
                .into_response(),
        };
    }
    match path {
        "/" => page("University home", &[], &[("/gone/", "Campus map")]),
        "/news/" => page(
            "News",
            &["International students win the regional scholarship prize."],
            &[("/news/2026/05/story/", "International students celebrate")],
        ),
        "/news/2026/05/story/" => page(
            "Story",
            &["International students celebrated the festival on campus."],
            &[],
        ),
        "/about/" => page(
            "About the university",
            &[],
            &[("/about/history/", "History")],
        ),
        "/about/history/" => page("History", &[], &[]),
        "/admissions/" => page(
            "Admissions",
            &["How to apply for admission, tuition fees and scholarships."],
            &[("/admissions/international/", "International students")],
        ),
        "/admissions/international/" => page(
            "International students",
            &[
                "Admission requirements, tuition, scholarship and visa information for international students.",
            ],
            &[
                ("/files/fees.pdf", "Tuition fees (PDF)"),
                ("/files/visa.pdf", "Visa guide (PDF)"),
                ("/admissions/international/apply/", "How to apply"),
            ],
        ),
        "/admissions/international/apply/" => page(
            "How to apply",
            &["Submit the admission form with your visa and scholarship documents."],
            &[],
        ),
        "/files/fees.pdf" => {
            ([("content-type", "application/pdf")], pdf("Tuition fees")).into_response()
        }
        "/files/visa.pdf" => {
            ([("content-type", "application/pdf")], pdf("Visa guide")).into_response()
        }
        "/privacy/" => page("Privacy", &[], &[]),
        // 真实站点的 404 也是带导航、带查询词的模板页：不能当内容，也不能跟它的链接
        "/gone/" => (
            StatusCode::NOT_FOUND,
            Html(render(
                "Page not found",
                nav,
                &["International students admission tuition scholarship visa: the page moved."],
                &[("/gone/deeper/", "International students admission")],
            )),
        )
            .into_response(),
        _ => (
            StatusCode::NOT_FOUND,
            Html(render("Not found", nav, &[], &[])),
        )
            .into_response(),
    }
}

async fn site() -> String {
    let app = Router::new().route("/", get(serve)).fallback(get(serve));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://uni.localhost:{port}")
}

#[derive(Default)]
struct Collect {
    pages: Mutex<Vec<Document>>,
    errors: Mutex<Vec<(String, String)>>,
    summary: Mutex<Option<CrawlSummary>>,
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
            .push((url.to_string(), err.message.clone()));
        Box::pin(async {})
    }
    fn total(&self, _: usize) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn cancelled(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { false })
    }
    fn summary(&self, s: CrawlSummary) -> BoxFuture<'_, ()> {
        *self.summary.lock().unwrap() = Some(s);
        Box::pin(async {})
    }
}

const QUERY: &str = "international students admission tuition scholarship visa";

async fn crawl(base: &str, extra: serde_json::Value) -> Arc<Collect> {
    let mut v = json!({
        "url": format!("{base}/"),
        "sitemap": "skip",
        "maxConcurrency": 1,
        "limit": 6,
        "scrapeOptions": { "formats": ["markdown"], "engine": "http" },
    });
    v.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let req: CrawlRequest = serde_json::from_value(v).unwrap();
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    let sink = Arc::new(Collect::default());
    Scraper::new(cfg)
        .unwrap()
        .crawl(&req, sink.clone())
        .await
        .unwrap();
    sink
}

/// 按抓取顺序排列的路径（含主机，去掉端口）。
fn order(c: &Collect) -> Vec<String> {
    c.pages
        .lock()
        .unwrap()
        .iter()
        .map(|d| {
            let u = url::Url::parse(d.metadata.url.as_deref().unwrap()).unwrap();
            format!("{}{}", u.host_str().unwrap(), u.path())
        })
        .collect()
}

#[tokio::test]
async fn nav_links_are_scored_by_their_anchor_text() {
    // 首页正文里没有相关链接；导航里的 "International students" 和子域名入口靠锚文本排到前面，
    // 新闻、关于、隐私这些家具页排在最后
    let base = site().await;
    let c = crawl(
        &base,
        json!({ "query": QUERY, "excludePaths": ["^/files/"] }),
    )
    .await;
    let o = order(&c);
    assert_eq!(o[0], "uni.localhost/", "{o:?}");
    assert!(
        o[1..3].contains(&"uni.localhost/admissions/international/".to_string())
            && o[1..3].contains(&"iss.uni.localhost/".to_string()),
        "{o:?}"
    );
    let pos = |p: &str| o.iter().position(|x| x == p).unwrap_or(usize::MAX);
    assert!(
        pos("uni.localhost/admissions/") < pos("uni.localhost/news/"),
        "{o:?}"
    );
    assert!(
        pos("uni.localhost/news/") > 3 || pos("uni.localhost/news/") == usize::MAX,
        "{o:?}"
    );
    assert!(
        !o.iter()
            .any(|p| p.contains("/about/") || p.contains("/privacy/")),
        "{o:?}"
    );
}

#[tokio::test]
async fn subdomains_are_in_scope_only_for_focused_crawls_by_default() {
    let base = site().await;
    let plain = crawl(&base, json!({ "limit": 20 })).await;
    assert!(
        !order(&plain).iter().any(|p| p.starts_with("iss.")),
        "{:?}",
        order(&plain)
    );
    let focused = crawl(&base, json!({ "query": QUERY, "limit": 20 })).await;
    assert!(order(&focused).iter().any(|p| p == "iss.uni.localhost/"));
    let off = crawl(
        &base,
        json!({ "query": QUERY, "limit": 20, "allowSubdomains": false }),
    )
    .await;
    assert!(!order(&off).iter().any(|p| p.starts_with("iss.")));
}

#[tokio::test]
async fn error_pages_are_errors_not_content() {
    // /gone/ 是 404 模板页，满是查询词、还链着 /gone/deeper/：报成错误，不输出，也不跟它的链接
    let base = site().await;
    let c = crawl(&base, json!({ "query": QUERY, "limit": 20 })).await;
    let o = order(&c);
    assert!(!o.iter().any(|p| p.contains("/gone")), "{o:?}");
    let errors = c.errors.lock().unwrap().clone();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].0.ends_with("/gone/") && errors[0].1.contains("HTTP 404"),
        "{errors:?}"
    );
}

#[tokio::test]
async fn crawl_trace_records_depth_parent_and_score() {
    let base = site().await;
    let c = crawl(&base, json!({ "query": QUERY })).await;
    let pages = c.pages.lock().unwrap();
    let trace = |path: &str| {
        pages
            .iter()
            .find(|d| d.metadata.url.as_deref().unwrap().ends_with(path))
            .unwrap_or_else(|| panic!("{path} not crawled"))
            .metadata
            .pagesift
            .as_ref()
            .unwrap()
            .crawl
            .clone()
            .unwrap()
    };
    let home = pages[0]
        .metadata
        .pagesift
        .as_ref()
        .unwrap()
        .crawl
        .clone()
        .unwrap();
    assert_eq!((home.depth, home.parent, home.score), (0, None, None));
    let intl = trace("/admissions/international/");
    assert_eq!(intl.depth, 1);
    assert_eq!(intl.parent.as_deref(), Some(format!("{base}/").as_str()));
    assert!(intl.score.unwrap() > 0.0);
    assert!(intl.attachment_of.is_none());
    // 轨迹和相关度一起序列化在 metadata.pagesift.crawl 下
    let v = serde_json::to_value(&pages[1]).unwrap();
    assert!(
        v["metadata"]["pagesift"]["crawl"]["score"].is_number(),
        "{v}"
    );
}

#[tokio::test]
async fn attachments_are_harvested_from_relevant_pages_with_their_own_budget() {
    let base = site().await;
    let seed = format!("{base}/admissions/");
    // 不开附件：从 /admissions/ 起爬，/files/ 不在路径范围里，PDF 不抓
    let c = crawl(&base, json!({ "url": seed, "query": QUERY, "limit": 20 })).await;
    assert!(
        !order(&c).iter().any(|p| p.ends_with(".pdf")),
        "{:?}",
        order(&c)
    );
    // 开了附件：相关页面上的 PDF 不受路径范围限制，按自己的预算抓回来，记着来源页
    let c = crawl(
        &base,
        json!({ "url": seed, "query": QUERY, "limit": 4, "attachments": true, "attachmentLimit": 1 }),
    )
    .await;
    let o = order(&c);
    let pdfs: Vec<&String> = o.iter().filter(|p| p.ends_with(".pdf")).collect();
    assert_eq!(pdfs.len(), 1, "{o:?}");
    assert_eq!(o.len(), 5, "4 pages + 1 attachment: {o:?}");
    let pages = c.pages.lock().unwrap();
    let doc = pages
        .iter()
        .find(|d| d.metadata.url.as_deref().unwrap().ends_with(".pdf"))
        .unwrap();
    let trace = doc
        .metadata
        .pagesift
        .as_ref()
        .unwrap()
        .crawl
        .clone()
        .unwrap();
    assert_eq!(
        trace.attachment_of.as_deref(),
        Some(format!("{base}/admissions/international/").as_str())
    );
    assert!(
        doc.markdown
            .as_deref()
            .unwrap()
            .contains("international students"),
        "{:?}",
        doc.markdown
    );
    let s = c.summary.lock().unwrap().clone().unwrap();
    assert_eq!(s.pages, 5);
}

#[tokio::test]
async fn attachments_need_a_query() {
    let base = site().await;
    let req: CrawlRequest =
        serde_json::from_value(json!({ "url": format!("{base}/"), "attachments": true })).unwrap();
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    let err = Scraper::new(cfg)
        .unwrap()
        .crawl(&req, Arc::new(Collect::default()))
        .await
        .unwrap_err();
    assert!(err.message.contains("query"), "{err:?}");
}
