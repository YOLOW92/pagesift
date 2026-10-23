//! 结构化数据提取基准测试多线程并发执行程序 (32 核心并发版)。
//! 用法: `bench_structured_mt <list_file> [num_threads=32] [output_tsv]`
//! 输出格式 (TSV): `<文件路径>\t<耗时微秒>\t<JSON-LD实体数>\t<Microdata实体数>\t<OpenGraph属性数>`

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use pagesift_extract::extract_structured;

struct StructResult {
    index: usize,
    path: String,
    latency_us: u128,
    json_ld_count: usize,
    microdata_count: usize,
    opengraph_count: usize,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: bench_structured_mt <list_file> [num_threads] [output_tsv]");
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
        .filter_map(|l| l.ok())
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
                let html = String::from_utf8_lossy(&bytes);

                let t0 = Instant::now();
                let res = extract_structured(&html);
                let us = t0.elapsed().as_micros();

                results.push(StructResult {
                    index: idx,
                    path: path_str.clone(),
                    latency_us: us,
                    json_ld_count: res.json_ld.len(),
                    microdata_count: res.microdata.len(),
                    opengraph_count: res.opengraph.len(),
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
        "[bench_structured_mt] Processed {} files across {} threads in {:.3}s ({:.1} files/s)",
        total, num_threads, total_elapsed, throughput
    );

    if let Some(ref path) = out_path {
        let mut out = BufWriter::new(File::create(path)?);
        for r in &all_results {
            writeln!(
                out,
                "{}\t{}\t{}\t{}\t{}",
                r.path, r.latency_us, r.json_ld_count, r.microdata_count, r.opengraph_count
            )?;
        }
        out.flush()?;
    } else {
        let stdout = std::io::stdout();
        let mut out = BufWriter::new(stdout.lock());
        for r in &all_results {
            writeln!(
                out,
                "{}\t{}\t{}\t{}\t{}",
                r.path, r.latency_us, r.json_ld_count, r.microdata_count, r.opengraph_count
            )?;
        }
        out.flush()?;
    }

    Ok(())
}
