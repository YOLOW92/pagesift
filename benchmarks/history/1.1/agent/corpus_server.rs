//! `/v2/agent` 评测用的本地“互联网”：把 `benchmarks/data/snapshots` 的页面放在一个端口上，用现有的
//! BM25（`pagesift_extract::bm25_scores`）建索引，搜索接口照 SearXNG（`[search] backend = "searxng"`）。
//!
//! - `GET /search?q=...&format=json`：前 20 条，`results[].url/title/content`（SearXNG 的字段）；
//!   `content` 是页面里和查询最相关的一段。
//! - `GET /p/{id}`：页面。脚本、样式、图片、外链都去掉了，页面不会把抓取方带到实网。
//! - `GET /private/{id}`：`--private` 列出的页面只在这里，`/robots.txt` 禁止抓取，搜索结果也没有摘要。
//! - `GET /sitemap.xml`：全部公开页面，给 map 工具用。
//!
//! 用法：`agent_corpus --snapshots DIR --port N [--private ID,ID]`（语料不提交，用绝对路径指过去）。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use pagesift_extract::{Bm25Options, bm25_scores};
use regex::Regex;
use serde_json::json;

struct Page {
    id: String,
    title: String,
    text: String,
    html: String,
    private: bool,
}

struct Corpus {
    pages: Vec<Page>,
    by_id: HashMap<String, usize>,
}

/// 去掉会发请求的东西（脚本、样式、图片、外链），页面就留在本机。
fn clean(html: &str) -> String {
    let mut out = html.to_string();
    for tag in [
        "script", "style", "noscript", "iframe", "svg", "object", "video", "audio",
    ] {
        let re = Regex::new(&format!(r"(?is)<{tag}\b.*?</{tag}\s*>")).unwrap();
        out = re.replace_all(&out, " ").into_owned();
    }
    let void = Regex::new(r"(?is)<(link|img|source|meta|base|embed)\b[^>]*>").unwrap();
    out = void.replace_all(&out, " ").into_owned();
    let handlers = Regex::new(r#"(?i)\son[a-z]+\s*=\s*("[^"]*"|'[^']*')"#).unwrap();
    out = handlers.replace_all(&out, " ").into_owned();
    let refs = Regex::new(r#"(?i)\s(href|src|action)\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)"#).unwrap();
    refs.replace_all(&out, r##" href="#""##).into_owned()
}

fn load(dir: &str, private: &HashSet<String>) -> Corpus {
    let mut pages = Vec::new();
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {dir}: {e}"))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "html"))
        .collect();
    names.sort();
    for path in names {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let id = name.split('.').next().unwrap_or_default().to_string();
        let html = clean(&String::from_utf8_lossy(&bytes));
        let doc = pagesift_content::process(&html, &Default::default());
        let title = doc.metadata.title.clone().unwrap_or_default();
        if doc.markdown.trim().is_empty() {
            continue;
        }
        pages.push(Page {
            private: private.contains(&id),
            id,
            text: format!("{title}\n\n{}", doc.markdown),
            title,
            html,
        });
    }
    let by_id = pages
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id.clone(), i))
        .collect();
    Corpus { pages, by_id }
}

fn options() -> Bm25Options {
    Bm25Options {
        threshold: None,
        block_weights: false,
        ..Default::default()
    }
}

/// 页面里和查询最相关的一段（摘要）。
fn snippet(page: &Page, q: &str) -> String {
    let blocks: Vec<&str> = page
        .text
        .split("\n\n")
        .filter(|b| !b.trim().is_empty())
        .collect();
    let scores = bm25_scores(&blocks, q, &options());
    let best = (0..blocks.len())
        .max_by(|&a, &b| scores[a].total_cmp(&scores[b]))
        .unwrap_or(0);
    blocks
        .get(best)
        .map(|b| b.chars().take(240).collect())
        .unwrap_or_default()
}

fn host(headers: &HeaderMap) -> String {
    headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1")
        .to_string()
}

#[derive(serde::Deserialize)]
struct SearchParams {
    q: String,
}

async fn search(
    State(c): State<Arc<Corpus>>,
    headers: HeaderMap,
    Query(p): Query<SearchParams>,
) -> impl IntoResponse {
    let host = host(&headers);
    let docs: Vec<&str> = c.pages.iter().map(|x| x.text.as_str()).collect();
    let scores = bm25_scores(&docs, &p.q, &options());
    let mut order: Vec<usize> = (0..docs.len()).filter(|&i| scores[i] > 0.0).collect();
    order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
    let results: Vec<_> = order
        .into_iter()
        .take(20)
        .map(|i| {
            let page = &c.pages[i];
            let (path, content) = if page.private {
                ("private", String::new())
            } else {
                ("p", snippet(page, &p.q))
            };
            json!({
                "url": format!("http://{host}/{path}/{}", page.id),
                "title": page.title,
                "content": content,
            })
        })
        .collect();
    axum::Json(json!({ "query": p.q, "results": results }))
}

fn page(c: &Corpus, id: &str, private: bool) -> impl IntoResponse + use<> {
    match c.by_id.get(id).map(|&i| &c.pages[i]) {
        Some(p) if p.private == private => Html(p.html.clone()).into_response(),
        _ => (StatusCode::NOT_FOUND, "no such page").into_response(),
    }
}

async fn public_page(State(c): State<Arc<Corpus>>, Path(id): Path<String>) -> impl IntoResponse {
    page(&c, &id, false)
}

async fn private_page(State(c): State<Arc<Corpus>>, Path(id): Path<String>) -> impl IntoResponse {
    page(&c, &id, true)
}

async fn sitemap(State(c): State<Arc<Corpus>>, headers: HeaderMap) -> impl IntoResponse {
    let host = host(&headers);
    let urls: String = c
        .pages
        .iter()
        .filter(|p| !p.private)
        .map(|p| format!("<url><loc>http://{host}/p/{}</loc></url>", p.id))
        .collect();
    (
        [("content-type", "application/xml")],
        format!(
            "<?xml version=\"1.0\"?><urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">{urls}</urlset>"
        ),
    )
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let mut snapshots = String::new();
    let mut port = 0u16;
    let mut private = HashSet::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--snapshots" => snapshots = args.next().expect("--snapshots DIR"),
            "--port" => port = args.next().expect("--port N").parse().expect("port"),
            "--private" => private.extend(
                args.next()
                    .expect("--private IDS")
                    .split(',')
                    .map(str::to_string),
            ),
            other => panic!("unknown argument {other}"),
        }
    }
    assert!(!snapshots.is_empty(), "--snapshots DIR is required");
    let corpus = Arc::new(load(&snapshots, &private));
    eprintln!("{} pages indexed", corpus.pages.len());
    let app = Router::new()
        .route("/search", get(search))
        .route("/p/{id}", get(public_page))
        .route("/private/{id}", get(private_page))
        .route("/sitemap.xml", get(sitemap))
        .route(
            "/robots.txt",
            get(|| async { "User-agent: *\nDisallow: /private/\nSitemap: /sitemap.xml\n" }),
        )
        .with_state(corpus);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    println!("ready {}", listener.local_addr().unwrap().port());
    axum::serve(listener, app).await.unwrap();
}
