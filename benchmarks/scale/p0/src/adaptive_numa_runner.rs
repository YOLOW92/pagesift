use clap::Parser;
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use pagesift_policy::adaptive::{
    AdaptiveCapacityController, CapacitySample, CpuSaturationTracker, HardwareTopology,
    QuantileLatencyTracker, pin_current_thread,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Parser)]
#[command(about = "NUMA-local, two-dimensional adaptive PageSift runner")]
struct Opts {
    #[arg(long, default_value = "http://127.0.0.1:19800")]
    origin_url: String,
    #[arg(long, default_value_t = 100_000_000)]
    total_tasks: u64,
    #[arg(long, default_value_t = 1_000)]
    telemetry_interval_ms: u64,
    #[arg(long, default_value_t = 256)]
    queue_sample_every: u64,
}

const TASK_BLOCK_SIZE: u64 = 256;

#[repr(align(128))]
#[derive(Default)]
struct WorkerCounters {
    completed: AtomicU64,
    pages: AtomicU64,
    failed: AtomicU64,
    bytes: AtomicU64,
}

struct Shared {
    next_task_block: AtomicU64,
    total_tasks: u64,
    active_workers: AtomicUsize,
    total_concurrency: AtomicUsize,
    running: AtomicBool,
    worker_counters: Vec<WorkerCounters>,
    queue_latency: QuantileLatencyTracker,
    queue_sample_every: u64,
    origin_url: String,
}

#[derive(Debug, Clone, Copy, Default)]
struct CounterSnapshot {
    completed: u64,
    pages: u64,
    failed: u64,
    bytes: u64,
}

impl Shared {
    fn counters(&self) -> CounterSnapshot {
        self.worker_counters
            .iter()
            .fold(CounterSnapshot::default(), |mut total, counters| {
                total.completed += counters.completed.load(Ordering::Relaxed);
                total.pages += counters.pages.load(Ordering::Relaxed);
                total.failed += counters.failed.load(Ordering::Relaxed);
                total.bytes += counters.bytes.load(Ordering::Relaxed);
                total
            })
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = Opts::parse();
    let topology = HardwareTopology::detect();
    let worker_cpus = topology.cpu_topology.runtime_worker_cpus();
    let controller = AdaptiveCapacityController::from_topology(&topology);
    let initial = controller.decision();
    let shared = Arc::new(Shared {
        next_task_block: AtomicU64::new(0),
        total_tasks: opts.total_tasks,
        active_workers: AtomicUsize::new(initial.active_workers),
        total_concurrency: AtomicUsize::new(initial.total_concurrency),
        running: AtomicBool::new(true),
        worker_counters: (0..worker_cpus.len())
            .map(|_| WorkerCounters::default())
            .collect(),
        queue_latency: QuantileLatencyTracker::new(2048),
        queue_sample_every: opts.queue_sample_every,
        origin_url: opts.origin_url,
    });

    println!("============================================================");
    println!("  PageSift P2 Adaptive NUMA Runner");
    println!(
        "  Topology: {} logical / {} physical / {} sockets / {} NUMA",
        topology.logical_cores, topology.physical_cores, topology.sockets, topology.numa_nodes
    );
    println!(
        "  Search space: {}..{} active workers / {}..{} concurrency",
        initial.active_workers,
        worker_cpus.len(),
        topology.min_concurrency,
        topology.max_concurrency
    );
    println!("  Data plane: local counters / {TASK_BLOCK_SIZE} task block");
    println!("============================================================");

    let mut workers = Vec::with_capacity(worker_cpus.len());
    for (rank, cpu_id) in worker_cpus.into_iter().enumerate() {
        let shared = shared.clone();
        workers.push(
            std::thread::Builder::new()
                .name(format!("p2-numa-{rank}"))
                .spawn(move || {
                    if !pin_current_thread(cpu_id) {
                        return Err(format!("failed to pin worker {rank} to CPU {cpu_id}"));
                    }
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| error.to_string())?
                        .block_on(worker_loop(rank, shared))
                        .map_err(|error| error.to_string())
                })?,
        );
    }

    let started = Instant::now();
    let interval = Duration::from_millis(opts.telemetry_interval_ms.max(100));
    let cpu = CpuSaturationTracker::with_assigned_cores(topology.worker_cores);
    let mut last_completed = 0u64;
    let mut last_failed = 0u64;
    let mut sampled_at = Instant::now();
    while shared.counters().completed < opts.total_tasks {
        std::thread::sleep(interval);
        let applied = controller.decision();
        let counters = shared.counters();
        let completed = counters.completed;
        let failed = counters.failed;
        let elapsed = sampled_at.elapsed().as_secs_f64().max(0.001);
        let delta = completed.saturating_sub(last_completed);
        let delta_failed = failed.saturating_sub(last_failed);
        let qps = delta as f64 / elapsed;
        let error_rate_pct = if delta == 0 {
            0.0
        } else {
            delta_failed as f64 / delta as f64 * 100.0
        };
        let (_, p95, p99) = shared.queue_latency.quantiles();
        let process_cpu = cpu.sample();
        let decision = controller.evaluate(CapacitySample {
            qps,
            queue_p95_ms: p95,
            error_rate_pct,
            process_cpu_pct: process_cpu.utilization_pct,
        });
        shared
            .active_workers
            .store(decision.active_workers, Ordering::Release);
        shared
            .total_concurrency
            .store(decision.total_concurrency, Ordering::Release);
        println!(
            "[{:.2}%] QPS {:>9.1} | workers {:>3} | C* {:>5} | CPU {:>5.1}% of {} physical | queue p95 {:.3} p99 {:.3} ms | failed {} | {:?}",
            completed as f64 / opts.total_tasks as f64 * 100.0,
            qps,
            applied.active_workers,
            applied.total_concurrency,
            process_cpu.utilization_pct,
            topology.worker_cores,
            p95,
            p99,
            failed,
            applied.phase,
        );
        if decision != applied {
            println!(
                "  -> next: {} workers / {} concurrency / {:?}",
                decision.active_workers, decision.total_concurrency, decision.phase
            );
        }
        last_completed = completed;
        last_failed = failed;
        sampled_at = Instant::now();
    }
    shared.running.store(false, Ordering::Release);

