//! 拦截页识别的离线复测（tasks/0.80/c）：读 `raw_fetch` 存下的页面，按当时的状态码跑一遍
//! [`pagesift_content::process`]，写出识别结果和正文开头，便于人工标注和新旧对比。
//! `barrier_probe <index.jsonl> <out.jsonl>`
use std::io::Write;

use pagesift_content::{Options, process};

fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: barrier_probe <index.jsonl> <out.jsonl>";
    let index = std::path::PathBuf::from(args.next().expect(usage));
    let out = args.next().expect(usage);
    let dir = index.parent().unwrap().to_path_buf();
    let mut w = std::io::BufWriter::new(std::fs::File::create(&out).unwrap());
    for line in std::fs::read_to_string(&index).unwrap().lines() {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        let Some(file) = row["file"].as_str() else {
            writeln!(w, "{row}").unwrap();
            continue;
        };
        let html = std::fs::read_to_string(dir.join(file)).unwrap();
        let opts = Options {
            base_url: row["final_url"]
                .as_str()
                .and_then(|u| url::Url::parse(u).ok()),
            status: row["status"].as_u64().map(|s| s as u16),
            ..Options::default()
        };
        let doc = process(&html, &opts);
        let head: String = doc.markdown.chars().take(600).collect();
        let rec = serde_json::json!({
            "n": row["n"], "url": row["url"], "status": row["status"],
            "title": doc.metadata.title,
            "barrier": doc.barrier.as_ref().map(|b| serde_json::json!({
                "vendor": b.vendor, "kind": b.kind.as_str(), "rule": b.rule_id, "confidence": b.confidence,
            })),
            "chars": doc.markdown.chars().filter(|c| !c.is_whitespace()).count(),
            "head": head,
        });
        writeln!(w, "{rec}").unwrap();
    }
}
