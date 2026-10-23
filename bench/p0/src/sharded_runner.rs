use clap::Parser;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

#[derive(Parser, Debug)]
#[command(author, version, about = "PageSift 0.60 Sharded Share-Nothing 100M Runner")]
pub struct Opts {
    #[arg(short, long, default_value = "http://127.0.0.1:19800")]
    pub origin_url: String,
    #[arg(short, long, default_value_t = 100_000_000)]
    pub total_tasks: u64,
    #[arg(short, long, default_value_t = 16)]
    pub shards: usize,
    #[arg(short, long, default_value_t = 128)]
    pub concurrency_per_shard: usize,
    /// Automatically sense CPU topology (e.g. 80C, 300C, 384C) and calculate optimal shards & concurrency
    #[arg(long)]
    pub auto: bool,
    #[arg(long, default_value = "samples_100m")]
    pub sample_dir: PathBuf,
    #[arg(long, default_value_t = 10_000)] // 1 in 10,000 = 0.01%
    pub sample_every: u64,
    #[arg(long, default_value = "100m_sharded_report.jsonl")]
    pub report_file: PathBuf,
}

#[repr(align(64))]
#[derive(Default)]
pub struct ShardCounters {
    pub n_task: AtomicU64,
    pub n_req: AtomicU64,
    pub n_page: AtomicU64,
    pub n_failed: AtomicU64,
    pub n_bytes: AtomicU64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = Opts::parse();
    fs::create_dir_all(&opts.sample_dir)?;

    // Universal Autonomous Topology Calculation:
    // S(N) = ceil(N_cpu / 8), C*(N) = N_cpu * 25, C_shard = ceil(C* / S)
    let (shards, conc_per_shard) = if opts.auto {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let s = (cores.div_ceil(8)).clamp(1, 64);
        let c_total = cores * 25;
        let c_shard = (c_total / s).max(16);
        (s, c_shard)
    } else {
        (opts.shards, opts.concurrency_per_shard)
    };

    let total_concurrency = shards * conc_per_shard;
    let base_tasks_per_shard = opts.total_tasks / shards as u64;
    let remainder_tasks = opts.total_tasks % shards as u64;

    println!("============================================================");
    println!("  PageSift 0.60 Official 100M Sharded Benchmark Runner");
    println!("  Target Origin:       {}", opts.origin_url);
    println!("  Total Tasks:         {} (100 Million)", opts.total_tasks);
    println!("  Topology Scaling:    {}", if opts.auto { "Autonomous C*(N) Scaling" } else { "Manual Topology" });
    println!("  Parallel Shards:     {} independent dispatchers", shards);
    println!("  Conc per Shard:      {}", conc_per_shard);
    println!("  Total Concurrency:   {}", total_concurrency);
    println!("  Disk Sample Dir:     {:?} (0.01% rate, 1 in {})", opts.sample_dir, opts.sample_every);
    println!("  Telemetry Report:    {:?}", opts.report_file);
    println!("============================================================");

    // Each shard gets its own dedicated cache-line aligned atomic counter set
    let mut shard_counters: Vec<Arc<ShardCounters>> = Vec::with_capacity(shards);
    for _ in 0..shards {
        shard_counters.push(Arc::new(ShardCounters::default()));
    }

    let running = Arc::new(AtomicBool::new(true));
    let t_running = running.clone();
    let t_counters = shard_counters.clone();
    let total_tasks_target = opts.total_tasks;
    let report_path = opts.report_file.clone();

