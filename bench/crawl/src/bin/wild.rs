//! 模拟真实网络的多站点源站：用固定种子随机给每个站点加上真实网站常见的陷阱和毛病，
//! 由源站自己统计每个爬虫抓了多少独立内容页、浪费了多少请求、是否守规矩。
//!
//! 每站有 P 个内容页（正文取自评测集的真实 HTML，原有 href 失效），第 j 页链接到 3j+1…3j+3、
//! 一个随机内容页和首页，所以只顺着干净链接就能走到全部 P 页。另外按站点随机开启（每项约一半站点）：
//!
//! | 陷阱 | 页面上多出的链接 | 抓了算什么 |
//! |---|---|---|
//! | tracking | 内容页链接带 utm_*/fbclid 等跟踪参数 | 同一页的重复 |
//! | session | 链接里带每页不同的会话 ID（;jsessionid=、PHPSESSID=、sid=） | 重复 |
//! | print | `?print=1` 打印版 | 重复 |
//! | index | `/p/j/index.html` | 重复 |
//! | relative | 相对链接 `more/`，路径越叠越长（/p/j/more/more/…） | 陷阱 |
//! | calendar | `/events?month=…`，每页链接上一月和下一月，无穷无尽 | 第一页是导航，其余是陷阱 |
//! | facets | `/list?sort=…&view=…` 的各种组合，内容相同 | 第一页是导航，其余是重复 |
//! | archive | `/archive?page=N`，有限页之后是"没有更多"的空页，但仍链接下一页 | 有内容的是导航，空页是陷阱 |
//! | binary | `/media/j.jpg`、`/files/j.zip` | 白抓 |
//! | robots | robots.txt 禁止 `/private/`，页面链接 `/private/j` | 违规 |
//!
//! 站点行为也随机：响应延迟是基准的 0.5～4 倍；部分站点对首次请求随机返回 503（重试会成功）；
//! 部分站点限流，同一秒内请求超过 R 个就返回 429 + Retry-After。
//!
//! `wild <inputs-dir> <port> <hosts> <pages-per-host> <base-latency-ms> <seed>`
//! `GET /__served`：送出的 200 响应数；`GET /__report`：按站点汇总的 JSON。

use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn h(parts: &[u64]) -> u64 {
    parts.iter().fold(0x1234_5678, |acc, &p| mix(acc ^ p))
}

/// 0..1 的伪随机数。
fn unit(parts: &[u64]) -> f64 {
    (h(parts) >> 11) as f64 / (1u64 << 53) as f64
}

const TRAPS: [&str; 10] = [
    "tracking", "session", "print", "index", "relative", "calendar", "facets", "archive", "binary",
    "robots",
];
const ARCHIVE_PER_PAGE: usize = 20;

struct Profile {
    traps: HashSet<&'static str>,
    latency: Duration,
    /// 首次请求返回 503 的概率
    flaky: f64,
    /// 每秒最多几个请求，超过返回 429；0 表示不限
    max_rps: usize,
}

#[derive(Default)]
struct Stats {
    requests: usize,
    /// 按类别计数：content、dup、trap、nav、binary、robots、503、429、404
    by_kind: HashMap<&'static str, usize>,
    unique_content: HashSet<usize>,
    nav_seen: HashSet<String>,
    attempts: HashMap<String, u32>,
    inflight: usize,
    peak_inflight: usize,
    /// 最近一秒内的请求开始时间（限流用）
    window: Vec<Instant>,
    starts: Vec<Instant>,
    first: Option<Instant>,
    last: Option<Instant>,
    /// 每个请求的 URI 和类别（调试用，/__log）
    log: Vec<(String, &'static str)>,
}

struct Site {
    pages: Vec<(String, String)>,
    hosts: usize,
    per_host: usize,
    profiles: Vec<Profile>,
    stats: Vec<Mutex<Stats>>,
    served: AtomicUsize,
    seed: u64,
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
    let body = disable_hrefs(html);
    match body.rfind("</body>") {
        Some(i) => (body[..i].to_string(), body[i..].to_string()),
        None => (body, String::new()),
    }
}

impl Site {
    fn has(&self, host: usize, trap: &str) -> bool {
        self.profiles[host].traps.contains(trap)
    }

