//! 本地多站点源站：按 Host 头区分站点（`s0.localhost` … `s{H-1}.localhost`），每站 P 页。
//! 页面正文取自评测集的真实 HTML；原有的 href 全部改名失效（改成 data-gone），再注入固定的站内链接：
//! 第 j 页链接到 3j+1、3j+2、3j+3 页和首页，所以从首页广度优先能走到全部 P 页。
//! 可以给每个响应加固定延迟模拟网络往返。
//!
//! `origin <inputs-dir> <port> <hosts> <pages-per-host> [latency-ms]`

use std::fmt::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

struct Site {
    /// 每个语料页面在 `</body>` 处切成前后两段，href 已经改写好：请求时只拼接，不再扫描整页，
    /// 免得源站自己吃掉大量 CPU、拉低被测工具能用的核数。
    pages: Vec<(String, String)>,
    hosts: usize,
    per_host: usize,
    latency: Duration,
    served: AtomicUsize,
}

fn host_index(headers: &HeaderMap, hosts: usize) -> Option<usize> {
    let host = headers.get(header::HOST)?.to_str().ok()?;
    let name = host.split(':').next()?;
    let i: usize = name.strip_prefix('s')?.strip_suffix(".localhost")?.parse().ok()?;
    (i < hosts).then_some(i)
}

/// 把所有 `href` 属性（任意大小写、前面是空白、引号或 `/`、`=` 两边可有空白）改名为 `data-gone`（名字里不能含 href：有的爬虫会把任何带 href 的属性当链接）。
fn disable_hrefs(html: &str) -> String {
    let b = html.as_bytes();
    let mut out = String::with_capacity(html.len() + 1024);
    let mut last = 0;
    let mut i = 1;
    while i + 4 <= b.len() {
        if b[i..i + 4].eq_ignore_ascii_case(b"href")
            && (b[i - 1].is_ascii_whitespace() || matches!(b[i - 1], b'"' | b'\'' | b'/')) {
            let mut j = i + 4;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            if b.get(j) == Some(&b'=') {
                out.push_str(&html[last..i]);
                out.push_str("data-gone");
                last = i + 4;
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&html[last..]);
    out
}

fn render(site: &Site, host: usize, page: usize) -> String {
    let (head, tail) = &site.pages[(host * site.per_host + page) % site.pages.len()];
    let mut out = String::with_capacity(head.len() + tail.len() + 400);
    out.push_str(head);
    out.push_str("<p class=\"crawl-bench-links\">");
    for k in [3 * page + 1, 3 * page + 2, 3 * page + 3] {
        if k < site.per_host {
            let _ = write!(out, "<a href=\"/p/{k}\">Read part {k} of this series in full</a> ");
        }
    }
    out.push_str("<a href=\"/\">Back to the start of the series</a></p>");
    out.push_str(tail);
    out
}

/// 改写 href，再在最后一个 `</body>` 处切开（没有就整页当前段）。
fn prepare(html: &str) -> (String, String) {
    let mut body = disable_hrefs(html);
    match body.rfind("</body>") {
        Some(i) => {
            let tail = body[i..].to_string();
            body.truncate(i);
            body.shrink_to_fit();
            (body, tail)
        }
        None => {
            body.shrink_to_fit();
            (body, String::new())
        }
    }
}

fn load_pages_parallel(files: &[std::path::PathBuf]) -> Vec<(String, String)> {
    let n_threads = std::thread::available_parallelism().map_or(8, |n| n.get()).min(files.len().max(1));
    let chunk_size = files.len().div_ceil(n_threads);
    std::thread::scope(|s| {
        let handles: Vec<_> = files
            .chunks(chunk_size)
            .map(|chunk| {
                s.spawn(move || {
                    chunk
                        .iter()
                        .map(|p| prepare(&String::from_utf8_lossy(&std::fs::read(p).unwrap())))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
    })
}

async fn page(site: &Site, headers: &HeaderMap, page: usize) -> Response {
    tokio::time::sleep(site.latency).await;
    let Some(h) = host_index(headers, site.hosts) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if page >= site.per_host {
        return StatusCode::NOT_FOUND.into_response();
    }
    site.served.fetch_add(1, Ordering::Relaxed);
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        render(site, h, page),
    )
        .into_response()
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: origin <inputs-dir> <port> <hosts> <pages-per-host> [latency-ms]";
    let dir = args.next().expect(usage);
    let port: u16 = args.next().expect(usage).parse().unwrap();
    let hosts: usize = args.next().expect(usage).parse().unwrap();
    let per_host: usize = args.next().expect(usage).parse().unwrap();
    let latency = Duration::from_millis(args.next().map_or(0, |v| v.parse().unwrap()));
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "html"))
        .collect();
    files.sort();
    let max_pages: usize = std::env::var("ORIGIN_MAX_PAGES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5000);
    if max_pages > 0 && files.len() > max_pages {
        files.truncate(max_pages);
    }
    let pages = load_pages_parallel(&files);
    let site = Arc::new(Site {
        pages,
        hosts,
        per_host,
        latency,
        served: AtomicUsize::new(0),
    });
    let app = Router::new()
        .route(
            "/robots.txt",
            get(|| async { "User-agent: *\nAllow: /\n" }),
        )
        .route(
            "/",
            get(|State(s): State<Arc<Site>>, h: HeaderMap| async move { page(&s, &h, 0).await }),
        )
        .route(
            "/p/{n}",
            get(
                |State(s): State<Arc<Site>>, h: HeaderMap, Path(n): Path<usize>| async move {
                    page(&s, &h, n).await
                },
            ),
        )
        .route(
            "/__served",
            get(|State(s): State<Arc<Site>>| async move { s.served.load(Ordering::Relaxed).to_string() }),
        )
        .with_state(site);
    let listener = tokio::net::TcpListener::bind(("::1", port)).await.unwrap();
    eprintln!("origin: {hosts} hosts x {per_host} pages on [::1]:{port}, latency {latency:?}");
    axum::serve(listener, app).await.unwrap();
}
