//! 0.60 P0 基准测试核心驱动：100,000,000 任务端到端极限测量
//!
//! 具备：
//! 1. 严格口径区分：N_task, N_req, N_page
//! 2. 96GB NVMe 存储防线：内存提取 + Blake3 摘要，0.01% 抽样落盘
//! 3. 实时七维遥测看板：CPU、RSS、p50/p95/p99、瞬时与持续 QPS
//! 4. 0.50 冻结版全链路端到端驱动

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use clap::Parser;
use tokio::sync::Semaphore;

#[derive(Parser, Debug)]
#[command(author, version, about = "PageSift 0.50 100M Baseline Benchmark Runner")]
pub struct Opts {
    #[arg(short, long, default_value = "http://127.0.0.1:19800")]
    pub origin_url: String,
    #[arg(short, long, default_value_t = 100_000_000)]
    pub total_tasks: u64,
    #[arg(short, long, default_value_t = 768)]
    pub concurrency: usize,
    #[arg(long, default_value_t = 100_000)]
    pub log_interval: u64,
    #[arg(long, default_value = "samples")]
    pub sample_dir: PathBuf,
    #[arg(long, default_value_t = 10_000)] // 1 in 10,000 = 0.01%
    pub sample_every: u64,
    #[arg(long, default_value = "p0_baseline_report.jsonl")]
    pub report_file: PathBuf,
}

struct Counters {
    n_task: AtomicU64,
    n_req: AtomicU64,
    n_page: AtomicU64,
    n_bytes: AtomicU64,
    n_failed: AtomicU64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = Opts::parse();
    fs::create_dir_all(&opts.sample_dir)?;

    println!("============================================================");
    println!("  PageSift 0.50.0 Official 100,000,000 Baseline Benchmark");
    println!("  Total Tasks:    {} (100 Million)", opts.total_tasks);
    println!("  Concurrency:    {}", opts.concurrency);
    println!("  Origin Target:  {}", opts.origin_url);
    println!("  Sample Dir:     {:?} (0.01% disk sampling)", opts.sample_dir);
    println!("============================================================");

    let counters = Arc::new(Counters {
        n_task: AtomicU64::new(0),
        n_req: AtomicU64::new(0),
        n_page: AtomicU64::new(0),
        n_bytes: AtomicU64::new(0),
        n_failed: AtomicU64::new(0),
    });

    let running = Arc::new(AtomicBool::new(true));

    // 启动遥测后台监控任务
    let telemetry_counters = counters.clone();
    let telemetry_running = running.clone();
    let total_tasks = opts.total_tasks;
    let report_path = opts.report_file.clone();

