//! JavaScript 端点与隐式 API 提取基准测试多线程执行程序 (32 核心并发版)。
//! 用法: `bench_js_endpoints_mt <list_file> [num_threads=32] [output_tsv]`
//! 输出格式 (TSV): `<文件路径>\t<耗时微秒>\t<导航/API端点数>\t<依赖模块数>`

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use pagesift_content::scan_script;
use url::Url;

struct EndpointsResult {
    index: usize,
    path: String,
    latency_us: u128,
    navigation_count: usize,
    dependencies_count: usize,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: bench_js_endpoints_mt <list_file> [num_threads] [output_tsv]");
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

    let default_base = Url::parse("https://example.com/assets/app.js").unwrap();
    let base = Arc::new(default_base);

    let t_start = Instant::now();
    let mut handles = Vec::with_capacity(num_threads);

    for _ in 0..num_threads {
        let paths = Arc::clone(&paths);
        let next_index = Arc::clone(&next_index);
        let base = Arc::clone(&base);

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
                let code = String::from_utf8_lossy(&bytes);

                let t0 = Instant::now();
                let scan = scan_script(&code, &base);
                let us = t0.elapsed().as_micros();

                if std::env::var("DUMP_URLS").is_ok() {
                    for u in &scan.navigation {
                        eprintln!("[DUMP] Found endpoint: {u}");
                    }
                }

                results.push(EndpointsResult {
                    index: idx,
                    path: path_str.clone(),
                    latency_us: us,
                    navigation_count: scan.navigation.len(),
                    dependencies_count: scan.dependencies.len(),
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
        "[bench_js_endpoints_mt] Processed {} files across {} threads in {:.3}s ({:.1} files/s)",
        total, num_threads, total_elapsed, throughput
    );

    if let Some(ref path) = out_path {
        let mut out = BufWriter::new(File::create(path)?);
        for r in &all_results {
            writeln!(
                out,
                "{}\t{}\t{}\t{}",
                r.path, r.latency_us, r.navigation_count, r.dependencies_count
            )?;
        }
        out.flush()?;
    } else {
        let stdout = std::io::stdout();
        let mut out = BufWriter::new(stdout.lock());
        for r in &all_results {
            writeln!(
                out,
                "{}\t{}\t{}\t{}",
                r.path, r.latency_us, r.navigation_count, r.dependencies_count
            )?;
        }
        out.flush()?;
    }

    Ok(())
}