    /// 指向内容页 k 的链接，按站点的陷阱加上跟踪参数或会话 ID。`from` 让同一目标在不同页面上的写法不同。
    fn content_href(&self, host: usize, from: usize, k: usize) -> String {
        let mut path = if k == 0 { "/".to_string() } else { format!("/p/{k}/") };
        let mut query: Vec<String> = Vec::new();
        let r = h(&[self.seed, host as u64, from as u64, k as u64]);
        if self.has(host, "session") {
            let sid = format!("{:016x}{:016x}", mix(r), mix(r ^ 1));
            match host % 3 {
                0 => path.push_str(&format!(";jsessionid={}", &sid[..24])),
                1 => query.push(format!("PHPSESSID={}", &sid[..26])),
                _ => query.push(format!("sid={sid}")),
            }
        }
        if self.has(host, "tracking") {
            query.push(format!("utm_source=site&utm_medium=related&utm_campaign=p{from}"));
            if r % 2 == 0 {
                query.push(format!("fbclid=IwAR{:x}", mix(r ^ 7)));
            }
        }
        if query.is_empty() {
            path
        } else {
            format!("{path}?{}", query.join("&"))
        }
    }

    fn content(&self, host: usize, page: usize) -> String {
        let (head, tail) = &self.pages[(host * self.per_host + page) % self.pages.len()];
        let mut out = String::with_capacity(head.len() + tail.len() + 2000);
        out.push_str(head);
        out.push_str("<div class=\"crawl-bench-links\"><ul>");
        let mut targets: Vec<usize> = [3 * page + 1, 3 * page + 2, 3 * page + 3]
            .into_iter()
            .filter(|&k| k < self.per_host)
            .collect();
        targets.push((h(&[self.seed, host as u64, page as u64, 99]) % self.per_host as u64) as usize);
        for k in targets {
            let _ = write!(
                out,
                "<li><a href=\"{}\">Read part {k} of this series in full</a></li>",
                self.content_href(host, page, k)
            );
        }
        out.push_str("</ul><p>");
        let _ = write!(out, "<a href=\"{}\">Home</a> ", self.content_href(host, page, 0));
        if self.has(host, "print") {
            let _ = write!(out, "<a href=\"?print=1\">Print this page</a> ");
        }
        if self.has(host, "index") && page > 0 {
            let _ = write!(out, "<a href=\"/p/{page}/index.html\">Permalink</a> ");
        }
        if self.has(host, "relative") && page > 0 {
            out.push_str("<a href=\"more/\">More on this topic</a> ");
        }
        if self.has(host, "calendar") {
            out.push_str("<a href=\"/events?month=2026-10\">Events calendar</a> ");
        }
        if self.has(host, "facets") {
            let sorts = ["new", "old", "popular", "az"];
            let views = ["grid", "list", "compact"];
            let r = h(&[self.seed, host as u64, page as u64, 5]) as usize;
            let _ = write!(
                out,
                "<a href=\"/list?sort={}&view={}\">Browse all</a> ",
                sorts[r % 4],
                views[(r / 4) % 3]
            );
        }
        if self.has(host, "archive") {
            out.push_str("<a href=\"/archive?page=1\">Archive</a> ");
        }
        if self.has(host, "binary") {
            let _ = write!(
                out,
                "<a href=\"/media/{page}.jpg\">Photo</a> <a href=\"/files/{page}.zip\">Download</a> "
            );
        }
        if self.has(host, "robots") {
            let _ = write!(out, "<a href=\"/private/{page}\">Members area</a> ");
        }
        out.push_str("</p></div>");
        out.push_str(tail);
        out
    }

    fn listing(&self, host: usize, title: &str, items: &[usize], extra: &str) -> String {
        let mut s = format!("<!doctype html><html><head><title>{title}</title></head><body><h1>{title}</h1><ul>");
        for &k in items {
            let _ = write!(s, "<li><a href=\"{}\">Story number {k}</a></li>", self.content_href(host, 0, k));
        }
        s.push_str("</ul>");
        s.push_str(extra);
        s.push_str("</body></html>");
        s
    }
}

fn query_map(q: Option<&str>) -> Vec<(String, String)> {
    q.unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (p.to_string(), String::new()),
        })
        .collect()
}

