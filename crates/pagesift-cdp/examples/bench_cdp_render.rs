//! PageSift CDP 渲染基准测试程序。
//! 用法: `bench_cdp_render <url> [concurrency=32] [iterations=50]`

use std::sync::Arc;
use std::time::Instant;

use pagesift_cdp::{
    BoxFuture, BrowserPool, PoolConfig, RenderOptions, RequestGuard, SubRequest, Verdict,
};

#[derive(Debug, Default, Clone)]
struct BenchmarkGuard;

impl RequestGuard for BenchmarkGuard {
    fn check<'a>(&'a self, _req: &'a SubRequest) -> BoxFuture<'a, Verdict> {
        Box::pin(async move { Verdict::Allow })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let url = args
        .get(1)
        .map(|s| s.as_str())
        .unwrap_or("http://127.0.0.1:18101/");
    let concurrency: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(32);
    let iterations: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(50);

    let mut config = PoolConfig::default();
    config.max_browsers = concurrency.max(1);

    eprintln!(
        "[PageSift CDP] Initializing BrowserPool (concurrency: {}, target: {})...",
        concurrency, url
    );

    let t_init = Instant::now();
    let pool = BrowserPool::new(config, Arc::new(BenchmarkGuard)).await?;
    let init_ms = t_init.elapsed().as_secs_f64() * 1000.0;
    eprintln!("[PageSift CDP] BrowserPool initialized in {:.2}ms", init_ms);

    let pool = Arc::new(pool);
    let options = Arc::new(RenderOptions::default());

    let t_start = Instant::now();
    let mut tasks = Vec::with_capacity(iterations);

    for _ in 0..iterations {
        let pool = Arc::clone(&pool);
        let opts = Arc::clone(&options);
        let target = url.to_string();

        tasks.push(tokio::spawn(async move {
            let t0 = Instant::now();
            let res = pool.render(&target, &opts).await;
            let us = t0.elapsed().as_micros();
            match res {
                Ok(page) => (true, us, page.html.len()),
                Err(_) => (false, us, 0),
            }
        }));
    }

    let mut success_count = 0;
    let mut total_bytes = 0;
    let mut latencies_us = Vec::with_capacity(iterations);

    for task in tasks {
        let (ok, us, len) = task.await?;
        if ok {
            success_count += 1;
            total_bytes += len;
        }
        latencies_us.push(us);
    }

    let total_elapsed = t_start.elapsed().as_secs_f64();
    let throughput = iterations as f64 / total_elapsed.max(0.000001);

    latencies_us.sort_unstable();
    let p50_ms = latencies_us[iterations / 2] as f64 / 1000.0;
    let p95_ms = latencies_us[(iterations * 95) / 100] as f64 / 1000.0;

    println!(
        "PageSift CDP: {}/{} renders in {:.3}s ({:.1} p/s) | p50: {:.2}ms, p95: {:.2}ms | Total bytes: {}",
        success_count, iterations, total_elapsed, throughput, p50_ms, p95_ms, total_bytes
    );

    pool.shutdown().await;
    Ok(())
}
