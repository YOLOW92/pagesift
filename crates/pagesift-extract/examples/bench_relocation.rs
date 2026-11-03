//! 评测 DOM 元素模糊重定位与自愈能力的可执行程序。
//! 输入 JSON Lines: `{"id": "...", "original_html": "...", "selector": "...", "mutated_html": "..."}`
//! 输出 TSV: `<id>\t<found>\t<matched_text>\t<score>\t<latency_us>\t<step>\t<posterior>\t<p>\t<candidate_text>`

use pagesift_extract::locate::{Located, Target, locate};
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

        let clean = |n: pagesift_html::NodeRef<'_>| n.text().replace(['\t', '\n', '\r'], " ");
        // 诊断列：走到的步骤、后验、背景检验 p 值、最高候选的文字（拒绝时也给出）
        let (found, text, score, step, post, p, cand) =
            match Target::capture(orig_doc.root(), selector, None) {
                Ok(fp) => {
                    let mut_doc = Html::parse(mut_html);
                    let l = locate(mut_doc.root(), &fp);
                    let step = l.step();
                    match l {
                        Located::Relocated {
                            node,
                            score,
                            posterior,
                            p,
                        } => (true, clean(node), score, step, posterior, p, clean(node)),
                        Located::Exact(node) | Located::Anchored(node) => {
                            (true, clean(node), 1.0, step, 1.0, 0.0, clean(node))
                        }
                        Located::Ambiguous {
                            best,
                            posterior,
                            p,
                            candidate,
                        } => (
                            false,
                            String::new(),
                            best,
                            step,
                            posterior,
                            p,
                            candidate.map(clean).unwrap_or_default(),
                        ),
                        Located::Missing => {
                            (false, String::new(), 0.0, step, 0.0, 1.0, String::new())
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[FAIL capture] id={id} err={e}");
                    (
                        false,
                        String::new(),
                        0.0,
                        "capture",
                        0.0,
                        1.0,
                        String::new(),
                    )
                }
            };
        let us = t0.elapsed().as_micros();
        writeln!(
            out,
            "{id}\t{found}\t{text}\t{score:.4}\t{us}\t{step}\t{post:.6}\t{p:.3e}\t{cand}"
        )?;
    }

    Ok(())
}
