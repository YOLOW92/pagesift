use clap::Parser;
use pagesift_policy::adaptive::{
    AdaptiveCapacityController, AdaptiveConcurrencyGate, CapacityControllerConfig, CapacityPhase,
    CapacitySample, HardwareTopology, set_process_affinity,
};
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "PageSift 0.60 Sharded Share-Nothing 100M Runner"
)]
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
    /// Telemetry sampling interval in milliseconds.
    #[arg(long, default_value_t = 1_000)]
    pub telemetry_interval_ms: u64,
    /// Sample one semaphore queue wait out of every N tasks (0 disables sampling).
    #[arg(long, default_value_t = 256)]
    pub queue_sample_every: u64,
    /// Maximum simultaneous CPU-bound document parses (0 leaves parsing unlimited).
    #[arg(long, default_value_t = 0)]
    pub parse_parallelism: usize,
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
    let topology = HardwareTopology::detect();
    fs::create_dir_all(&opts.sample_dir)?;

    let shard_tiers = dispatch_shard_tiers(topology.numa_nodes, topology.physical_cores);
    // High-capacity start: make the complete machine available immediately,
    // then let measurements prove which resources should be removed.
    let (initial_shards, conc_per_shard) = if opts.auto {
        let s = shard_tiers.last().copied().unwrap_or(1);
        let c_total = topology.base_concurrency;
        let c_shard = c_total.div_ceil(s).max(1);
        (s, c_shard)
    } else {
        (opts.shards, opts.concurrency_per_shard)
    };

    let dispatcher_count = if opts.auto {
        shard_tiers.last().copied().unwrap_or(initial_shards)
    } else {
        initial_shards
    };
    let total_concurrency = initial_shards * conc_per_shard;
    let shard_gates = (0..dispatcher_count)
        .map(|shard| {
            let local = if shard < initial_shards {
                total_concurrency / initial_shards
                    + usize::from(shard < total_concurrency % initial_shards)
            } else {
                1
            };
            AdaptiveConcurrencyGate::new(local.max(1))
        })
        .collect::<Vec<_>>();
    let cpu_activation_tiers = Arc::new(topology.cpu_topology.worker_activation_tiers());
    let capacity_controller = opts.auto.then(|| {
        let config = CapacityControllerConfig {
            settle_samples: 1,
            min_probe_samples: 3,
            max_probe_samples: 4,
            min_concurrency: topology.min_concurrency,
            max_concurrency: topology.max_concurrency,
            initial_concurrency_step_tiers: 2,
            max_inconclusive_probes: 1,
            ..CapacityControllerConfig::default()
        };
        Arc::new(AdaptiveCapacityController::new_with_initial_worker(
            cpu_activation_tiers.iter().map(Vec::len).collect(),
            topology.logical_cores,
            topology.base_concurrency,
            config,
        ))
    });
    let initial_active_cpus = capacity_controller
        .as_ref()
        .map(|controller| controller.decision().active_workers)
        .unwrap_or(topology.logical_cores);
    if opts.auto
        && let Some(cpus) = cpu_activation_tiers
            .iter()
            .find(|tier| tier.len() == initial_active_cpus)
    {
        set_process_affinity(cpus);
    }
    let active_cpu_count = Arc::new(AtomicUsize::new(initial_active_cpus));
    let shard_controller = opts.auto.then(|| {
        let config = CapacityControllerConfig {
            settle_samples: 1,
            min_probe_samples: 3,
            max_probe_samples: 4,
            min_concurrency: 1,
            max_concurrency: 1,
            initial_worker_step_tiers: 4,
            initial_concurrency_step_tiers: 1,
            locked_reprobe_samples: usize::MAX,
            ..CapacityControllerConfig::default()
        };
        Arc::new(AdaptiveCapacityController::new_with_initial_worker(
            shard_tiers.clone(),
            initial_shards,
            1,
            config,
        ))
    });
    let active_shards = Arc::new(AtomicUsize::new(initial_shards));
    let parse_gate =
        (opts.parse_parallelism > 0).then(|| AdaptiveConcurrencyGate::new(opts.parse_parallelism));

    println!("============================================================");
    println!("  PageSift 0.60 Official 100M Sharded Benchmark Runner");
    println!("  Target Origin:       {}", opts.origin_url);
    println!("  Total Tasks:         {} (100 Million)", opts.total_tasks);
    println!(
        "  Topology Scaling:    {}",
        if opts.auto {
            "Autonomous C*(N) Scaling"
        } else {
            "Manual Topology"
        }
    );
    println!(
        "  Parallel Shards:     {} active / {} available dispatchers",
        initial_shards, dispatcher_count
    );
    println!("  Conc per Shard:      {}", conc_per_shard);
    println!("  Total Concurrency:   {}", total_concurrency);
    println!("  Active CPU Set:      {}", initial_active_cpus);
    println!(
        "  Parse Parallelism:   {}",
        parse_gate
            .as_ref()
            .map(|gate| gate.limit().to_string())
            .unwrap_or_else(|| "unlimited".to_string())
    );
    println!(
        "  CPU Topology:        {} logical / {} physical / {} sockets / {} NUMA",
        topology.logical_cores, topology.physical_cores, topology.sockets, topology.numa_nodes
    );
    println!(
        "  Runtime Workers:     {} physical-first",
        topology.runtime_workers
    );
    println!(
        "  Disk Sample Dir:     {:?} (0.01% rate, 1 in {})",
        opts.sample_dir, opts.sample_every
    );
    println!("  Telemetry Report:    {:?}", opts.report_file);
    println!("============================================================");

    // Each shard gets its own dedicated cache-line aligned atomic counter set
    let mut shard_counters: Vec<Arc<ShardCounters>> = Vec::with_capacity(dispatcher_count);
    for _ in 0..dispatcher_count {
        shard_counters.push(Arc::new(ShardCounters::default()));
    }

    let running = Arc::new(AtomicBool::new(true));
    let queue_samples_us = Arc::new(Mutex::new(Vec::new()));
    let t_running = running.clone();
    let t_counters = shard_counters.clone();
    let total_tasks_target = opts.total_tasks;
    let report_path = opts.report_file.clone();
    let telemetry_interval = Duration::from_millis(opts.telemetry_interval_ms.max(10));
    let t_shard_gates = shard_gates.clone();
    let t_controller = capacity_controller.clone();
    let t_parse_gate = parse_gate.clone();
    let t_queue_samples = queue_samples_us.clone();
    let t_cpu_activation_tiers = cpu_activation_tiers.clone();
    let t_active_cpu_count = active_cpu_count.clone();
    let t_shard_controller = shard_controller.clone();
    let t_active_shards = active_shards.clone();

    // Real-time telemetry monitor
    let telemetry_handle = tokio::spawn(async move {
        let mut report_writer = File::create(&report_path).ok();
        let t0 = Instant::now();
        let mut last_tasks = 0u64;
        let mut last_failed = 0u64;
        let mut last_time = Instant::now();
        let mut peak_rss_mb = 0.0f64;
        let mut process_cpu = ProcessCpuTracker::new();

        while t_running.load(Ordering::Relaxed) {
            tokio::time::sleep(telemetry_interval).await;
            if !t_running.load(Ordering::Relaxed) {
                break;
            }

            let cur_tasks: u64 = t_counters
                .iter()
                .map(|c| c.n_task.load(Ordering::Relaxed))
                .sum();
            let cur_reqs: u64 = t_counters
                .iter()
                .map(|c| c.n_req.load(Ordering::Relaxed))
                .sum();
            let cur_pages: u64 = t_counters
                .iter()
                .map(|c| c.n_page.load(Ordering::Relaxed))
                .sum();
            let cur_failed: u64 = t_counters
                .iter()
                .map(|c| c.n_failed.load(Ordering::Relaxed))
                .sum();
            let cur_bytes: u64 = t_counters
                .iter()
                .map(|c| c.n_bytes.load(Ordering::Relaxed))
                .sum();

            let dt = last_time.elapsed().as_secs_f64().max(0.001);
            let d_tasks = cur_tasks.saturating_sub(last_tasks);
            let d_failed = cur_failed.saturating_sub(last_failed);
            let instant_qps = d_tasks as f64 / dt;
            let error_rate_pct = if d_tasks == 0 {
                0.0
            } else {
                d_failed as f64 / d_tasks as f64 * 100.0
            };

            let total_dt = t0.elapsed().as_secs_f64().max(0.001);
            let sustained_qps = cur_tasks as f64 / total_dt;

            last_tasks = cur_tasks;
            last_failed = cur_failed;
            last_time = Instant::now();

            let rss_mb = get_current_rss_mb();
            if rss_mb > peak_rss_mb {
                peak_rss_mb = rss_mb;
            }
            let active_cpus = t_active_cpu_count.load(Ordering::Relaxed);
            let (process_cpu_pct, process_cpu_time_s) = process_cpu.sample(active_cpus);
            let queue_p95_ms = t_queue_samples
                .lock()
                .map(|samples| queue_quantiles_ms(&samples).1)
                .unwrap_or(0.0);
            let sample = CapacitySample {
                qps: instant_qps,
                queue_p95_ms,
                error_rate_pct,
                process_cpu_pct,
            };
            let (target_concurrency, parse_parallelism, active_dispatch_shards, controller_state) =
                if let Some(controller) = &t_controller {
                    let decision = controller.evaluate(sample);
                    if decision.active_workers != active_cpus
                        && let Some(cpus) = t_cpu_activation_tiers
                            .iter()
                            .find(|tier| tier.len() == decision.active_workers)
                    {
                        set_process_affinity(cpus);
                        t_active_cpu_count.store(decision.active_workers, Ordering::Relaxed);
                    }
                    let (active, shard_phase) = if decision.phase == CapacityPhase::Locked {
                        if let Some(shard_controller) = &t_shard_controller {
                            let shard_decision = shard_controller.evaluate(sample);
                            t_active_shards.store(shard_decision.active_workers, Ordering::Relaxed);
                            (
                                shard_decision.active_workers,
                                format!("/{:?}", shard_decision.phase),
                            )
                        } else {
                            (t_active_shards.load(Ordering::Relaxed), String::new())
                        }
                    } else {
                        (t_active_shards.load(Ordering::Relaxed), String::new())
                    };
                    let target = decision.total_concurrency.max(active);
                    for (shard, gate) in t_shard_gates.iter().enumerate() {
                        let local = if shard < active {
                            target / active + usize::from(shard < target % active)
                        } else {
                            1
                        };
                        gate.set_limit(local);
                    }
                    (
                        target,
                        decision.active_workers,
                        active,
                        format!("{:?}{}", decision.phase, shard_phase),
                    )
                } else {
                    (
                        t_shard_gates
                            .iter()
                            .map(AdaptiveConcurrencyGate::limit)
                            .sum(),
                        t_parse_gate
                            .as_ref()
                            .map(AdaptiveConcurrencyGate::limit)
                            .unwrap_or(0),
                        t_active_shards.load(Ordering::Relaxed),
                        "Manual".to_string(),
                    )
                };
            let pct = (cur_tasks as f64 / total_tasks_target as f64) * 100.0;
            let eta_sec = if sustained_qps > 0.0 {
                (total_tasks_target.saturating_sub(cur_tasks) as f64) / sustained_qps
            } else {
                0.0
            };

            println!(
                "[{:>5.2}%] Tasks: {:>8}/{} | Instant: {:>8.1} p/s (Sustained: {:>8.1}) | IO: {:>4} CPU: {:>3} S: {:>2} ({}) | RSS: {:>5.1}MB (peak: {:>5.1}MB) | Runner CPU: {:>4.1}% | ETA: {:>4.1}m",
                pct,
                cur_tasks,
                total_tasks_target,
                instant_qps,
                sustained_qps,
                target_concurrency,
                parse_parallelism,
                active_dispatch_shards,
                controller_state,
                rss_mb,
                peak_rss_mb,
                process_cpu_pct,
                eta_sec / 60.0
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
                    "process_cpu_pct": process_cpu_pct,
                    "process_cpu_time_s": process_cpu_time_s,
                    "assigned_cores": active_cpus
                    ,"adaptive_concurrency": target_concurrency
                    ,"adaptive_parse_parallelism": parse_parallelism
                    ,"adaptive_shards": active_dispatch_shards
                    ,"controller_state": controller_state
                });
                let _ = writeln!(w, "{}", log_entry.to_string());
                let _ = w.flush();
            }

            if cur_tasks >= total_tasks_target {
                break;
            }
        }
    });

    let benchmark_cpu_start_ticks = read_process_cpu_ticks().unwrap_or(0);
    let start_all = Instant::now();
    let mut shard_handles = Vec::with_capacity(dispatcher_count);
    let next_task = Arc::new(AtomicU64::new(0));
    const TASK_BLOCK: u64 = 4096;

    for shard_id in 0..dispatcher_count {
        let shard_client = reqwest::Client::builder()
            .pool_max_idle_per_host(conc_per_shard * 2)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_nodelay(true)
            .build()?;

        let counters = shard_counters[shard_id].clone();
        let origin = opts.origin_url.clone();
        let gate = shard_gates[shard_id].clone();
        let parse_gate = parse_gate.clone();
        let allocator = next_task.clone();
        let dispatcher_limit = active_shards.clone();
        let total_tasks_target = opts.total_tasks;
        let sample_dir = opts.sample_dir.clone();
        let sample_every = opts.sample_every;
        let queue_sample_every = opts.queue_sample_every;
        let queue_samples = queue_samples_us.clone();

        let handle = tokio::spawn(async move {
            loop {
                if allocator.load(Ordering::Relaxed) >= total_tasks_target {
                    break;
                }
                if shard_id >= dispatcher_limit.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    continue;
                }
                let block_start = allocator.fetch_add(TASK_BLOCK, Ordering::Relaxed);
                if block_start >= total_tasks_target {
                    break;
                }
                let block_end = (block_start + TASK_BLOCK).min(total_tasks_target);
                for task_id in block_start..block_end {
                    let queued_at = Instant::now();
                    let permit = gate.acquire().await;
                    if queue_sample_every > 0 && task_id % queue_sample_every == 0 {
                        let wait_us = queued_at.elapsed().as_micros().min(u64::MAX as u128) as u64;
                        if let Ok(mut samples) = queue_samples.lock() {
                            samples.push(wait_us);
                        }
                    }
                    let c_inner = shard_client.clone();
                    let cnt_inner = counters.clone();
                    let url = format!("{}/p/{}", origin, task_id);
                    let s_dir = sample_dir.clone();
                    let task_parse_gate = parse_gate.clone();

                    tokio::spawn(async move {
                        let _permit = permit;
                        cnt_inner.n_task.fetch_add(1, Ordering::Relaxed);
                        cnt_inner.n_req.fetch_add(1, Ordering::Relaxed);

                        match c_inner.get(&url).send().await {
                            Ok(resp) => {
                                if let Ok(body) = resp.bytes().await {
                                    cnt_inner
                                        .n_bytes
                                        .fetch_add(body.len() as u64, Ordering::Relaxed);
                                    if let Ok(s) = std::str::from_utf8(&body) {
                                        let _parse_permit = if let Some(gate) = &task_parse_gate {
                                            Some(gate.acquire().await)
                                        } else {
                                            None
                                        };
                                        let options = pagesift_content::Options {
                                            base_url: url::Url::parse(&url).ok(),
                                            ..Default::default()
                                        };
                                        let doc = pagesift_content::process(s, &options);
                                        if !doc.markdown.is_empty() {
                                            cnt_inner.n_page.fetch_add(1, Ordering::Relaxed);
                                            if task_id % sample_every == 0 {
                                                let sample_path =
                                                    s_dir.join(format!("sample_{}.md", task_id));
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
            }
            gate.wait_idle().await;
        });
        shard_handles.push(handle);
    }

    for h in shard_handles {
        let _ = h.await;
    }

    let elapsed = start_all.elapsed().as_secs_f64();
    let benchmark_cpu_ticks = read_process_cpu_ticks()
        .unwrap_or(benchmark_cpu_start_ticks)
        .saturating_sub(benchmark_cpu_start_ticks);
    let benchmark_cpu_time_s = benchmark_cpu_ticks as f64 / clock_ticks_per_second();
    let final_active_cpus = active_cpu_count.load(Ordering::Relaxed);
    let benchmark_cpu_util_pct =
        (benchmark_cpu_time_s / (elapsed.max(1e-6) * final_active_cpus as f64) * 100.0)
            .clamp(0.0, 100.0);
    running.store(false, Ordering::Relaxed);
    let _ = telemetry_handle.await;

    let total_tasks: u64 = shard_counters
        .iter()
        .map(|c| c.n_task.load(Ordering::Relaxed))
        .sum();
    let total_reqs: u64 = shard_counters
        .iter()
        .map(|c| c.n_req.load(Ordering::Relaxed))
        .sum();
    let total_pages: u64 = shard_counters
        .iter()
        .map(|c| c.n_page.load(Ordering::Relaxed))
        .sum();
    let total_failed: u64 = shard_counters
        .iter()
        .map(|c| c.n_failed.load(Ordering::Relaxed))
        .sum();
    let total_bytes: u64 = shard_counters
        .iter()
        .map(|c| c.n_bytes.load(Ordering::Relaxed))
        .sum();
    let sustained_qps = total_pages as f64 / elapsed;
    let (queue_p50_ms, queue_p95_ms, queue_p99_ms, queue_sample_count) = queue_samples_us
        .lock()
        .map(|samples| queue_quantiles_ms(&samples))
        .unwrap_or((0.0, 0.0, 0.0, 0));

    println!("\n============================================================");
    println!("  PAGE SIFT 0.60 100M SHARDED BENCHMARK SUMMARY");
    println!("============================================================");
    println!(
        "  Wall Clock Time:    {:.2}s ({:.2} minutes)",
        elapsed,
        elapsed / 60.0
    );
    println!(
        "  Tasks Completed:    {} / {}",
        total_tasks, opts.total_tasks
    );
    println!("  HTTP Requests:      {}", total_reqs);
    println!(
        "  Valid Markdown:     {} ({:.4}%)",
        total_pages,
        (total_pages as f64 / total_tasks as f64) * 100.0
    );
    println!("  Failed Count:       {}", total_failed);
    println!("  Sustained QPS:      {:.2} pages/sec", sustained_qps);
    println!(
        "  Runner CPU Time:    {:.6} s | Utilization {:.3}% of {} assigned cores",
        benchmark_cpu_time_s, benchmark_cpu_util_pct, final_active_cpus
    );
    println!(
        "  Queue Latency:      p50 {:.3} ms | p95 {:.3} ms | p99 {:.3} ms ({} samples)",
        queue_p50_ms, queue_p95_ms, queue_p99_ms, queue_sample_count
    );
    println!(
        "  Throughput Volume:  {:.2} MB/s ({:.2} GB total)",
        (total_bytes as f64 / 1_048_576.0) / elapsed,
        total_bytes as f64 / 1_073_741_824.0
    );
    println!("  Final Peak RSS:     {:.1} MB", get_current_rss_mb());
    println!("============================================================");

    Ok(())
}

fn dispatch_shard_tiers(numa_nodes: usize, physical_cores: usize) -> Vec<usize> {
    let stride = numa_nodes.max(1);
    let ceiling = physical_cores.min(256).max(stride);
    let mut tiers = (stride..=ceiling).step_by(stride).collect::<Vec<_>>();
    if tiers.last().copied() != Some(ceiling) {
        tiers.push(ceiling);
    }
    tiers.sort_unstable();
    tiers.dedup();
    tiers
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

struct ProcessCpuTracker {
    previous_ticks: u64,
    previous_at: Instant,
    ticks_per_second: f64,
}

impl ProcessCpuTracker {
    fn new() -> Self {
        Self {
            previous_ticks: read_process_cpu_ticks().unwrap_or(0),
            previous_at: Instant::now(),
            ticks_per_second: clock_ticks_per_second(),
        }
    }

    fn sample(&mut self, assigned_cores: usize) -> (f64, f64) {
        let now = Instant::now();
        let current_ticks = read_process_cpu_ticks().unwrap_or(self.previous_ticks);
        let elapsed = now.duration_since(self.previous_at).as_secs_f64().max(1e-6);
        let delta_ticks = current_ticks.saturating_sub(self.previous_ticks);
        self.previous_ticks = current_ticks;
        self.previous_at = now;

        let cpu_seconds = delta_ticks as f64 / self.ticks_per_second;
        let capacity_seconds = elapsed * assigned_cores.max(1) as f64;
        let utilization = (cpu_seconds / capacity_seconds * 100.0).clamp(0.0, 100.0);
        let total_cpu_seconds = current_ticks as f64 / self.ticks_per_second;
        (utilization, total_cpu_seconds)
    }
}

fn read_process_cpu_ticks() -> Option<u64> {
    let stat = fs::read_to_string("/proc/self/stat").ok()?;
    parse_process_cpu_ticks(&stat)
}

fn parse_process_cpu_ticks(stat: &str) -> Option<u64> {
    let (_, fields) = stat.rsplit_once(") ")?;
    let fields = fields.split_whitespace().collect::<Vec<_>>();
    let user_ticks = fields.get(11)?.parse::<u64>().ok()?;
    let system_ticks = fields.get(12)?.parse::<u64>().ok()?;
    Some(user_ticks.saturating_add(system_ticks))
}

fn clock_ticks_per_second() -> f64 {
    Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|ticks| *ticks > 0.0)
        .unwrap_or(100.0)
}

fn queue_quantiles_ms(samples_us: &[u64]) -> (f64, f64, f64, usize) {
    if samples_us.is_empty() {
        return (0.0, 0.0, 0.0, 0);
    }
    let mut sorted = samples_us.to_vec();
    sorted.sort_unstable();
    let value = |percentile: usize| {
        let index = ((sorted.len() - 1) * percentile) / 100;
        sorted[index] as f64 / 1000.0
    };
    (value(50), value(95), value(99), sorted.len())
}

#[cfg(test)]
mod tests {
    use super::{dispatch_shard_tiers, parse_process_cpu_ticks, queue_quantiles_ms};

    #[test]
    fn shard_tiers_follow_numa_stride_and_keep_a_bounded_ceiling() {
        let tiers = dispatch_shard_tiers(4, 96);
        assert_eq!(tiers.first(), Some(&4));
        assert!(tiers.contains(&12));
        assert_eq!(tiers.last(), Some(&96));
    }

    #[test]
    fn queue_quantiles_are_reported_in_milliseconds() {
        let samples = [100, 200, 300, 400, 500];
        assert_eq!(queue_quantiles_ms(&samples), (0.3, 0.4, 0.4, 5));
    }

    #[test]
    fn parses_process_user_and_system_ticks_after_complex_comm() {
        let stat = "123 (runner ) worker) R 1 2 3 4 5 6 7 8 9 10 120 30";
        assert_eq!(parse_process_cpu_ticks(stat), Some(150));
    }
}
