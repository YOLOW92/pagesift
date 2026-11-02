//! 质量分的离线数据：逐页提取正文，估计文字体系权重，输出特征。
//!
//! `cargo run --release -p pagesift-plan --example quality_data -- <manifest.jsonl> <out.jsonl> <weights.json>`
//!
//! 清单每行 `{id, set, path, url}`。两遍：第一遍提取并累计各文字体系的字符分布，
//! 第二遍用估出的权重算特征。输出每行 `{id, set, markdown, features}`。
use std::io::Write;

use pagesift_content::{Options, process};
use pagesift_plan::quality::{FEATURE_NAMES, features};
use pagesift_plan::script::ScriptStats;
use pagesift_stat::Delta;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [manifest, out, weights_out] = args.as_slice() else {
        panic!("usage: quality_data <manifest.jsonl> <out.jsonl> <weights.json>");
    };
    let mut rows = Vec::new();
    let mut stats = ScriptStats::default();
    for line in std::fs::read_to_string(manifest)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
    {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        let html = String::from_utf8_lossy(&std::fs::read(row["path"].as_str().unwrap()).unwrap())
            .into_owned();
        let opts = Options {
            base_url: row["url"].as_str().and_then(|u| url::Url::parse(u).ok()),
            ..Options::default()
        };
        let doc = process(&html, &opts);
        stats.observe(doc.plain_text.as_deref().unwrap_or(&doc.markdown));
        rows.push((row, html.len(), doc));
    }
    let weights = stats.weights(Delta::DEFAULT);
    let mut w = std::io::BufWriter::new(std::fs::File::create(out).unwrap());
    for (row, html_bytes, doc) in &rows {
        let f = features(doc, *html_bytes, &weights);
        let rec = serde_json::json!({"id": row["id"], "set": row["set"], "markdown": doc.markdown, "features": f});
        writeln!(w, "{rec}").unwrap();
    }
    let table: serde_json::Map<String, serde_json::Value> = weights
        .iter()
        .map(|(s, wt)| {
            let (h, n) = stats.entropy(s).unwrap_or((f64::NAN, 0));
            (
                format!("{s:?}"),
                serde_json::json!({"weight": wt, "entropy_nats": h, "chars": n}),
            )
        })
        .collect();
    std::fs::write(weights_out, serde_json::to_string_pretty(&table).unwrap()).unwrap();
    eprintln!(
        "quality_data: {} pages, {} features",
        rows.len(),
        FEATURE_NAMES.len()
    );
}
