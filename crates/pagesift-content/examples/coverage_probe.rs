//! 完整度诊断：逐页写出正文、整页（不只取正文）的 Markdown 和完整度警告（[`pagesift_content::Coverage::warnings`]），
//! 给警告校准（tasks/0.80/c）离线分析用。
//! `cargo run --release -p pagesift-content --example coverage_probe -- <manifest.jsonl> <out.jsonl>`
use std::io::Write;

use pagesift_content::{Options, process};

fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: coverage_probe <manifest.jsonl> <out.jsonl>";
    let manifest = args.next().expect(usage);
    let out = args.next().expect(usage);
    let mut w = std::io::BufWriter::new(std::fs::File::create(&out).unwrap());
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
        let doc = process(&html, &opts);
        let full = process(
            &html,
            &Options {
                only_main_content: false,
                detect_barrier: false,
                ..opts.clone()
            },
        );
        let rec = serde_json::json!({
            "id": row["id"],
            "markdown": doc.markdown,
            "full": full.markdown,
            "html_bytes": html.len(),
            "coverage_warnings": doc.coverage.warnings(),
        });
        writeln!(w, "{rec}").unwrap();
    }
}
