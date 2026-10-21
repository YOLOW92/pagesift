//! 对比评测用的 pagesift 适配器：和 bench/compare 里其他工具的适配器一样，读清单
//! （每行 `{id, path, url, base_url}`），逐页单线程转换正文，写出 `{id, markdown, ms}`。
//! 每页只计一次耗时（和其他适配器一致），不含读文件。
//! `cargo run --release -p pagesift-content --example extract_jsonl -- <manifest.jsonl> <out.jsonl>`
use std::io::Write;
use std::time::Instant;

use pagesift_content::{Options, process};

fn main() {
    let mut args = std::env::args().skip(1);
    let manifest = args
        .next()
        .expect("usage: extract_jsonl <manifest.jsonl> <out.jsonl>");
    let out = args
        .next()
        .expect("usage: extract_jsonl <manifest.jsonl> <out.jsonl>");
    let mut w = std::io::BufWriter::new(std::fs::File::create(&out).unwrap());
    let mut total = 0.0;
    let mut n = 0usize;
    for line in std::fs::read_to_string(&manifest).unwrap().lines() {
        if line.trim().is_empty() {
            continue;
        }
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        let path = row["path"].as_str().unwrap();
        let html = String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned();
        let base = row["base_url"].as_str().or(row["url"].as_str());
        let opts = Options {
            base_url: base.and_then(|u| url::Url::parse(u).ok()),
            ..Options::default()
        };
        let t = Instant::now();
        let doc = process(&html, &opts);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        total += ms;
        n += 1;
        let rec = serde_json::json!({"id": row["id"], "markdown": doc.markdown, "ms": ms});
        writeln!(w, "{rec}").unwrap();
    }
    eprintln!(
        "pagesift: {n} pages, {:.2} ms/page",
        total / n.max(1) as f64
    );
}
