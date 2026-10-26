use clap::Parser;
use pagesift_policy::adaptive::{
    AdaptiveCapacityController, AdaptiveConcurrencyGate, CapacityControllerConfig, CapacityPhase,
    CapacitySample, CpuSaturationTracker, HardwareTopology, LocalityCluster,
    QuantileLatencyTracker, pin_current_thread, pin_current_thread_to_set,
};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Parser)]
#[command(about = "Hierarchical NUMA-local adaptive PageSift runner")]
struct Opts {
    #[arg(long, default_value = "http://127.0.0.1:19800")]
    origin_url: String,
    #[arg(long, default_value_t = 100_000_000)]
    total_tasks: u64,
    #[arg(long, default_value_t = 1_000)]
    telemetry_interval_ms: u64,
    #[arg(long, default_value_t = 4096)]
    queue_sample_every: u64,
}

const TASK_BLOCK: u64 = 4096;

#[repr(align(128))]
#[derive(Default)]
struct NodeCounters {
    completed: AtomicU64,
    pages: AtomicU64,
    failed: AtomicU64,
    bytes: AtomicU64,
}

struct Shared {
    next_task: AtomicU64,
    total_tasks: u64,
    active_cpu_workers: AtomicUsize,
    total_concurrency: AtomicUsize,
    active_shards: AtomicUsize,
    running: AtomicBool,
    counters: Vec<NodeCounters>,
    queue_latency: QuantileLatencyTracker,
    queue_sample_every: u64,
    origin_url: String,
}

struct ParseJob {
    body: bytes::Bytes,
    url: String,
    result: tokio::sync::oneshot::Sender<Result<usize, String>>,
}

struct CpuQueue {
    jobs: Mutex<VecDeque<ParseJob>>,
    ready: Condvar,
    shutdown: AtomicBool,
}

#[derive(Clone)]
struct CpuSubmitter {
    queue: Arc<CpuQueue>,
}

impl CpuSubmitter {
    fn submit(&self, job: ParseJob) {
        self.queue.jobs.lock().unwrap().push_back(job);
        self.queue.ready.notify_all();
    }
}

#[derive(Default, Clone, Copy)]
struct Snapshot {
    completed: u64,
    pages: u64,
    failed: u64,
    bytes: u64,
}

