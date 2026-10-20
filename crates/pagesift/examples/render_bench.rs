//! 浏览器渲染压测：本地源站提供评测集里的真实页面和几种合成的 SPA 页，完整的 Scraper
//! 用 `engine: browser` 并发渲染。报告每页延迟、吞吐、内存（本进程和 Chrome 进程树）和分阶段耗时，
//! 每页的 Markdown 写到输出目录，用来比较优化前后的正文。
//!
//! `cargo run --release -p pagesift --example render_bench -- --inputs bench/data/inputs --out /tmp/md`
//!
//! 参数：`--inputs <dir>`、`--real <真实页面数，默认 120>`、`--synthetic <每种合成页份数，默认 10>`、
//! `--concurrency <并发，默认等于池容量>`、`--browsers <n>`、`--tabs <n>`、`--out <dir>`、`--label <名字>`、
//! `--only <逗号分隔的页面名>`（只跑这些页，排查用）。
//! 源站之外的主机都被 Chrome 的 `--host-resolver-rules` 解析失败，结果不受外网影响，也不会访问外网。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use futures_util::StreamExt;
use pagesift::api::types::ScrapeOptions;
use pagesift::{Scraper, ScraperConfig};

/// 1×1 透明 PNG。
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

fn paragraphs(prefix: &str, n: usize) -> String {
    (0..n)
        .map(|i| {
            format!(
                "<p>{prefix} paragraph {i}: the quick brown fox jumps over the lazy dog, \
                 and the article keeps going with enough words to count as real content.</p>"
            )
        })
        .collect()
}

/// 合成页。`kind` 决定页面结构，内容都要靠 JS 或子资源才能完整出现。
fn synthetic(kind: &str, n: usize) -> String {
    match kind {
        // 脚本延迟加载，再请求接口渲染正文。
        "spa" => format!(
            r#"<!doctype html><title>SPA {n}</title><div id="app">loading</div>
<script src="/static/app.js?d=60"></script>"#
        ),
        // 懒加载图片：40 张 loading=lazy 和 data-src 图片，图片源站延迟 200ms。
        "lazy" => {
            let imgs: String = (0..40)
                .map(|i| {
                    format!(
                        r#"<figure><img loading="lazy" src="/img/{i}.png?d=200" alt="figure {i}"><img class="lz" data-src="/img/b{i}.png?d=200" src="data:image/gif;base64,R0lGODlhAQABAAAAACw=" alt="lazy {i}"><figcaption>caption {i}</figcaption></figure>"#
                    )
                })
                .collect();
            format!(
                r#"<!doctype html><title>Lazy {n}</title><link rel="stylesheet" href="/static/site.css?d=80"><article><h1>Gallery {n}</h1>{}{imgs}</article>
<script>const io = new IntersectionObserver((es, o) => es.forEach(e => {{ if (e.isIntersecting) {{ e.target.src = e.target.dataset.src; o.unobserve(e.target); }} }})); document.querySelectorAll('img.lz').forEach(i => io.observe(i));</script>"#,
                paragraphs("gallery", 12)
            )
        }
        // 正文在 iframe 里，iframe 里的正文又是脚本渲染的。
        "iframe" => format!(
            r#"<!doctype html><title>Docs {n}</title><nav><a href="/">home</a> <a href="/docs">docs</a></nav>
<iframe src="/frame/{n}" width="800" height="600"></iframe>"#
        ),
        // 正文是静态的，但页面一直轮询接口：网络永远等不到静默。
        "poll" => format!(
            r#"<!doctype html><title>Live {n}</title><article><h1>Live blog {n}</h1>{}</article>
<script>setInterval(() => fetch('/api/poll?d=30').catch(() => {{}}), 60);</script>"#,
            paragraphs("live", 15)
        ),
        _ => unreachable!(),
    }
}

const KINDS: &[&str] = &["spa", "lazy", "iframe", "poll"];

