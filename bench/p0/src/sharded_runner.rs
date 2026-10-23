use clap::Parser;
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

#[derive(Parser, Debug)]
pub struct Opts {
    #[arg(short, long, default_value = "http://127.0.0.1:19800")]
    pub origin_url: String,
    #[arg(short, long, default_value_t = 1_000_000)]
    pub total_tasks: u64,
    #[arg(short, long, default_value_t = 8)]
    pub shards: usize,
    #[arg(short, long, default_value_t = 128)]
    pub concurrency_per_shard: usize,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = Opts::parse();
    let per_shard_tasks = opts.total_tasks / opts.shards as u64;
    let total_concurrency = opts.shards * opts.concurrency_per_shard;

    println!("============================================================");
    println!("  PageSift 0.60 Sharded Share-Nothing Architecture Runner");
    println!("  Target Origin:     {}", opts.origin_url);
    println!("  Total Tasks:       {}", opts.total_tasks);
    println!("  Parallel Shards:   {} independent dispatchers", opts.shards);
    println!("  Per-shard tasks:   {}", per_shard_tasks);
    println!("  Per-shard conc:    {}", opts.concurrency_per_shard);
    println!("  Total Concurrency: {}", total_concurrency);
    println!("============================================================");

    // Each shard gets its own dedicated atomic counter (no cross-shard cache bouncing)
    let mut shard_counters: Vec<Arc<AtomicU64>> = Vec::with_capacity(opts.shards);
    let mut shard_bytes: Vec<Arc<AtomicU64>> = Vec::with_capacity(opts.shards);
    for _ in 0..opts.shards {
        shard_counters.push(Arc::new(AtomicU64::new(0)));
        shard_bytes.push(Arc::new(AtomicU64::new(0)));
    }

    let running = Arc::new(AtomicBool::new(true));
    let t_running = running.clone();
    let t_counters = shard_counters.clone();
    let total_tasks_target = opts.total_tasks;