impl Shared {
    fn snapshot(&self) -> Snapshot {
        self.counters
            .iter()
            .fold(Snapshot::default(), |mut sum, node| {
                sum.completed += node.completed.load(Ordering::Relaxed);
                sum.pages += node.pages.load(Ordering::Relaxed);
                sum.failed += node.failed.load(Ordering::Relaxed);
                sum.bytes += node.bytes.load(Ordering::Relaxed);
                sum
            })
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = Opts::parse();
    let topology = HardwareTopology::detect();
    let node_count = topology.cpu_topology.clusters.len().max(1);
    let initial_cpu = topology.logical_cores;
    let initial_concurrency = topology.base_concurrency;
    let shard_tiers = shard_tiers(node_count, topology.physical_cores);
    let initial_shards = shard_tiers.last().copied().unwrap_or(node_count);

    let capacity_config = CapacityControllerConfig {
        settle_samples: 1,
        min_probe_samples: 3,
        max_probe_samples: 4,
        min_concurrency: topology.min_concurrency,
        max_concurrency: topology.max_concurrency,
        initial_concurrency_step_tiers: 2,
        max_inconclusive_probes: 1,
        ..CapacityControllerConfig::default()
    };
    let cpu_tiers = topology
        .cpu_topology
        .worker_activation_tiers()
        .into_iter()
        .map(|tier| tier.len())
        .collect();
    let capacity = AdaptiveCapacityController::new_with_initial_worker(
        cpu_tiers,
        initial_cpu,
        initial_concurrency,
        capacity_config,
    );
    let shard_config = CapacityControllerConfig {
        settle_samples: 1,
        min_probe_samples: 3,
        max_probe_samples: 4,
        min_concurrency: 1,
        max_concurrency: 1,
        initial_worker_step_tiers: 4,
        max_inconclusive_probes: 1,
        locked_reprobe_samples: usize::MAX,
        ..CapacityControllerConfig::default()
    };
    let shard_controller = AdaptiveCapacityController::new_with_initial_worker(
        shard_tiers,
        initial_shards,
        1,
        shard_config,
    );

    let shared = Arc::new(Shared {
        next_task: AtomicU64::new(0),
        total_tasks: opts.total_tasks,
        active_cpu_workers: AtomicUsize::new(initial_cpu),
        total_concurrency: AtomicUsize::new(initial_concurrency),
        active_shards: AtomicUsize::new(initial_shards),
        running: AtomicBool::new(true),
        counters: (0..node_count).map(|_| NodeCounters::default()).collect(),
        queue_latency: QuantileLatencyTracker::new(4096),
        queue_sample_every: opts.queue_sample_every,
        origin_url: opts.origin_url,
    });

    println!("============================================================");
    println!("  PageSift Hierarchical NUMA Adaptive Runner");
    println!(
        "  Topology: {} logical / {} physical / {} sockets / {} NUMA",
        topology.logical_cores, topology.physical_cores, topology.sockets, topology.numa_nodes
    );
    println!(
        "  Initial: {} CPU workers / {} concurrency / {} shards",
        initial_cpu, initial_concurrency, initial_shards
    );
    for (index, cluster) in topology.cpu_topology.clusters.iter().enumerate() {
        println!(
            "  NUMA {index}: socket {} node {} / {} physical + {} SMT",
            cluster.socket_id,
            cluster.numa_node,
            cluster.physical_cpus.len(),
            cluster.smt_siblings.len()
        );
    }
    println!("============================================================");

    let started = Instant::now();
    let mut node_threads = Vec::with_capacity(node_count);
    for (node_index, cluster) in topology
        .cpu_topology
        .clusters
        .clone()
        .into_iter()
        .enumerate()
    {
        let shared = shared.clone();
        node_threads.push(
            std::thread::Builder::new()
                .name(format!("pagesift-numa-{node_index}"))
                .spawn(move || run_node(node_index, node_count, cluster, shared))?,
        );
    }

    let cpu = CpuSaturationTracker::with_assigned_cores(topology.logical_cores);
    let interval = Duration::from_millis(opts.telemetry_interval_ms.max(100));
    let mut last = Snapshot::default();
    let mut sampled_at = Instant::now();
    while shared.snapshot().completed < opts.total_tasks {
        std::thread::sleep(interval);
        let snapshot = shared.snapshot();
        let elapsed = sampled_at.elapsed().as_secs_f64().max(0.001);
        let delta = snapshot.completed.saturating_sub(last.completed);
        let delta_failed = snapshot.failed.saturating_sub(last.failed);
        let qps = delta as f64 / elapsed;
        let error_rate_pct = if delta == 0 {
            0.0
        } else {
            delta_failed as f64 / delta as f64 * 100.0
        };
        let (_, queue_p95, queue_p99) = shared.queue_latency.quantiles();
        let process_cpu = cpu.sample();
        let sample = CapacitySample {
            qps,
            queue_p95_ms: queue_p95,
            error_rate_pct,
            process_cpu_pct: process_cpu.utilization_pct,
        };
        let decision = capacity.evaluate(sample);
        shared
            .active_cpu_workers
            .store(decision.active_workers, Ordering::Release);
        shared
            .total_concurrency
            .store(decision.total_concurrency, Ordering::Release);

        let (active_shards, shard_phase) = if decision.phase == CapacityPhase::Locked {
            let shard_decision = shard_controller.evaluate(sample);
            shared
                .active_shards
                .store(shard_decision.active_workers, Ordering::Release);
            (shard_decision.active_workers, shard_decision.phase)
        } else {
            (
                shared.active_shards.load(Ordering::Acquire),
                shard_controller.decision().phase,
            )
        };
        println!(
            "[{:.2}%] QPS {:>9.1} | CPU {:>3} (all {} NUMA online) | IO {:>5} | shards {:>3} | proc CPU {:>5.1}% | queue p95 {:.3} p99 {:.3} ms | failed {} | {:?}/{:?}",
            snapshot.completed as f64 / opts.total_tasks as f64 * 100.0,
            qps,
            decision.active_workers,
            node_count,
            decision.total_concurrency,
            active_shards,
            process_cpu.utilization_pct,
            queue_p95,
            queue_p99,
            snapshot.failed,
            decision.phase,
            shard_phase,
        );
        last = snapshot;
        sampled_at = Instant::now();
    }
    shared.running.store(false, Ordering::Release);

    for thread in node_threads {
        match thread.join() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => return Err("NUMA runtime panicked".into()),
        }
    }

