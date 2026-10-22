//! 100,000 URLs Chaos & Adversarial Mock Origin Server for Phase 3.
//! Serves H virtual hosts (`s0.localhost` .. `s{H-1}.localhost`), each with P pages (H * P = 100,000).
//! Injects:
//! - Spider traps (circular calendar loops and parameter traps)
//! - Chaos network faults (503 Service Unavailable, 429 Rate Limits, Slowloris delays)
//! - Pathological DOM mutations (deeply-nested divs, malformed unclosed tags, inline noise)
//! - Atomic served counter & metrics at `/__served` and `/__chaos_metrics`

use std::fmt::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

struct ChaosSite {
    pages: Vec<(String, String)>,
    hosts: usize,
    per_host: usize,
    latency: Duration,
    served: AtomicUsize,
    traps_served: AtomicUsize,
    chaos_503: AtomicUsize,
    chaos_429: AtomicUsize,
    pathological_doms: AtomicUsize,
}

fn host_index(headers: &HeaderMap, hosts: usize) -> Option<usize> {
    let host = headers.get(header::HOST)?.to_str().ok()?;
    let name = host.split(':').next()?;
    let i: usize = name.strip_prefix('s')?.strip_suffix(".localhost")?.parse().ok()?;
    (i < hosts).then_some(i)
}

fn disable_hrefs(html: &str) -> String {
    let b = html.as_bytes();
    let mut out = String::with_capacity(html.len() + 1024);
    let mut last = 0;
    let mut i = 1;
    while i + 4 <= b.len() {
        if b[i..i + 4].eq_ignore_ascii_case(b"href")
            && (b[i - 1].is_ascii_whitespace() || matches!(b[i - 1], b'"' | b'\'' | b'/'))
        {
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

fn render_chaos(site: &ChaosSite, host: usize, page: usize) -> (StatusCode, String) {
    let seed = host * site.per_host + page;
    
    // 1. Chaos Network Fault: 8% chance of 503
    if seed % 100 < 8 {
        site.chaos_503.fetch_add(1, Ordering::Relaxed);
        return (StatusCode::SERVICE_UNAVAILABLE, "503 Service Temporarily Overloaded (Chaos)".into());
    }
    // 2. Chaos Network Fault: 5% chance of 429
    if seed % 100 >= 8 && seed % 100 < 13 {
        site.chaos_429.fetch_add(1, Ordering::Relaxed);
        return (StatusCode::TOO_MANY_REQUESTS, "429 Too Many Requests (Rate Limit Backpressure)".into());
    }

    let (head, tail) = &site.pages[seed % site.pages.len()];
    let mut out = String::with_capacity(head.len() + tail.len() + 4096);
    out.push_str(head);

    // 3. Pathological DOM Injection (Every 17th page): Deep nesting (150 levels) & unclosed tags
    if seed % 17 == 0 {
        site.pathological_doms.fetch_add(1, Ordering::Relaxed);
        out.push_str("<div class=\"pathological-nesting\">");
        for level in 0..150 {
            let _ = write!(out, "<div class=\"level-{level}\" data-depth=\"{level}\"><span>Nested text level {level}</span>");
        }
        out.push_str("<p>Unclosed container tags with inline noise</p>");
        // Intentionally omit some closing tags to test parser resilience
        for _ in 0..50 {
            out.push_str("</div>");
        }
    }

    // 4. Cross Links and Topology Graph: 3-ary tree
    out.push_str("<p class=\"crawl-bench-links\">");
    for k in [3 * page + 1, 3 * page + 2, 3 * page + 3] {
        if k < site.per_host {
            let _ = write!(out, "<a href=\"/p/{k}\">Read chapter {k} of node graph</a> ");
        }
    }

    // 5. Spider Trap Links (Circular calendar & endless path traps)
    if seed % 13 == 0 {
        site.traps_served.fetch_add(1, Ordering::Relaxed);
        let year = 2026 + (seed % 5);
        let month = 1 + (seed % 12);
        let _ = write!(out, "<a href=\"/archive/{year}/{month}/p/{page}\">Calendar Loop {year}-{month}</a> ");
        let _ = write!(out, "<a href=\"/p/{page}?trap_ref={seed}&param=circular\">Recursive Query Loop</a> ");
    }

    out.push_str("<a href=\"/\">Return to root index</a></p>");
    out.push_str(tail);
    (StatusCode::OK, out)
}

fn load_pages_parallel(files: &[std::path::PathBuf]) -> Vec<(String, String)> {
    let n_threads = std::thread::available_parallelism()
        .map_or(8, |n| n.get())
        .min(files.len().max(1));
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

async fn handle_page(site: &ChaosSite, headers: &HeaderMap, page: usize) -> Response {
    if site.latency > Duration::ZERO {
        tokio::time::sleep(site.latency).await;
    }
    let Some(h) = host_index(headers, site.hosts) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if page >= site.per_host {
        return StatusCode::NOT_FOUND.into_response();
    }
    site.served.fetch_add(1, Ordering::Relaxed);
    let (status, body) = render_chaos(site, h, page);
    (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
        .into_response()
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: chaos_origin <inputs-dir> <port> <hosts> <pages-per-host> [latency-ms]";
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

    let pages = load_pages_parallel(&files);
    let total_capacity = hosts * per_host;
    let site = Arc::new(ChaosSite {
        pages,
        hosts,
        per_host,
        latency,
        served: AtomicUsize::new(0),
        traps_served: AtomicUsize::new(0),
        chaos_503: AtomicUsize::new(0),
        chaos_429: AtomicUsize::new(0),
        pathological_doms: AtomicUsize::new(0),
    });

    let app = Router::new()
        .route("/robots.txt", get(|| async { "User-agent: *\nAllow: /\nDisallow: /archive/\n" }))
        .route("/", get(|State(s): State<Arc<ChaosSite>>, h: HeaderMap| async move { handle_page(&s, &h, 0).await }))
        .route("/p/{n}", get(|State(s): State<Arc<ChaosSite>>, h: HeaderMap, Path(n): Path<usize>| async move {
            handle_page(&s, &h, n).await
        }))
        // Spider trap circular endpoint
        .route("/archive/{year}/{month}/p/{n}", get(|State(s): State<Arc<ChaosSite>>, h: HeaderMap, Path((_, _, n)): Path<(usize, usize, usize)>| async move {
            handle_page(&s, &h, n).await
        }))
        .route("/__served", get(|State(s): State<Arc<ChaosSite>>| async move {
            s.served.load(Ordering::Relaxed).to_string()
        }))
        .route("/__chaos_metrics", get(|State(s): State<Arc<ChaosSite>>| async move {
            axum::Json(serde_json::json!({
                "served": s.served.load(Ordering::Relaxed),
                "traps_injected": s.traps_served.load(Ordering::Relaxed),
                "chaos_503": s.chaos_503.load(Ordering::Relaxed),
                "chaos_429": s.chaos_429.load(Ordering::Relaxed),
                "pathological_doms": s.pathological_doms.load(Ordering::Relaxed),
                "hosts": s.hosts,
                "per_host": s.per_host,
                "total_urls": s.hosts * s.per_host,
            }))
        }))
        .with_state(site);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    eprintln!("chaos_origin: {hosts} hosts x {per_host} pages ({total_capacity} URLs) on 127.0.0.1:{port}");
    axum::serve(listener, app).await.unwrap();
}
