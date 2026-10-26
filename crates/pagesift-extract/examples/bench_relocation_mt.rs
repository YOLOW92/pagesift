//! 评测 DOM 元素模糊重定位与自愈能力的多线程可执行程序 (32 核心并发版)。
//! 输入 JSON Lines: `{"id": "...", "original_html": "...", "selector": "...", "mutated_html": "..."}`
//! 输出 TSV: `<id>\t<found>\t<matched_text>\t<score>\t<latency_us>`

use pagesift_extract::relocation::{
    ElementFingerprint, RelocateOutcome, RelocationOptions, relocate,
};
use pagesift_html::Html;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

struct RelocateResult {
    index: usize,
    id: String,
    found: bool,
    text: String,
    score: f64,
    latency_us: u128,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: bench_relocation_mt <input_jsonl> [num_threads] [output_tsv]");
        std::process::exit(1);
    }

    let input_path = &args[1];
    let num_threads: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(32)
    });
    let output_path = args.get(3).cloned();

    let file = File::open(input_path)?;
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let total = lines.len();

    let lines = Arc::new(lines);
    let next_index = Arc::new(AtomicUsize::new(0));

    let opts = RelocationOptions {
        min_score: 0.50,
        min_margin: 0.05,
    };

    let t_start = Instant::now();

    let mut handles = Vec::with_capacity(num_threads);
    for _ in 0..num_threads {
        let lines = Arc::clone(&lines);
        let next_index = Arc::clone(&next_index);
        let handle = std::thread::spawn(move || {
            let mut results = Vec::new();
            loop {
                let idx = next_index.fetch_add(1, Ordering::Relaxed);
                if idx >= lines.len() {
                    break;
                }
                let line = &lines[idx];
                let row: serde_json::Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let id = row["id"].as_str().unwrap_or("unknown").to_string();
                let orig_html = row["original_html"].as_str().unwrap_or("");
                let selector = row["selector"].as_str().unwrap_or("");
                let mut_html = row["mutated_html"].as_str().unwrap_or("");

                let t0 = Instant::now();
                let orig_doc = Html::parse(orig_html);

                let (found, text, score) = match ElementFingerprint::capture(&orig_doc, selector) {
                    Ok(fp) => {
                        let mut_doc = Html::parse(mut_html);
                        match relocate(&fp, &mut_doc, opts) {
                            RelocateOutcome::Found { node, score, .. } => {
                                (true, node.text().replace(['\t', '\n', '\r'], " "), score)
                            }
                            _ => (false, String::new(), 0.0),
                        }
                    }
                    Err(_) => (false, String::new(), 0.0),
                };
                let us = t0.elapsed().as_micros();
                results.push(RelocateResult {
                    index: idx,
                    id,
                    found,
                    text,
                    score,
                    latency_us: us,
                });
            }
            results
        });
        handles.push(handle);
    }

    let mut all_results = Vec::with_capacity(total);
    for h in handles {
        let res = h.join().unwrap();
        all_results.extend(res);
    }
    all_results.sort_by_key(|r| r.index);

    let total_elapsed = t_start.elapsed();
    let total_elapsed_s = total_elapsed.as_secs_f64();
    let throughput = (total as f64) / total_elapsed_s.max(0.000001);

    eprintln!(
        "[bench_relocation_mt] Processed {} cases across {} threads in {:.3}s ({:.1} ops/s)",
        total, num_threads, total_elapsed_s, throughput
    );

    if let Some(ref out_file) = output_path {
        let mut out = BufWriter::new(File::create(out_file)?);
        for r in &all_results {
            writeln!(
                out,
                "{}\t{}\t{}\t{:.4}\t{}",
                r.id, r.found, r.text, r.score, r.latency_us
            )?;
        }
        out.flush()?;
    } else {
        let out = std::io::stdout();
        let mut out = out.lock();
        for r in &all_results {
            writeln!(
                out,
                "{}\t{}\t{}\t{:.4}\t{}",
                r.id, r.found, r.text, r.score, r.latency_us
            )?;
        }
    }

    Ok(())
}