    let wall = started.elapsed().as_secs_f64();
    let snapshot = shared.snapshot();
    let final_cpu = cpu.sample();
    println!("============================================================");
    println!("  Wall Clock Time:    {:.2}s", wall);
    println!(
        "  Tasks Completed:    {} / {}",
        snapshot.completed, opts.total_tasks
    );
    println!("  Valid Markdown:     {}", snapshot.pages);
    println!("  Failed Count:       {}", snapshot.failed);
    println!(
        "  Sustained QPS:      {:.2}",
        snapshot.pages as f64 / wall.max(0.001)
    );
    println!("  Process CPU Time:   {:.2}s", final_cpu.process_cpu_time_s);
    println!(
        "  Throughput Volume:  {:.2} MB/s",
        snapshot.bytes as f64 / 1_048_576.0 / wall.max(0.001)
    );
    println!("============================================================");
    Ok(())
}

fn run_node(
    node_index: usize,
    node_count: usize,
    cluster: LocalityCluster,
    shared: Arc<Shared>,
) -> Result<(), String> {
    let cpus = cluster.logical_cpus().collect::<Vec<_>>();
    if !pin_current_thread_to_set(&cpus) {
        return Err(format!("failed to bind NUMA coordinator {node_index}"));
    }
    let (cpu_submitter, cpu_threads) =
        build_cpu_pool(node_index, node_count, &cpus, shared.clone())?;

    let io_worker_cpus = cpus.clone();
    let io_workers = cpus.len().div_ceil(8).clamp(2, 8);
    let io_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(io_workers)
        .enable_all()
        .thread_name(format!("ps-io-n{node_index}"))
        .on_thread_start(move || {
            pin_current_thread_to_set(&io_worker_cpus);
        })
        .build()
        .map_err(|error| error.to_string())?;
    let result = io_runtime.block_on(node_loop(
        node_index,
        node_count,
        cpu_submitter.clone(),
        shared,
    ));
    cpu_submitter.queue.shutdown.store(true, Ordering::Release);
    cpu_submitter.queue.ready.notify_all();
    for thread in cpu_threads {
        thread
            .join()
            .map_err(|_| format!("CPU worker panicked in NUMA {node_index}"))??;
    }
    result
}

fn build_cpu_pool(
    node_index: usize,
    node_count: usize,
    cpus: &[usize],
    shared: Arc<Shared>,
) -> Result<
    (
        CpuSubmitter,
        Vec<std::thread::JoinHandle<Result<(), String>>>,
    ),
    String,
> {
    let queue = Arc::new(CpuQueue {
        jobs: Mutex::new(VecDeque::new()),
        ready: Condvar::new(),
        shutdown: AtomicBool::new(false),
    });
    let mut threads = Vec::with_capacity(cpus.len());
    for (local_rank, cpu_id) in cpus.iter().copied().enumerate() {
        let queue = queue.clone();
        let shared = shared.clone();
        threads.push(
            std::thread::Builder::new()
                .name(format!("ps-cpu-n{node_index}-{local_rank}"))
                .spawn(move || {
                    if !pin_current_thread(cpu_id) {
                        return Err(format!(
                            "failed to pin NUMA {node_index} CPU worker {cpu_id}"
                        ));
                    }
                    loop {
                        let mut jobs = queue.jobs.lock().unwrap();
                        while !queue.shutdown.load(Ordering::Acquire) {
                            let total_active = shared
                                .active_cpu_workers
                                .load(Ordering::Acquire)
                                .max(node_count);
                            let local_active = total_active / node_count
                                + usize::from(node_index < total_active % node_count);
                            if local_rank < local_active && !jobs.is_empty() {
                                break;
                            }
                            let waited = queue
                                .ready
                                .wait_timeout(jobs, Duration::from_millis(5))
                                .unwrap();
                            jobs = waited.0;
                        }
                        if queue.shutdown.load(Ordering::Acquire) && jobs.is_empty() {
                            break;
                        }
                        let Some(job) = jobs.pop_front() else {
                            continue;
                        };
                        drop(jobs);

                        let result = (|| {
                            let text = std::str::from_utf8(&job.body)
                                .map_err(|error| format!("response was not UTF-8: {error}"))?;
                            let options = pagesift_content::Options {
                                base_url: url::Url::parse(&job.url).ok(),
                                ..Default::default()
                            };
                            let document = pagesift_content::process(text, &options);
                            if document.markdown.is_empty() {
                                return Err("empty Markdown".to_string());
                            }
                            Ok(job.body.len())
                        })();
                        let _ = job.result.send(result);
                    }
                    Ok(())
                })
                .map_err(|error| error.to_string())?,
        );
    }
    Ok((CpuSubmitter { queue }, threads))
}