    // Real-time telemetry monitor
    let telemetry_handle = tokio::spawn(async move {
        let mut report_writer = File::create(&report_path).ok();
        let t0 = Instant::now();
        let mut last_tasks = 0u64;
        let mut last_time = Instant::now();
        let mut peak_rss_mb = 0.0f64;

        while t_running.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(1000)).await;
            if !t_running.load(Ordering::Relaxed) {
                break;
            }

            let cur_tasks: u64 = t_counters.iter().map(|c| c.n_task.load(Ordering::Relaxed)).sum();
            let cur_reqs: u64 = t_counters.iter().map(|c| c.n_req.load(Ordering::Relaxed)).sum();
            let cur_pages: u64 = t_counters.iter().map(|c| c.n_page.load(Ordering::Relaxed)).sum();
            let cur_failed: u64 = t_counters.iter().map(|c| c.n_failed.load(Ordering::Relaxed)).sum();
            let cur_bytes: u64 = t_counters.iter().map(|c| c.n_bytes.load(Ordering::Relaxed)).sum();

            let dt = last_time.elapsed().as_secs_f64().max(0.001);
            let d_tasks = cur_tasks.saturating_sub(last_tasks);
            let instant_qps = d_tasks as f64 / dt;

            let total_dt = t0.elapsed().as_secs_f64().max(0.001);
            let sustained_qps = cur_tasks as f64 / total_dt;

            last_tasks = cur_tasks;
            last_time = Instant::now();

            let rss_mb = get_current_rss_mb();
            if rss_mb > peak_rss_mb {
                peak_rss_mb = rss_mb;
            }
            let cpu_pct = get_cpu_utilization();
            let pct = (cur_tasks as f64 / total_tasks_target as f64) * 100.0;
            let eta_sec = if sustained_qps > 0.0 {
                (total_tasks_target.saturating_sub(cur_tasks) as f64) / sustained_qps
            } else {
                0.0
            };

            println!(
                "[{:>5.2}%] Tasks: {:>8}/{} | Instant: {:>8.1} p/s (Sustained: {:>8.1}) | RSS: {:>5.1}MB (peak: {:>5.1}MB) | CPU: {:>4.1}% | ETA: {:>4.1}m",
                pct, cur_tasks, total_tasks_target, instant_qps, sustained_qps, rss_mb, peak_rss_mb, cpu_pct, eta_sec / 60.0
            );

            if let Some(w) = report_writer.as_mut() {
                let log_entry = serde_json::json!({
                    "elapsed_s": total_dt,
                    "n_task": cur_tasks,
                    "n_req": cur_reqs,
                    "n_page": cur_pages,
                    "n_failed": cur_failed,
                    "bytes_mb": cur_bytes as f64 / 1_048_576.0,
                    "cur_qps": instant_qps,
                    "sustained_qps": sustained_qps,
                    "rss_mb": rss_mb,
                    "peak_rss_mb": peak_rss_mb,
                    "cpu_pct": cpu_pct
                });
                let _ = writeln!(w, "{}", log_entry.to_string());
                let _ = w.flush();
            }

            if cur_tasks >= total_tasks_target {
                break;
            }
        }
    });

    let start_all = Instant::now();
    let mut shard_handles = Vec::with_capacity(shards);
    let mut current_offset = 0u64;

    for shard_id in 0..shards {
        let sem = Arc::new(Semaphore::new(conc_per_shard));
        let shard_client = reqwest::Client::builder()
            .pool_max_idle_per_host(conc_per_shard * 2)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_nodelay(true)
            .build()?;

        let counters = shard_counters[shard_id].clone();
        let origin = opts.origin_url.clone();
        let conc = conc_per_shard;
        let my_tasks = base_tasks_per_shard + if (shard_id as u64) < remainder_tasks { 1 } else { 0 };
        let shard_start = current_offset;
        current_offset += my_tasks;
        let sample_dir = opts.sample_dir.clone();
        let sample_every = opts.sample_every;

        let handle = tokio::spawn(async move {
            for i in 0..my_tasks {
                let permit = match sem.clone().acquire_owned().await {
                    Ok(p) => p,
                    Err(_) => break,
                };
                let c_inner = shard_client.clone();
                let cnt_inner = counters.clone();
                let task_id = shard_start + i;
                let url = format!("{}/p/{}", origin, task_id);
                let s_dir = sample_dir.clone();

                tokio::spawn(async move {
                    let _permit = permit;
                    cnt_inner.n_task.fetch_add(1, Ordering::Relaxed);
                    cnt_inner.n_req.fetch_add(1, Ordering::Relaxed);

                    match c_inner.get(&url).send().await {
                        Ok(resp) => {
                            if let Ok(body) = resp.bytes().await {
                                cnt_inner.n_bytes.fetch_add(body.len() as u64, Ordering::Relaxed);
                                if let Ok(s) = std::str::from_utf8(&body) {
                                    let options = pagesift_content::Options {
                                        base_url: url::Url::parse(&url).ok(),
                                        ..Default::default()
                                    };
                                    let doc = pagesift_content::process(s, &options);
                                    if !doc.markdown.is_empty() {
                                        cnt_inner.n_page.fetch_add(1, Ordering::Relaxed);
                                        if task_id % sample_every == 0 {
                                            let sample_path = s_dir.join(format!("sample_{}.md", task_id));
                                            let _ = fs::write(sample_path, &doc.markdown);
                                        }
                                    } else {
                                        cnt_inner.n_failed.fetch_add(1, Ordering::Relaxed);
                                    }
                                } else {
                                    cnt_inner.n_failed.fetch_add(1, Ordering::Relaxed);
                                }
                            } else {
                                cnt_inner.n_failed.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Err(e) => {
                            cnt_inner.n_failed.fetch_add(1, Ordering::Relaxed);
                            static ERR_COUNT: AtomicU64 = AtomicU64::new(0);
                            if ERR_COUNT.fetch_add(1, Ordering::Relaxed) < 3 {
                                eprintln!("Request Error: {:?}", e);
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

    let total_tasks: u64 = shard_counters.iter().map(|c| c.n_task.load(Ordering::Relaxed)).sum();
    let total_reqs: u64 = shard_counters.iter().map(|c| c.n_req.load(Ordering::Relaxed)).sum();
    let total_pages: u64 = shard_counters.iter().map(|c| c.n_page.load(Ordering::Relaxed)).sum();
    let total_failed: u64 = shard_counters.iter().map(|c| c.n_failed.load(Ordering::Relaxed)).sum();
    let total_bytes: u64 = shard_counters.iter().map(|c| c.n_bytes.load(Ordering::Relaxed)).sum();
    let sustained_qps = total_pages as f64 / elapsed;

    println!("\n============================================================");
    println!("  PAGE SIFT 0.60 100M SHARDED BENCHMARK SUMMARY");
    println!("============================================================");
    println!("  Wall Clock Time:    {:.2}s ({:.2} minutes)", elapsed, elapsed / 60.0);
    println!("  Tasks Completed:    {} / {}", total_tasks, opts.total_tasks);
    println!("  HTTP Requests:      {}", total_reqs);
    println!("  Valid Markdown:     {} ({:.4}%)", total_pages, (total_pages as f64 / total_tasks as f64) * 100.0);
    println!("  Failed Count:       {}", total_failed);
    println!("  Sustained QPS:      {:.2} pages/sec", sustained_qps);
    println!("  Throughput Volume:  {:.2} MB/s ({:.2} GB total)",
        (total_bytes as f64 / 1_048_576.0) / elapsed,
        total_bytes as f64 / 1_073_741_824.0
    );
    println!("  Final Peak RSS:     {:.1} MB", get_current_rss_mb());
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