    // Real-time telemetry monitor
    let telemetry_handle = tokio::spawn(async move {
        let t0 = Instant::now();
        let mut last_tasks = 0u64;
        let mut last_time = Instant::now();

        while t_running.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(1000)).await;
            if !t_running.load(Ordering::Relaxed) {
                break;
            }

            let cur_tasks: u64 = t_counters.iter().map(|c| c.load(Ordering::Relaxed)).sum();
            let dt = last_time.elapsed().as_secs_f64().max(0.001);
            let d_tasks = cur_tasks.saturating_sub(last_tasks);
            let instant_qps = d_tasks as f64 / dt;

            let total_dt = t0.elapsed().as_secs_f64().max(0.001);
            let sustained_qps = cur_tasks as f64 / total_dt;

            last_tasks = cur_tasks;
            last_time = Instant::now();

            let rss_mb = get_current_rss_mb();
            let cpu_pct = get_cpu_utilization();
            let pct = (cur_tasks as f64 / total_tasks_target as f64) * 100.0;

            println!(
                "[{:>5.1}%] Completed: {:>7}/{} | Instant QPS: {:>8.1} (Sustained: {:>8.1}) | RSS: {:>5.1}MB | CPU: {:>4.1}%",
                pct, cur_tasks, total_tasks_target, instant_qps, sustained_qps, rss_mb, cpu_pct
            );

            if cur_tasks >= total_tasks_target {
                break;
            }
        }
    });

    let start_all = Instant::now();
    let mut shard_handles = Vec::new();

    for shard_id in 0..opts.shards {
        let sem = Arc::new(Semaphore::new(opts.concurrency_per_shard));
        // Dedicated connection pool per shard: zero mutex contention between shards!
        let shard_client = reqwest::Client::builder()
            .pool_max_idle_per_host(opts.concurrency_per_shard * 2)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_nodelay(true)
            .build()?;

        let cnt = shard_counters[shard_id].clone();
        let b_cnt = shard_bytes[shard_id].clone();
        let origin = opts.origin_url.clone();
        let conc = opts.concurrency_per_shard;

        let handle = tokio::spawn(async move {
            for i in 0..per_shard_tasks {
                let permit = match sem.clone().acquire_owned().await {
                    Ok(p) => p,
                    Err(_) => break,
                };
                let c_inner = shard_client.clone();
                let cnt_inner = cnt.clone();
                let b_cnt_inner = b_cnt.clone();
                let task_id = shard_id as u64 * per_shard_tasks + i;
                let url = format!("{}/p/{}", origin, task_id);

                tokio::spawn(async move {
                    let _permit = permit;
                    if let Ok(resp) = c_inner.get(&url).send().await {
                        if let Ok(body) = resp.bytes().await {
                            b_cnt_inner.fetch_add(body.len() as u64, Ordering::Relaxed);
                            if let Ok(s) = std::str::from_utf8(&body) {
                                let doc = pagesift_content::process(s, &Default::default());
                                if !doc.markdown.is_empty() {
                                    cnt_inner.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                });
            }
            let _ = sem.acquire_many(conc as u32).await;
        });
        shard_handles.push(handle);
    }

    for h in shard_handles {
        let _ = h.await;
    }

    let elapsed = start_all.elapsed().as_secs_f64();
    running.store(false, Ordering::Relaxed);
    let _ = telemetry_handle.await;

    let total_finished: u64 = shard_counters.iter().map(|c| c.load(Ordering::Relaxed)).sum();
    let total_bytes: u64 = shard_bytes.iter().map(|c| c.load(Ordering::Relaxed)).sum();
    let sustained_qps = total_finished as f64 / elapsed;

    println!("\n============================================================");
    println!("  SHARDED ARCHITECTURE RUN SUMMARY");
    println!("============================================================");
    println!("  Wall Clock Time:    {:.2}s", elapsed);
    println!("  Tasks Completed:    {}", total_finished);
    println!("  Sustained QPS:      {:.2} pages/sec", sustained_qps);
    println!("  Throughput Volume:  {:.2} MB/s ({:.2} GB total)",
        (total_bytes as f64 / 1_048_576.0) / elapsed,
        total_bytes as f64 / 1_073_741_824.0
    );
    println!("  Peak Memory (RSS):  {:.1} MB", get_current_rss_mb());
    println!("============================================================");

    Ok(())
}

fn get_current_rss_mb() -> f64 {
    if let Ok(statm) = fs::read_to_string("/proc/self/statm") {
        if let Some(pages_str) = statm.split_whitespace().nth(1) {
            if let Ok(pages) = pages_str.parse::<u64>() {
                return (pages * 4096) as f64 / 1_048_576.0;
            }
        }
    }
    0.0
}

static mut PREV_WORK_CPU: u64 = 0;
static mut PREV_TOTAL_CPU: u64 = 0;

fn get_cpu_utilization() -> f64 {
    if let Ok(stat) = fs::read_to_string("/proc/stat") {
        if let Some(line) = stat.lines().next() {
            let parts: Vec<u64> = line
                .split_whitespace()
                .skip(1)
                .filter_map(|s| s.parse().ok())
                .collect();
            if parts.len() >= 4 {
                let user = parts[0];
                let nice = parts[1];
                let system = parts[2];
                let idle = parts[3];
                let iowait = parts.get(4).copied().unwrap_or(0);
                let irq = parts.get(5).copied().unwrap_or(0);
                let softirq = parts.get(6).copied().unwrap_or(0);

                let work = user + nice + system + irq + softirq;
                let total = work + idle + iowait;

                unsafe {
                    let d_work = work.saturating_sub(PREV_WORK_CPU);
                    let d_total = total.saturating_sub(PREV_TOTAL_CPU);
                    PREV_WORK_CPU = work;
                    PREV_TOTAL_CPU = total;
                    if d_total > 0 {
                        return ((d_work as f64 / d_total as f64) * 100.0).clamp(0.0, 100.0);
                    }
                }
            }
        }
    }
    0.0
}