async fn node_loop(
    node_index: usize,
    node_count: usize,
    cpu_submitter: CpuSubmitter,
    shared: Arc<Shared>,
) -> Result<(), String> {
    let max_local_shards = shared
        .active_shards
        .load(Ordering::Acquire)
        .div_ceil(node_count);
    let mut dispatchers = Vec::with_capacity(max_local_shards);
    for local_rank in 0..max_local_shards {
        let shared = shared.clone();
        let gate = AdaptiveConcurrencyGate::new(16);
        let dispatcher_cpu_submitter = cpu_submitter.clone();
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(64)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_nodelay(true)
            .build()
            .map_err(|error| error.to_string())?;
        dispatchers.push(tokio::spawn(async move {
            loop {
                if shared.next_task.load(Ordering::Relaxed) >= shared.total_tasks {
                    break;
                }
                let global_shards = shared.active_shards.load(Ordering::Acquire).max(node_count);
                let local_active = global_shards / node_count
                    + usize::from(node_index < global_shards % node_count);
                if local_rank >= local_active {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    continue;
                }
                let total_concurrency = shared.total_concurrency.load(Ordering::Acquire);
                let local_concurrency = (total_concurrency / global_shards
                    + usize::from(
                        node_index * max_local_shards + local_rank
                            < total_concurrency % global_shards,
                    ))
                .max(1);
                gate.set_limit(local_concurrency);

                let block_start = shared.next_task.fetch_add(TASK_BLOCK, Ordering::Relaxed);
                if block_start >= shared.total_tasks {
                    break;
                }
                let block_end = (block_start + TASK_BLOCK).min(shared.total_tasks);
                for task_id in block_start..block_end {
                    let permit = gate.acquire().await;
                    let client = client.clone();
                    let shared = shared.clone();
                    let cpu_submitter = dispatcher_cpu_submitter.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        let queued_at = Instant::now();
                        let url = format!("{}/p/{task_id}", shared.origin_url);
                        let outcome = async {
                            let body = client.get(&url).send().await?.bytes().await?;
                            if shared.queue_sample_every > 0
                                && task_id % shared.queue_sample_every == 0
                            {
                                shared
                                    .queue_latency
                                    .record(queued_at.elapsed().as_secs_f64() * 1_000.0);
                            }
                            let body_len = body.len();
                            let (result_tx, result_rx) = tokio::sync::oneshot::channel();
                            cpu_submitter.submit(ParseJob {
                                body,
                                url,
                                result: result_tx,
                            });
                            result_rx
                                .await
                                .map_err(|_| -> Box<dyn std::error::Error + Send + Sync> {
                                    "CPU parse worker stopped".into()
                                })?
                                .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> {
                                    error.into()
                                })?;
                            Ok::<usize, Box<dyn std::error::Error + Send + Sync>>(body_len)
                        }
                        .await;
                        let counters = &shared.counters[node_index];
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
            }
            gate.wait_idle().await;
        }));
    }
    for dispatcher in dispatchers {
        dispatcher.await.map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn shard_tiers(numa_nodes: usize, physical_cores: usize) -> Vec<usize> {
    let stride = numa_nodes.max(1);
    let ceiling = physical_cores.min(256).max(stride);
    let mut tiers = (stride..=ceiling).step_by(stride).collect::<Vec<_>>();
    if tiers.last().copied() != Some(ceiling) {
        tiers.push(ceiling);
    }
    tiers
}
