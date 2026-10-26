//! 字符集编码检测基准测试多线程并发执行程序 (32 核心并发版)。
//! 用法: `detect_encoding_mt <list_file> [num_threads=32] [output_tsv]`
//! 输出格式 (TSV): `<文件路径>\t<探测出的编码>\t<耗时微秒>`

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

struct DetectResult {
    index: usize,
    path: String,
    encoding: String,
    latency_us: u128,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: detect_encoding_mt <list_file> [num_threads] [output_tsv]");
        std::process::exit(1);
    }

    let list_path = &args[1];
    let num_threads: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(32)
    });
    let out_path = args.get(3).cloned();

    let file = File::open(list_path)?;
    let reader = BufReader::new(file);
    let paths: Vec<String> = reader
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let total = paths.len();

    let paths = Arc::new(paths);
    let next_index = Arc::new(AtomicUsize::new(0));

    let t_start = Instant::now();
    let mut handles = Vec::with_capacity(num_threads);

    for _ in 0..num_threads {
        let paths = Arc::clone(&paths);
        let next_index = Arc::clone(&next_index);

        let handle = std::thread::spawn(move || {
            let mut results = Vec::new();
            loop {
                let idx = next_index.fetch_add(1, Ordering::Relaxed);
                if idx >= paths.len() {
                    break;
                }
                let path_str = &paths[idx];
                let Ok(bytes) = std::fs::read(path_str) else {
                    continue;
                };

                let t0 = Instant::now();
                let decoded = pagesift_net::decode(&bytes, None, None);
                let us = t0.elapsed().as_micros();

                results.push(DetectResult {
                    index: idx,
                    path: path_str.clone(),
                    encoding: decoded.encoding_name.to_string(),
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

    let total_elapsed = t_start.elapsed().as_secs_f64();
    let throughput = total as f64 / total_elapsed.max(0.000001);

    eprintln!(
        "[detect_encoding_mt] Processed {} files across {} threads in {:.3}s ({:.1} files/s)",
        total, num_threads, total_elapsed, throughput
    );

    if let Some(ref path) = out_path {
        let mut out = BufWriter::new(File::create(path)?);
        for r in &all_results {
            writeln!(out, "{}\t{}\t{}", r.path, r.encoding, r.latency_us)?;
        }
        out.flush()?;
    } else {
        let out = std::io::stdout();
        let mut out = out.lock();
        for r in &all_results {
            writeln!(out, "{}\t{}\t{}", r.path, r.encoding, r.latency_us)?;
        }
    }

    Ok(())
}