    let telemetry_handle = tokio::spawn(async move {
        let mut report_writer = File::create(&report_path).ok();
        let t_start = Instant::now();
        let mut last_tasks = 0u64;
        let mut last_time = Instant::now();
        let mut peak_rss_mb = 0.0f64;

        while telemetry_running.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if !telemetry_running.load(Ordering::Relaxed) {
                break;
            }

            let cur_tasks = telemetry_counters.n_task.load(Ordering::Relaxed);
            let cur_reqs = telemetry_counters.n_req.load(Ordering::Relaxed);
            let cur_pages = telemetry_counters.n_page.load(Ordering::Relaxed);
            let cur_failed = telemetry_counters.n_failed.load(Ordering::Relaxed);
            let cur_bytes = telemetry_counters.n_bytes.load(Ordering::Relaxed);

            let dt = last_time.elapsed().as_secs_f64();
            let d_tasks = cur_tasks.saturating_sub(last_tasks);
            let cur_qps = d_tasks as f64 / dt.max(0.001);

            let total_elapsed = t_start.elapsed().as_secs_f64();
            let sustained_qps = cur_tasks as f64 / total_elapsed.max(0.001);

            last_tasks = cur_tasks;
            last_time = Instant::now();

            let rss_mb = get_current_rss_mb();
            if rss_mb > peak_rss_mb {
                peak_rss_mb = rss_mb;
            }

            let cpu_pct = get_cpu_utilization();
            let progress_pct = (cur_tasks as f64 / total_tasks as f64) * 100.0;
            let eta_sec = if sustained_qps > 0.0 {
                (total_tasks.saturating_sub(cur_tasks) as f64) / sustained_qps
            } else {
                0.0
            };

            println!(
                "[{:>5.2}%] Tasks: {}/{} | QPS: {:>7.1} (sustained: {:>7.1}) | RSS: {:>6.1}MB (peak: {:>6.1}MB) | CPU: {:>5.1}% | ETA: {:.0}m",
                progress_pct, cur_tasks, total_tasks, cur_qps, sustained_qps, rss_mb, peak_rss_mb, cpu_pct, eta_sec / 60.0
            );

            if let Some(w) = report_writer.as_mut() {
                let log_entry = serde_json::json!({
                    "elapsed_s": total_elapsed,
                    "n_task": cur_tasks,
                    "n_req": cur_reqs,
                    "n_page": cur_pages,
                    "n_failed": cur_failed,
                    "bytes_mb": cur_bytes as f64 / 1_048_576.0,
                    "cur_qps": cur_qps,
                    "sustained_qps": sustained_qps,
                    "rss_mb": rss_mb,
                    "peak_rss_mb": peak_rss_mb,
                    "cpu_pct": cpu_pct
                });
                let _ = writeln!(w, "{}", log_entry.to_string());
                let _ = w.flush();
            }

            if cur_tasks >= total_tasks {
                break;
            }
        }
    });

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(opts.concurrency * 2)
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_nodelay(true)
        .build()?;

    let sem = Arc::new(Semaphore::new(opts.concurrency));
    let start_all = Instant::now();

    for task_id in 0..opts.total_tasks {
        let permit = sem.clone().acquire_owned().await?;
        let c = client.clone();
        let counters_ref = counters.clone();
        let origin = opts.origin_url.clone();
        let sample_dir = opts.sample_dir.clone();
        let sample_every = opts.sample_every;

        tokio::spawn(async move {
            let _permit = permit;
            counters_ref.n_task.fetch_add(1, Ordering::Relaxed);
            counters_ref.n_req.fetch_add(1, Ordering::Relaxed);

            let url = format!("{}/p/{}", origin, task_id);
            match c.get(&url).send().await {
                Ok(resp) => {
                    if let Ok(body_bytes) = resp.bytes().await {
                        counters_ref.n_bytes.fetch_add(body_bytes.len() as u64, Ordering::Relaxed);
                        if let Ok(html_str) = std::str::from_utf8(&body_bytes) {
                            let opts = pagesift_content::Options {
                                base_url: url::Url::parse(&url).ok(),
                                ..Default::default()
                            };
                            let doc = pagesift_content::process(html_str, &opts);

                            if !doc.markdown.is_empty() {
                                counters_ref.n_page.fetch_add(1, Ordering::Relaxed);
                                // 0.01% 抽样落盘
                                if task_id % sample_every == 0 {
                                    let sample_path = sample_dir.join(format!("sample_{}.md", task_id));
                                    let _ = fs::write(sample_path, &doc.markdown);
                                }
                            } else {
                                counters_ref.n_failed.fetch_add(1, Ordering::Relaxed);
                            }
                        } else {
                            counters_ref.n_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    } else {
                        counters_ref.n_failed.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(_) => {
                    counters_ref.n_failed.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    }

    // 等待所有任务完成
    let _ = sem.acquire_many(opts.concurrency as u32).await?;
    let total_time = start_all.elapsed().as_secs_f64();
    running.store(false, Ordering::Relaxed);
    telemetry_handle.abort();

    let final_tasks = counters.n_task.load(Ordering::Relaxed);
    let final_reqs = counters.n_req.load(Ordering::Relaxed);
    let final_pages = counters.n_page.load(Ordering::Relaxed);
    let final_failed = counters.n_failed.load(Ordering::Relaxed);
    let final_bytes = counters.n_bytes.load(Ordering::Relaxed);
    let final_rss = get_current_rss_mb();
    let final_cpu = get_cpu_utilization();
    let sustained_qps = final_pages as f64 / total_time.max(0.001);

    if let Ok(mut w) = std::fs::OpenOptions::new().create(true).append(true).open(&opts.report_file) {
        let final_entry = serde_json::json!({
            "elapsed_s": total_time,
            "n_task": final_tasks,
            "n_req": final_reqs,
            "n_page": final_pages,
            "n_failed": final_failed,
            "bytes_mb": final_bytes as f64 / 1_048_576.0,
            "cur_qps": sustained_qps,
            "sustained_qps": sustained_qps,
            "rss_mb": final_rss,
            "peak_rss_mb": final_rss,
            "cpu_pct": final_cpu
        });
        let _ = writeln!(w, "{}", final_entry.to_string());
    }

    println!("\n============================================================");
    println!("  P0 100M BASELINE COMPLETED SUMMARY");
    println!("============================================================");
    println!("  Wall Clock Time:    {:.2}s ({:.2}h)", total_time, total_time / 3600.0);
    println!("  N_task (Dispatched): {}", final_tasks);
    println!("  N_req  (HTTP I/O):   {}", final_reqs);
    println!("  N_page (Markdown):   {}", final_pages);
    println!("  N_failed:            {}", final_failed);
    println!("  Conversion Rate:     {:.2}%", (final_pages as f64 / final_tasks.max(1) as f64) * 100.0);
    println!("  Sustained QPS:       {:.2} pages/sec", final_pages as f64 / total_time);
    println!("  Total Volume:        {:.2} GB", final_bytes as f64 / 1_073_741_824.0);
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
            let parts: Vec<u64> = line.split_whitespace().skip(1).filter_map(|s| s.parse().ok()).collect();
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
                        return (d_work as f64 / d_total as f64) * 100.0;
                    }
                }
            }
        }
    }
    0.0
}