async fn delay(q: &BTreeMap<String, String>) {
    if let Some(ms) = q.get("d").and_then(|d| d.parse().ok()) {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
}

fn origin(pages: Arc<Vec<String>>) -> Router {
    Router::new()
        .route("/robots.txt", get(|| async { "User-agent: *\nAllow: /\n" }))
        .route(
            "/p/{i}",
            get(
                |State(p): State<Arc<Vec<String>>>, Path(i): Path<usize>| async move {
                    Html(p[i % p.len()].clone())
                },
            ),
        )
        .route(
            "/s/{kind}/{n}",
            get(|Path((kind, n)): Path<(String, usize)>| async move {
                Html(synthetic(&kind, n))
            }),
        )
        .route(
            "/frame/{n}",
            get(|Path(n): Path<usize>| async move {
                Html(format!(
                    r#"<!doctype html><div id="app">loading</div><script src="/static/app.js?d=40&n={n}"></script>"#
                ))
            }),
        )
        .route(
            "/static/app.js",
            get(|Query(q): Query<BTreeMap<String, String>>| async move {
                delay(&q).await;
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    "fetch('/api/article?d=150').then(r => r.json()).then(a => { \
                     const app = document.getElementById('app'); \
                     app.innerHTML = '<h1>' + a.title + '</h1>' + a.body.map(p => '<p>' + p + '</p>').join(''); });",
                )
            }),
        )
        .route(
            "/static/site.css",
            get(|Query(q): Query<BTreeMap<String, String>>| async move {
                delay(&q).await;
                (
                    [(header::CONTENT_TYPE, "text/css")],
                    "body{font-family:Bench,serif}@font-face{font-family:Bench;src:url(/font.woff2?d=300)}",
                )
            }),
        )
        .route(
            "/api/article",
            get(|Query(q): Query<BTreeMap<String, String>>| async move {
                delay(&q).await;
                let body: Vec<String> = (0..20)
                    .map(|i| {
                        format!(
                            "rendered paragraph {i}: content that only exists after the script \
                             fetched it from the API, long enough to be the main text."
                        )
                    })
                    .collect();
                axum::Json(serde_json::json!({ "title": "Rendered article", "body": body }))
            }),
        )
        .route(
            "/api/poll",
            get(|Query(q): Query<BTreeMap<String, String>>| async move {
                delay(&q).await;
                "{}"
            }),
        )
        .route(
            "/img/{name}",
            get(|Query(q): Query<BTreeMap<String, String>>| async move {
                delay(&q).await;
                ([(header::CONTENT_TYPE, "image/png")], PNG).into_response()
            }),
        )
        .route(
            "/font.woff2",
            get(|Query(q): Query<BTreeMap<String, String>>| async move {
                delay(&q).await;
                ([(header::CONTENT_TYPE, "font/woff2")], vec![0u8; 20_000]).into_response()
            }),
        )
        .with_state(pages)
}

#[derive(Default, Clone, Copy)]
struct Mem {
    self_rss: u64,
    chrome_rss: u64,
    chrome_pss: u64,
    chrome_procs: usize,
}

fn status_kb(pid: &str, key: &str) -> Option<u64> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    s.lines()
        .find_map(|l| l.strip_prefix(key))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
}

fn pss_kb(pid: &str) -> Option<u64> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok()?;
    s.lines()
        .find_map(|l| l.strip_prefix("Pss:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
}

/// 本进程的 RSS，以及所有子孙进程（Chrome）的 RSS 和 PSS 之和，单位 KB。
fn sample_mem() -> Mem {
    let me = std::process::id().to_string();
    let mut parent: BTreeMap<String, String> = BTreeMap::new();
    if let Ok(dir) = std::fs::read_dir("/proc") {
        for e in dir.flatten() {
            let pid = e.file_name().to_string_lossy().into_owned();
            if !pid.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
                && let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r)
                && let Some(ppid) = rest.split_whitespace().nth(1)
            {
                parent.insert(pid, ppid.to_owned());
            }
        }
    }
    let mut m = Mem {
        self_rss: status_kb(&me, "VmRSS:").unwrap_or(0),
        ..Mem::default()
    };
    for pid in parent.keys() {
        let mut p = parent.get(pid);
        let mut descendant = false;
        for _ in 0..64 {
            match p {
                Some(pp) if *pp == me => {
                    descendant = true;
                    break;
                }
                Some(pp) => p = parent.get(pp),
                None => break,
            }
        }
        if descendant {
            m.chrome_procs += 1;
            m.chrome_rss += status_kb(pid, "VmRSS:").unwrap_or(0);
            m.chrome_pss += pss_kb(pid).unwrap_or(0);
        }
    }
    m
}

fn pct(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * p).round() as usize]
}

struct Outcome {
    name: String,
    kind: &'static str,
    ms: f64,
    stages: BTreeMap<String, f64>,
    markdown: Option<String>,
}

