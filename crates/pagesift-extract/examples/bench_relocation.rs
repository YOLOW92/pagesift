//! 评测 DOM 元素模糊重定位与自愈能力的可执行程序。
//! 输入 JSON Lines: `{"id": "...", "original_html": "...", "selector": "...", "mutated_html": "..."}`
//! 输出 TSV: `<id>\t<found>\t<matched_text>\t<score>\t<latency_us>`

use pagesift_extract::{FuzzyAnchor, FuzzyMatch, FuzzyOptions};
use pagesift_html::Html;
use std::io::{BufRead, Write};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: bench_relocation <input_jsonl>");
        std::process::exit(1);
    }

    let file = std::fs::File::open(&args[1])?;
    let out = std::io::stdout();
    let mut out = out.lock();

    let opts = FuzzyOptions {
        minimum_score: 0.50,
        minimum_margin: 0.05,
    };

    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let row: serde_json::Value = serde_json::from_str(&line)?;
        let id = row["id"].as_str().unwrap_or("unknown");
        let orig_html = row["original_html"].as_str().unwrap_or("");
        let selector = row["selector"].as_str().unwrap_or("");
        let mut_html = row["mutated_html"].as_str().unwrap_or("");

        let orig_doc = Html::parse(orig_html);
        let t0 = Instant::now();

        let (found, text, score) = match FuzzyAnchor::capture(&orig_doc, selector) {
            Ok(anchor) => {
                let mut_doc = Html::parse(mut_html);
                match anchor.locate(&mut_doc, opts) {
                    Ok(FuzzyMatch::Candidate { node, score, .. }) => {
                        (true, node.text().replace(['\t', '\n', '\r'], " "), score)
                    }
                    _ => (false, String::new(), 0.0),
                }
            }
            Err(_) => (false, String::new(), 0.0),
        };

        let us = t0.elapsed().as_micros();
        writeln!(out, "{id}\t{found}\t{text}\t{score:.4}\t{us}")?;
    }

    Ok(())
}
