//! PageSift 0.50 QueryFilter 10,000 级并发查询风暴评测程序。
//! 用法: `bench_qf_storm <corpus.md> <queries.jsonl> [num_threads=32]`

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use pagesift_extract::{QueryFilterOptions, filter_markdown_by_query};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: bench_qf_storm <corpus.md> <queries.jsonl> [num_threads]");
        std::process::exit(1);
    }

    let corpus_path = &args[1];
    let queries_path = &args[2];
    let num_threads: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(32)
    });

    eprintln!("[bench_qf_storm] Loading corpus from {}...", corpus_path);
    let corpus = Arc::new(std::fs::read_to_string(corpus_path)?);
    let corpus_len_mb = corpus.len() as f64 / (1024.0 * 1024.0);

    eprintln!("[bench_qf_storm] Loading queries from {}...", queries_path);
    let file = File::open(queries_path)?;
    let reader = BufReader::new(file);
    let mut queries = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let row: serde_json::Value = serde_json::from_str(&line)?;
        if let Some(q) = row["query"].as_str() {
            queries.push(q.to_string());
        }
    }
    let total_queries = queries.len();
    eprintln!(
        "[bench_qf_storm] Loaded {} queries. Starting benchmark on {:.2} MB corpus across {} threads...",
        total_queries, corpus_len_mb, num_threads
    );

    let queries = Arc::new(queries);
    let next_idx = Arc::new(AtomicUsize::new(0));
    let opts = Arc::new(QueryFilterOptions::default());

    let t_start = Instant::now();
    let mut handles = Vec::with_capacity(num_threads);

    for _ in 0..num_threads {
        let corpus = Arc::clone(&corpus);
        let queries = Arc::clone(&queries);
        let next_idx = Arc::clone(&next_idx);
        let opts = Arc::clone(&opts);

        let handle = std::thread::spawn(move || {
            let mut latencies = Vec::new();
            loop {
                let idx = next_idx.fetch_add(1, Ordering::Relaxed);
                if idx >= queries.len() {
                    break;
                }
                let q = &queries[idx];
                let t0 = Instant::now();
                let res = filter_markdown_by_query(&corpus, q, &opts);
                let us = t0.elapsed().as_micros();
                // 确保结果不被编译器优化掉
                std::hint::black_box(res.kept);
                latencies.push(us);
            }
            latencies
        });
        handles.push(handle);
    }

    let mut all_latencies = Vec::with_capacity(total_queries);
    for h in handles {
        let lats = h.join().unwrap();
        all_latencies.extend(lats);
    }

    let total_elapsed = t_start.elapsed();
    let total_secs = total_elapsed.as_secs_f64();
    let qps = total_queries as f64 / total_secs;

    all_latencies.sort();
    let mean_us = all_latencies.iter().sum::<u128>() as f64 / total_queries as f64;
    let p50_us = all_latencies[total_queries * 50 / 100];
    let p95_us = all_latencies[total_queries * 95 / 100];
    let p99_us = all_latencies[total_queries * 99 / 100];

    println!(
        "{{\"queries\": {}, \"corpus_mb\": {:.2}, \"threads\": {}, \"wall_s\": {:.4}, \"qps\": {:.1}, \"mean_us\": {:.1}, \"p50_us\": {}, \"p95_us\": {}, \"p99_us\": {}}}",
        total_queries, corpus_len_mb, num_threads, total_secs, qps, mean_us, p50_us, p95_us, p99_us
    );

    Ok(())
}