fn month_shift(m: &str, d: i32) -> String {
    let (y, mm) = m.split_once('-').unwrap_or(("2026", "10"));
    let total = y.parse::<i32>().unwrap_or(2026) * 12 + mm.parse::<i32>().unwrap_or(10) - 1 + d;
    format!("{}-{:02}", total.div_euclid(12), total.rem_euclid(12) + 1)
}

enum Reply {
    Html(&'static str, String),
    Bytes(&'static str, Vec<u8>),
    Status(&'static str, StatusCode),
}

/// 按路径决定响应和类别。`key` 是内容页编号（内容页和它的重复写法）或导航页的去重键。
fn route(site: &Site, host: usize, uri: &Uri) -> Reply {
    // 路径里的会话 ID（;jsessionid=…）不影响是哪一页
    let raw = uri.path();
    let session_in_path = raw.contains(";jsessionid=");
    let stripped = match raw.find(";jsessionid=") {
        Some(i) => {
            let end = raw[i..].find('/').map_or(raw.len(), |j| i + j);
            format!("{}{}", &raw[..i], &raw[end..])
        }
        None => raw.to_string(),
    };
    let path = stripped.as_str();
    let q = query_map(uri.query());
    let content_page = |p: usize, decorated: bool| -> Reply {
        let kind = if decorated { "dup" } else { "content" };
        Reply::Html(kind, site.content(host, p))
    };
    // 内容页：/ 和 /p/{j}/，以及它们的各种重复写法
    let (page, rest) = if path == "/" {
        (Some(0), "")
    } else if let Some(r) = path.strip_prefix("/p/") {
        let (num, rest) = r.split_once('/').unwrap_or((r, ""));
        let num = num.split(';').next().unwrap_or(num);
        (num.parse::<usize>().ok().filter(|&n| n < site.per_host), rest)
    } else {
        (None, "")
    };
    if let Some(p) = page {
        if !rest.is_empty() {
            if rest.trim_end_matches('/').split('/').all(|s| s == "more") && site.has(host, "relative") {
                return Reply::Html("trap", site.content(host, p));
            }
            if rest == "index.html" && site.has(host, "index") {
                return content_page(p, true);
            }
            return Reply::Status("404", StatusCode::NOT_FOUND);
        }
        return content_page(p, session_in_path || !q.is_empty());
    }
    match path {
        "/events" if site.has(host, "calendar") => {
            let m = q.iter().find(|(k, _)| k == "month").map_or("2026-10".to_string(), |(_, v)| v.clone());
            let kind = if m == "2026-10" { "nav" } else { "trap" };
            let extra = format!(
                "<p><a href=\"/events?month={}\">Previous month</a> <a href=\"/events?month={}\">Next month</a></p>",
                month_shift(&m, -1),
                month_shift(&m, 1)
            );
            Reply::Html(kind, site.listing(host, &format!("Events in {m}"), &[], &format!("<p>No events scheduled.</p>{extra}")))
        }
        "/list" if site.has(host, "facets") => {
            let sorts = ["new", "old", "popular", "az"];
            let views = ["grid", "list", "compact"];
            let mut extra = String::from("<p>");
            for s in sorts {
                for v in views {
                    let _ = write!(extra, "<a href=\"/list?sort={s}&view={v}\">{s}/{v}</a> ");
                }
            }
            extra.push_str("</p>");
            let items: Vec<usize> = (0..site.per_host.min(ARCHIVE_PER_PAGE)).collect();
            Reply::Html("nav:list", site.listing(host, "All stories", &items, &extra))
        }
        "/archive" if site.has(host, "archive") => {
            let n: usize = q.iter().find(|(k, _)| k == "page").and_then(|(_, v)| v.parse().ok()).unwrap_or(1).max(1);
            let start = (n - 1) * ARCHIVE_PER_PAGE;
            let items: Vec<usize> = (start..(start + ARCHIVE_PER_PAGE).min(site.per_host)).collect();
            let kind = if items.is_empty() { "trap" } else { "nav" };
            let extra = format!(
                "{}<p><a href=\"/archive?page={}\">Older posts</a></p>",
                if items.is_empty() { "<p>No more posts.</p>" } else { "" },
                n + 1
            );
            Reply::Html(kind, site.listing(host, &format!("Archive page {n}"), &items, &extra))
        }
        _ if path.starts_with("/media/") && site.has(host, "binary") => {
            Reply::Bytes("binary", vec![0xff; 64 * 1024])
        }
        _ if path.starts_with("/files/") && site.has(host, "binary") => {
            Reply::Bytes("binary", vec![0x50; 64 * 1024])
        }
        _ if path.starts_with("/private/") && site.has(host, "robots") => {
            Reply::Html("robots", site.listing(host, "Members", &[], "<p>Members only.</p>"))
        }
        _ => Reply::Status("404", StatusCode::NOT_FOUND),
    }
}

async fn handle(State(site): State<Arc<Site>>, headers: HeaderMap, uri: Uri) -> Response {
    let Some(host) = host_index(&headers, site.hosts) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let prof = &site.profiles[host];
    let full = uri.to_string();
    // 先记请求开始、限流和首次失败，再等延迟
    let early: Option<(&'static str, StatusCode)> = {
        let mut st = site.stats[host].lock().unwrap();
        let now = Instant::now();
        st.requests += 1;
        st.first.get_or_insert(now);
        st.last = Some(now);
        st.starts.push(now);
        st.inflight += 1;
        st.peak_inflight = st.peak_inflight.max(st.inflight);
        st.window.retain(|t| now.duration_since(*t) < Duration::from_secs(1));
        st.window.push(now);
        let attempt = {
            let a = st.attempts.entry(full.clone()).or_insert(0);
            *a += 1;
            *a
        };
        if prof.max_rps > 0 && st.window.len() > prof.max_rps {
            Some(("429", StatusCode::TOO_MANY_REQUESTS))
        } else if attempt == 1 && unit(&[site.seed, host as u64, mix(full.len() as u64) ^ h(&full.bytes().map(u64::from).collect::<Vec<_>>()), 3]) < prof.flaky {
            Some(("503", StatusCode::SERVICE_UNAVAILABLE))
        } else {
            None
        }
    };
    tokio::time::sleep(prof.latency).await;
    let finish = |kind: &'static str| {
        let mut st = site.stats[host].lock().unwrap();
        st.inflight -= 1;
        *st.by_kind.entry(kind).or_default() += 1;
        st.log.push((full.clone(), kind));
    };
    if let Some((kind, code)) = early {
        finish(kind);
        let mut resp = code.into_response();
        if code == StatusCode::TOO_MANY_REQUESTS {
            resp.headers_mut().insert(header::RETRY_AFTER, "1".parse().unwrap());
        }
        return resp;
    }
    let reply = route(&site, host, &uri);
    match reply {
        Reply::Status(kind, code) => {
            finish(kind);
            code.into_response()
        }
        Reply::Bytes(kind, body) => {
            finish(kind);
            site.served.fetch_add(1, Ordering::Relaxed);
            let ct = if uri.path().ends_with(".jpg") { "image/jpeg" } else { "application/zip" };
            ([(header::CONTENT_TYPE, ct)], body).into_response()
        }
        Reply::Html(kind, body) => {
            site.served.fetch_add(1, Ordering::Relaxed);
            let kind = {
                let mut st = site.stats[host].lock().unwrap();
                st.inflight -= 1;
                let k: &'static str = match kind {
                    "content" | "dup" => {
                        let p = if uri.path() == "/" || uri.path().starts_with("/;") || !uri.path().starts_with("/p/") {
                            0
                        } else {
                            uri.path()[3..].split(['/', ';']).next().and_then(|n| n.parse().ok()).unwrap_or(0)
                        };
                        if st.unique_content.insert(p) { "content" } else { "dup" }
                    }
                    "nav:list" => {
                        if st.nav_seen.insert("list".into()) { "nav" } else { "dup" }
                    }
                    "nav" => {
                        if st.nav_seen.insert(full.clone()) { "nav" } else { "dup" }
                    }
                    other => other,
                };
                *st.by_kind.entry(k).or_default() += 1;
                st.log.push((full.clone(), k));
                k
            };
            let _ = kind;
            ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response()
        }
    }
}

async fn robots(State(site): State<Arc<Site>>, headers: HeaderMap) -> String {
    match host_index(&headers, site.hosts) {
        Some(i) if site.has(i, "robots") => "User-agent: *\nDisallow: /private/\n".into(),
        _ => "User-agent: *\nAllow: /\n".into(),
    }
}

fn report(site: &Site) -> String {
    let mut out = String::from("[");
    for (i, (prof, st)) in site.profiles.iter().zip(&site.stats).enumerate() {
        let st = st.lock().unwrap();
        let mut traps: Vec<_> = prof.traps.iter().copied().collect();
        traps.sort();
        let mut kinds: Vec<_> = st.by_kind.iter().collect();
        kinds.sort();
        let kinds: Vec<String> = kinds.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
        // 相邻请求开始时间的最小间隔（毫秒）
        let mut starts = st.starts.clone();
        starts.sort();
        let min_gap = starts.windows(2).map(|w| w[1].duration_since(w[0]).as_secs_f64() * 1000.0).fold(f64::INFINITY, f64::min);
        let span = match (st.first, st.last) {
            (Some(a), Some(b)) => b.duration_since(a).as_secs_f64(),
            _ => 0.0,
        };
        let _ = write!(
            out,
            "{}{{\"host\":{i},\"traps\":{:?},\"latency_ms\":{},\"flaky\":{},\"max_rps\":{},\"requests\":{},\"unique_content\":{},\"kinds\":{{{}}},\"peak_inflight\":{},\"min_gap_ms\":{},\"span_s\":{span:.3}}}",
            if i > 0 { "," } else { "" },
            traps,
            prof.latency.as_millis(),
            prof.flaky,
            prof.max_rps,
            st.requests,
            st.unique_content.len(),
            kinds.join(","),
            st.peak_inflight,
            if min_gap.is_finite() { format!("{min_gap:.1}") } else { "null".into() },
        );
    }
    out.push(']');
    out
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: wild <inputs-dir> <port> <hosts> <pages-per-host> <base-latency-ms> <seed>";
    let dir = args.next().expect(usage);
    let port: u16 = args.next().expect(usage).parse().unwrap();
    let hosts: usize = args.next().expect(usage).parse().unwrap();
    let per_host: usize = args.next().expect(usage).parse().unwrap();
    let base: u64 = args.next().expect(usage).parse().unwrap();
    let seed: u64 = args.next().expect(usage).parse().unwrap();
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "html"))
        .collect();
    files.sort();
    let pages = files
        .iter()
        .map(|p| prepare(&String::from_utf8_lossy(&std::fs::read(p).unwrap())))
        .collect();
    let profiles: Vec<Profile> = (0..hosts as u64)
        .map(|i| {
            let traps = TRAPS
                .iter()
                .enumerate()
                .filter(|(t, _)| unit(&[seed, i, *t as u64, 1]) < 0.5)
                .map(|(_, n)| *n)
                .collect();
            let factor = 0.5 + 3.5 * unit(&[seed, i, 2]).powi(2);
            let flaky = [0.0, 0.0, 0.05, 0.15][(h(&[seed, i, 4]) % 4) as usize];
            let max_rps = [0, 0, 0, 5][(h(&[seed, i, 6]) % 4) as usize];
            Profile {
                traps,
                latency: Duration::from_micros((base as f64 * 1000.0 * factor) as u64),
                flaky,
                max_rps,
            }
        })
        .collect();
    let site = Arc::new(Site {
        pages,
        hosts,
        per_host,
        stats: (0..hosts).map(|_| Mutex::new(Stats::default())).collect(),
        profiles,
        served: AtomicUsize::new(0),
        seed,
    });
    let app = Router::new()
        .route("/robots.txt", get(robots))
        .route(
            "/__served",
            get(|State(s): State<Arc<Site>>| async move { s.served.load(Ordering::Relaxed).to_string() }),
        )
        .route("/__report", get(|State(s): State<Arc<Site>>| async move { report(&s) }))
        .route(
            "/__log",
            get(|State(s): State<Arc<Site>>| async move {
                let mut out = String::new();
                for (i, st) in s.stats.iter().enumerate() {
                    for (u, k) in &st.lock().unwrap().log {
                        let _ = writeln!(out, "{i}\t{k}\t{u}");
                    }
                }
                out
            }),
        )
        .fallback(handle)
        .with_state(site);
    let listener = tokio::net::TcpListener::bind(("::1", port)).await.unwrap();
    eprintln!("wild: {hosts} hosts x {per_host} pages on [::1]:{port}, base latency {base} ms, seed {seed}");
    axum::serve(listener, app).await.unwrap();
}
