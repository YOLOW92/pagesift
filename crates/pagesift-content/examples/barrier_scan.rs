//! 在评测快照上跑拦截页识别：列出命中的快照，并测识别本身的额外耗时。
//! `cargo run --release -p pagesift-content --example barrier_scan -- <数据目录>`

use std::time::Instant;

use pagesift_content::{Options, process};
use url::Url;

fn best_ms(html: &str, opts: &Options) -> f64 {
    (0..5)
        .map(|_| {
            let t = Instant::now();
            std::hint::black_box(process(html, opts));
            t.elapsed().as_secs_f64() * 1000.0
        })
        .fold(f64::MAX, f64::min)
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: barrier_scan <data dir>");
    let (mut pages, mut hits) = (0, 0);
    let mut extra = Vec::new();
    // 原始抓取的快照，以及浏览器渲染的快照（含 4xx 页面）
    let mut items = Vec::new();
    for (index, suffix) in [("meta.jsonl", "html"), ("rendered.jsonl", "rendered.html")] {
        let text = std::fs::read_to_string(format!("{dir}/snapshots/{index}")).unwrap_or_default();
        items.extend(text.lines().map(|l| (l.to_string(), suffix)));
    }
    for (line, suffix) in &items {
        let Ok(m) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let id = m["id"].as_str().unwrap_or_default();
        let Ok(bytes) = std::fs::read(format!("{dir}/snapshots/{id}.{suffix}")) else {
            continue;
        };
        if bytes.starts_with(b"%PDF") {
            continue;
        }
        let html = String::from_utf8_lossy(&bytes);
        let status = m["status"].as_u64().map(|s| s as u16);
        let opts = Options {
            base_url: m["final_url"].as_str().and_then(|u| Url::parse(u).ok()),
            status,
            ..Default::default()
        };
        pages += 1;
        let doc = process(&html, &opts);
        if let Some(b) = &doc.barrier {
            hits += 1;
            println!(
                "HIT {suffix} id={id} status={status:?} vendor={} kind={} conf={:.2} rule={} title={:?} url={}",
                b.vendor,
                b.kind.as_str(),
                b.confidence,
                b.rule_id,
                doc.metadata.title.as_deref().unwrap_or(""),
                m["url"].as_str().unwrap_or_default()
            );
        } else if status.is_some_and(|s| s >= 400) {
            println!(
                "MISS {suffix} id={id} status={status:?} md_chars={} title={:?} url={}",
                doc.markdown.chars().count(),
                doc.metadata.title.as_deref().unwrap_or(""),
                m["url"].as_str().unwrap_or_default()
            );
        }
        let off = Options {
            detect_barrier: false,
            ..opts.clone()
        };
        // 交替测两次取较小差值，减少噪声
        let d1 = best_ms(&html, &opts) - best_ms(&html, &off);
        let d2 = best_ms(&html, &opts) - best_ms(&html, &off);
        let d = d1.min(d2);
        if d > 0.3 {
            println!(
                "SLOW {suffix} id={id} extra_ms={d:.3} html_bytes={}",
                html.len()
            );
        }
        extra.push(d);
    }
    extra.sort_by(f64::total_cmp);
    let pct = |p: f64| extra[((extra.len() - 1) as f64 * p) as usize];
    println!(
        "pages {pages}, barriers {hits}; barrier detection extra ms: p50 {:.4} p95 {:.4} max {:.4}",
        pct(0.5),
        pct(0.95),
        pct(1.0)
    );
}