    for worker in workers {
        match worker.join() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => return Err("adaptive NUMA worker panicked".into()),
        }
    }

    let wall = started.elapsed().as_secs_f64();
    let counters = shared.counters();
    let pages = counters.pages;
    let failed = counters.failed;
    let bytes = counters.bytes;
    let final_decision = controller.decision();
    let final_cpu = cpu.sample();
    let rss_mb = HardwareTopology::read_process_rss() as f64 / 1_048_576.0;
    println!("============================================================");
    println!("  Completed: {} / {}", pages, opts.total_tasks);
    println!("  Failed: {failed}");
    println!("  Sustained QPS: {:.2}", pages as f64 / wall.max(0.001));
    println!(
        "  Selected: {} workers / {} concurrency / {:?}",
        final_decision.active_workers, final_decision.total_concurrency, final_decision.phase
    );
    println!(
        "  Throughput: {:.2} MB/s",
        bytes as f64 / 1_048_576.0 / wall
    );
    println!("  Process CPU time: {:.2} s", final_cpu.process_cpu_time_s);
    println!("  Peak/final RSS snapshot: {:.2} MiB", rss_mb);
    println!("  Worker directional measurements:");
    for measurement in controller.worker_measurements() {
        println!(
            "    {:>3} workers | n {:>2} | mean {:>9.1} QPS | stddev {:>7.1}",
            measurement.active_workers,
            measurement.samples,
            measurement.mean_qps,
            measurement.stddev_qps,
        );
    }
    println!("  Concurrency directional measurements:");
    for measurement in controller.concurrency_measurements() {
        println!(
            "    C* {:>5} | n {:>2} | mean {:>9.1} QPS | stddev {:>7.1} | queue p95 {:>6.3} ms | CPU {:>5.1}%",
            measurement.total_concurrency,
            measurement.samples,
            measurement.mean_qps,
            measurement.stddev_qps,
            measurement.mean_queue_p95_ms,
            measurement.mean_process_cpu_pct,
        );
    }
    println!("============================================================");
    Ok(())
}

async fn worker_loop(rank: usize, shared: Arc<Shared>) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(32)
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_nodelay(true)
        .build()?;
    let client = &client;
    let counters = &shared.worker_counters[rank];
    let queue_latency = &shared.queue_latency;
    let origin_url = &shared.origin_url;
    let mut inflight = FuturesUnordered::new();
    let mut block_next = 0u64;
    let mut block_end = 0u64;

    loop {
        let active_workers = shared.active_workers.load(Ordering::Acquire).max(1);
        let active = rank < active_workers;
        let total_concurrency = shared.total_concurrency.load(Ordering::Acquire).max(1);
        // Keep the hot request path NUMA-local. The controller changes this
        // exact global budget automatically, while each runtime owns its local
        // share without a cross-socket atomic operation per request.
        let base_limit = total_concurrency / active_workers;
        let remainder = total_concurrency % active_workers;
        let local_limit = base_limit + usize::from(rank < remainder);

        while shared.running.load(Ordering::Acquire)
            && (active || block_next < block_end)
            && local_limit > 0
            && inflight.len() < local_limit
        {
            if block_next >= block_end {
                if !active {
                    break;
                }
                block_next = shared
                    .next_task_block
                    .fetch_add(TASK_BLOCK_SIZE, Ordering::Relaxed);
                block_end = block_next
                    .saturating_add(TASK_BLOCK_SIZE)
                    .min(shared.total_tasks);
                if block_next >= block_end {
                    break;
                }
            }
            let task_id = block_next;
            block_next += 1;
            let sample_queue =
                shared.queue_sample_every > 0 && task_id % shared.queue_sample_every == 0;
            let queued_at = Instant::now();
            inflight.push(async move {
                if sample_queue {
                    queue_latency.record(queued_at.elapsed().as_secs_f64() * 1_000.0);
                }
                let url = format!("{origin_url}/p/{task_id}");
                let outcome = async {
                    let body = client.get(&url).send().await?.bytes().await?;
                    let text = std::str::from_utf8(&body)
                        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;
                    let options = pagesift_content::Options {
                        base_url: url::Url::parse(&url).ok(),
                        ..Default::default()
                    };
                    let document = pagesift_content::process(text, &options);
                    if document.markdown.is_empty() {
                        return Err::<usize, Box<dyn std::error::Error>>("empty Markdown".into());
                    }
                    Ok(body.len())
                }
                .await;
                match outcome {
                    Ok(bytes) => {
                        counters.pages.fetch_add(1, Ordering::Relaxed);
                        counters.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
                    }
                    Err(_) => {
                        counters.failed.fetch_add(1, Ordering::Relaxed);
                    }
                }
                counters.completed.fetch_add(1, Ordering::Release);
            });
        }

        if let Some(()) = inflight.next().await {
            continue;
        }
        if block_next >= block_end
            && shared.next_task_block.load(Ordering::Relaxed) >= shared.total_tasks
        {
            break;
        }
        if !shared.running.load(Ordering::Acquire) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}
