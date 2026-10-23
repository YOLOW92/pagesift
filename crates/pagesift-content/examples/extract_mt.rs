//! Multi-threaded extraction throughput benchmark using pure std::thread::scope.
use pagesift_content::{Options, process};
use std::time::Instant;

fn main() {
    let manifest_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "manifest.jsonl".to_string());
    let threads: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    // 1. Preload all HTML into memory to isolate pure extraction engine throughput
    let mut pages = Vec::new();
    for line in std::fs::read_to_string(&manifest_path).unwrap().lines() {
        if line.trim().is_empty() {
            continue;
        }
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        let path = row["path"].as_str().unwrap();
        let html = std::fs::read_to_string(path).unwrap_or_default();
        let base = row["base_url"]
            .as_str()
            .or(row["url"].as_str())
            .map(ToString::to_string);
        pages.push((html, base));
    }
    let total_pages = pages.len();

    // 2. Multi-threaded benchmark using std::thread::scope
    let chunk_size = (total_pages + threads - 1) / threads;
    let t0 = Instant::now();
    std::thread::scope(|s| {
        for chunk in pages.chunks(chunk_size) {
            s.spawn(move || {
                for (html, base) in chunk {
                    let opts = Options {
                        base_url: base.as_deref().and_then(|u| url::Url::parse(u).ok()),
                        ..Options::default()
                    };
                    let _doc = process(html, &opts);
                }
            });
        }
    });
    let elapsed = t0.elapsed().as_secs_f64();
    let qps = total_pages as f64 / elapsed;
    println!(
        "Threads: {:2} | Total: {} pages | Elapsed: {:.3}s | Throughput: {:7.1} pages/sec | Latency: {:.2} ms/page",
        threads,
        total_pages,
        elapsed,
        qps,
        (elapsed * 1000.0) / total_pages as f64
    );
}