fn main() {
    let mut inputs = "bench/data/inputs".to_owned();
    let mut real = 120usize;
    let mut synth = 10usize;
    let mut concurrency = 0usize;
    let mut out: Option<String> = None;
    let mut label = "run".to_owned();
    let mut only: Option<String> = None;
    let mut browser = pagesift_cdp::PoolConfig::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut v = || args.next().expect("missing value");
        match a.as_str() {
            "--inputs" => inputs = v(),
            "--real" => real = v().parse().unwrap(),
            "--synthetic" => synth = v().parse().unwrap(),
            "--concurrency" => concurrency = v().parse().unwrap(),
            "--browsers" => browser.max_browsers = v().parse().unwrap(),
            "--tabs" => browser.max_tabs_per_browser = v().parse().unwrap(),
            "--out" => out = Some(v()),
            "--label" => label = v(),
            "--only" => only = Some(v()),
            other => panic!("unknown argument {other}"),
        }
    }
    if concurrency == 0 {
        concurrency = browser.max_browsers * browser.max_tabs_per_browser;
    }
    let pool = format!("{}x{}", browser.max_browsers, browser.max_tabs_per_browser);

    // 真实页面按 id 排序后等间隔抽样，每次运行抽到的都一样。
    let mut files: Vec<(u64, std::path::PathBuf)> = std::fs::read_dir(&inputs)
        .expect("inputs dir")
        .filter_map(|e| {
            let p = e.ok()?.path();
            let id = p.file_stem()?.to_str()?.parse().ok()?;
            (p.extension()? == "html").then_some((id, p))
        })
        .collect();
    files.sort();
    let step = (files.len() / real.max(1)).max(1);
    let picked: Vec<(u64, std::path::PathBuf)> =
        files.into_iter().step_by(step).take(real).collect();
    let pages: Arc<Vec<String>> = Arc::new(
        picked
            .iter()
            .map(|(_, p)| String::from_utf8_lossy(&std::fs::read(p).unwrap()).into_owned())
            .collect(),
    );

    let origin_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let base = origin_rt.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = origin(pages.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    });

    let mut jobs: Vec<(String, &'static str, String)> = picked
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (format!("real-{id}"), "real", format!("{base}/p/{i}")))
        .collect();
    for n in 0..synth {
        for kind in KINDS {
            jobs.push((format!("{kind}-{n}"), kind, format!("{base}/s/{kind}/{n}")));
        }
    }
    if let Some(only) = &only {
        jobs.retain(|(name, ..)| only.split(',').any(|o| name == o));
    }
    // 真实页和合成页交错，避免某一段时间全是同一种页面。
    jobs.sort_by_key(|(name, ..)| mix_order(name));

    let sampling = Arc::new(AtomicBool::new(true));
    let peak = Arc::new(std::sync::Mutex::new(Mem::default()));
    let sampler = {
        let (sampling, peak) = (sampling.clone(), peak.clone());
        std::thread::spawn(move || {
            while sampling.load(Ordering::Relaxed) {
                let m = sample_mem();
                let mut p = peak.lock().unwrap();
                p.self_rss = p.self_rss.max(m.self_rss);
                p.chrome_rss = p.chrome_rss.max(m.chrome_rss);
                p.chrome_pss = p.chrome_pss.max(m.chrome_pss);
                p.chrome_procs = p.chrome_procs.max(m.chrome_procs);
                drop(p);
                std::thread::sleep(Duration::from_millis(200));
            }
        })
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (cold_ms, wall, results) = rt.block_on(async {
        let mut cfg = ScraperConfig::default();
        cfg.fetcher.ssrf.allow_private_network = true;
        cfg.policy.rate.default_interval = Duration::ZERO;
        cfg.policy.rate.per_host_concurrency = concurrency;
        cfg.policy.rate.global_concurrency = concurrency;
        browser
            .extra_args
            .push("--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1".into());
        cfg.browser = browser;
        let scraper = Scraper::new(cfg).unwrap();
        let opts: ScrapeOptions = serde_json::from_value(
            serde_json::json!({ "engine": "browser", "formats": ["markdown"] }),
        )
        .unwrap();

        // 冷启动：第一页要启动浏览器。
        let t = Instant::now();
        scraper
            .scrape(&format!("{base}/s/spa/9999"), &opts)
            .await
            .expect("warm-up render");
        let cold_ms = t.elapsed().as_secs_f64() * 1000.0;

        let t = Instant::now();
        let results: Vec<Outcome> = futures_util::stream::iter(jobs)
            .map(|(name, kind, url)| {
                let (scraper, opts) = (&scraper, &opts);
                async move {
                    let t = Instant::now();
                    let r = scraper.scrape(&url, opts).await;
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    match r {
                        Ok(d) => Outcome {
                            name,
                            kind,
                            ms,
                            stages: d.metadata.pagesift.map(|p| p.timings).unwrap_or_default(),
                            markdown: d.markdown,
                        },
                        Err(e) => {
                            eprintln!("{name}: {} {}", e.code.as_str(), e.message);
                            Outcome {
                                name,
                                kind,
                                ms,
                                stages: BTreeMap::new(),
                                markdown: None,
                            }
                        }
                    }
                }
            })
            .buffer_unordered(concurrency)
            .collect()
            .await;
        let wall = t.elapsed();
        scraper.shutdown().await;
        (cold_ms, wall, results)
    });
    sampling.store(false, Ordering::Relaxed);
    sampler.join().unwrap();
    drop(rt);
    let peak = *peak.lock().unwrap();
    let leftover = sample_mem().chrome_procs;

    let ok: Vec<&Outcome> = results.iter().filter(|r| r.markdown.is_some()).collect();
    let errors = results.len() - ok.len();
    if let Some(dir) = &out {
        std::fs::create_dir_all(dir).unwrap();
        for r in &ok {
            std::fs::write(
                format!("{dir}/{}.md", r.name),
                r.markdown.as_deref().unwrap_or_default(),
            )
            .unwrap();
        }
    }

    println!(
        "## {label}: {} pages ({} real + {} synthetic), concurrency {concurrency}, pool {pool}",
        results.len(),
        picked.len(),
        synth * KINDS.len(),
    );
    println!(
        "cold start {cold_ms:.0} ms; wall {:.2} s; throughput {:.2} pages/s; errors {errors}; \
         leftover chrome processes {leftover}",
        wall.as_secs_f64(),
        results.len() as f64 / wall.as_secs_f64()
    );
    println!(
        "peak memory: pagesift RSS {:.0} MB, chrome RSS sum {:.0} MB, chrome PSS sum {:.0} MB, \
         chrome processes {}",
        peak.self_rss as f64 / 1024.0,
        peak.chrome_rss as f64 / 1024.0,
        peak.chrome_pss as f64 / 1024.0,
        peak.chrome_procs
    );
    println!();
    println!("| pages | n | p50 ms | p95 ms | mean ms |");
    println!("|---|---|---|---|---|");
    let mut groups: Vec<(&str, Vec<f64>)> = vec![("all", ok.iter().map(|r| r.ms).collect())];
    for kind in std::iter::once(&"real").chain(KINDS) {
        groups.push((
            kind,
            ok.iter()
                .filter(|r| r.kind == *kind)
                .map(|r| r.ms)
                .collect(),
        ));
    }
    for (g, v) in &groups {
        let mean = v.iter().sum::<f64>() / v.len().max(1) as f64;
        println!(
            "| {g} | {} | {:.0} | {:.0} | {mean:.0} |",
            v.len(),
            pct(v, 0.5),
            pct(v, 0.95)
        );
    }
    println!();
    let stages = [
        "render_acquire",
        "render_setup",
        "render_navigation",
        "render_wait",
        "render_html",
        "render_close",
        "render",
        "extract",
        "total",
    ];
    println!("| stage | p50 ms | p95 ms | mean ms |");
    println!("|---|---|---|---|");
    for s in stages {
        let v: Vec<f64> = ok.iter().filter_map(|r| r.stages.get(s).copied()).collect();
        let mean = v.iter().sum::<f64>() / v.len().max(1) as f64;
        println!(
            "| {s} | {:.1} | {:.1} | {mean:.1} |",
            pct(&v, 0.5),
            pct(&v, 0.95)
        );
    }
    let md_bytes: usize = ok
        .iter()
        .map(|r| r.markdown.as_ref().map_or(0, String::len))
        .sum();
    println!();
    println!("markdown total {:.1} KB", md_bytes as f64 / 1024.0);
}

/// 固定的伪随机顺序（FNV-1a），不依赖随机数。
fn mix_order(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    })
}
