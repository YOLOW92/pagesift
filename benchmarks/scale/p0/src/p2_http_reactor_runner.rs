//! P2 Step 3: NUMA-local, fixed-ownership HTTP/1.1 reactor experiment.
//!
//! This data path intentionally avoids Tokio, reqwest, hyper, per-page tasks,
//! and global task allocation. Every reactor thread owns its sockets and fixed
//! slots for their complete lifetime. Mio supplies the safe epoll wrapper; the
//! request encoder, response framing, timeout/retry policy, and pool ownership
//! are implemented here.

#![forbid(unsafe_code)]

// The benchmark process creates and frees response buffers and parse
// workspaces on every reactor/content thread. A general-purpose allocator can
// serialize those operations on shared arenas on a many-core host. mimalloc
// keeps allocation thread-local and is already used by the other benchmark
// binaries in this repository.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use clap::{Parser, ValueEnum};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError, TrySendError, bounded};
use mio::event::Event;
use mio::net::TcpStream;
use mio::{Events, Interest, Poll, Token};
use pagesift_policy::adaptive::{
    AdaptiveCapacityController, CapacityControllerConfig, CapacityDecision, CapacityPhase,
    CapacitySample, ConcurrencyMeasurement, DistributionAllocation, DistributionControlState,
    DistributionController, DistributionControllerConfig, DistributionSample, HardwareTopology,
    PipelineAllocation, PipelineControlState, PipelineController, PipelineControllerConfig,
    PipelineSample, pin_current_thread_to_set,
};
use serde::Serialize;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, ToSocketAddrs};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use url::Url;

const READ_CHUNK: usize = 16 * 1024;

#[derive(Debug, Parser)]
#[command(about = "PageSift P2 NUMA-local HTTP/1.1 reactor runner")]
struct Opts {
    /// Comma-separated HTTP origins. Connections are distributed round-robin.
    #[arg(long, value_delimiter = ',', default_value = "http://127.0.0.1:19800")]
    origin_url: Vec<String>,
    #[arg(long, default_value_t = 100_000_000)]
    total_tasks: u64,
    #[arg(long, value_enum, default_value_t = ProbeMode::Full)]
    mode: ProbeMode,
    /// Persistent connection capacity. Zero enables topology-derived online
    /// adaptation and is mandatory for a formal P3 run.
    #[arg(long, default_value_t = 0)]
    connections: usize,
    /// Reactor threads. Zero uses all logical CPUs inline, or an automatic
    /// topology/connection split with --split-pipeline.
    #[arg(long, default_value_t = 0)]
    reactors: usize,
    /// Split network polling from content processing with NUMA-local bounded queues.
    #[arg(long, default_value_t = false)]
    split_pipeline: bool,
    /// Runtime-tune batching and starvation-only NUMA work stealing. Disable
    /// only for a frozen A/B baseline.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    adaptive_distribution: bool,
    /// Enforce the frozen P3 autonomous-run contract.
    #[arg(long, default_value_t = false)]
    formal: bool,
    /// Reuse this many deterministic fixtures. Zero uses unique page ids.
    #[arg(long, default_value_t = 5)]
    fixture_pages: u64,
    #[arg(long, default_value_t = 30_000)]
    timeout_ms: u64,
    /// Idempotent reconnect attempts after the first request attempt.
    #[arg(long, default_value_t = 1)]
    retries: u32,
    #[arg(long, default_value_t = 8 * 1024 * 1024)]
    max_response_bytes: usize,
    #[arg(long, default_value_t = 1_000)]
    telemetry_interval_ms: u64,
    #[arg(long, default_value = "p2_http_reactor_runner.json")]
    output: PathBuf,
    /// Keep one deterministic content hash per N completed pages in the JSON
    /// report. 10,000 equals the frozen P3 0.01% audit rate.
    #[arg(long, default_value_t = 10_000)]
    sample_every: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ProbeMode {
    Transport,
    Full,
}

#[derive(Debug, Clone)]
struct Endpoint {
    base_url: Url,
    addresses: Vec<SocketAddr>,
    host_header: String,
    fixture_urls: Vec<Url>,
}

#[repr(align(128))]
#[derive(Default)]
struct NodeCounters {
    completed: AtomicU64,
    valid: AtomicU64,
    failed: AtomicU64,
    content_failed: AtomicU64,
    transport_failed: AtomicU64,
    bytes: AtomicU64,
    connects: AtomicU64,
    reused: AtomicU64,
    retries: AtomicU64,
    timeouts: AtomicU64,
    queue_backpressure: AtomicU64,
    response_buffer_allocations: AtomicU64,
    content_queued: AtomicU64,
    reactor_inline_parsed: AtomicU64,
    local_batches: AtomicU64,
    local_jobs: AtomicU64,
    local_steal_jobs: AtomicU64,
    steal_attempts: AtomicU64,
    steal_successes: AtomicU64,
    stolen_jobs: AtomicU64,
}

/// Diagnostic-only counters that explain timeout states and reactor stalls.
/// They are printed to stdout and deliberately excluded from the formal JSON
/// schema so the frozen P3 report shape does not change.
#[derive(Default)]
struct ReactorDiagnostics {
    timeouts_connecting: AtomicU64,
    timeouts_writing: AtomicU64,
    timeouts_reading: AtomicU64,
    timeouts_disabled: AtomicU64,
    timeouts_reading_empty: AtomicU64,
    timeouts_reading_partial: AtomicU64,
    timeouts_writing_partial: AtomicU64,
    timeouts_first_attempt: AtomicU64,
    timeouts_retry: AtomicU64,
    /// State of the socket at the moment a Reading timeout fires. A non-zero
    /// peek means the response was sitting in the kernel buffer and the
    /// reactor missed a readiness edge; WouldBlock means the origin never
    /// answered; EOF means the peer closed and the close edge was missed.
    timeouts_peek_data: AtomicU64,
    timeouts_peek_eof: AtomicU64,
    timeouts_peek_blocked: AtomicU64,
    timeouts_peek_error: AtomicU64,
    inline_parse_nanos: AtomicU64,
    max_loop_work_nanos: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
struct Snapshot {    completed: u64,
    valid: u64,
    failed: u64,
    content_failed: u64,
    transport_failed: u64,
    bytes: u64,
    connects: u64,
    reused: u64,
    retries: u64,
    timeouts: u64,
    queue_backpressure: u64,
    response_buffer_allocations: u64,
    content_queued: u64,
    reactor_inline_parsed: u64,
    local_batches: u64,
    local_jobs: u64,
    local_steal_jobs: u64,
    steal_attempts: u64,
    steal_successes: u64,
    stolen_jobs: u64,
}

#[derive(Debug, Serialize)]
struct NodeReport {
    node_index: usize,
    numa_node: usize,
    socket_id: usize,
    reactors: usize,
    content_workers: usize,
    connections: usize,
    completed: u64,
    valid: u64,
    failed: u64,
    content_failed: u64,
    transport_failed: u64,
    bytes: u64,
    connects: u64,
    reused: u64,
    retries: u64,
    timeouts: u64,
    queue_backpressure: u64,
    response_buffer_allocations: u64,
    content_queued: u64,
    reactor_inline_parsed: u64,
    local_batches: u64,
    local_jobs: u64,
    local_steal_jobs: u64,
    steal_attempts: u64,
    steal_successes: u64,
    stolen_jobs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SampleRecord {
    task_id: u64,
    body_bytes: usize,
    body_blake3: String,
}

#[derive(Debug, Serialize)]
struct FinalReport {
    schema: u32,
    formal: bool,
    executor: &'static str,
    transport: &'static str,
    mode: &'static str,
    total_tasks: u64,
    completed: u64,
    valid: u64,
    failed: u64,
    content_failed: u64,
    transport_failed: u64,
    bytes: u64,
    wall_time_s: f64,
    sustained_qps: f64,
    throughput_mb_s: f64,
    process_cpu_time_s: f64,
    process_cpu_cores: f64,
    process_cpu_pct_of_assigned: f64,
    peak_rss_bytes: u64,
    assigned_cpus: usize,
    physical_cores: usize,
    sockets: usize,
    numa_nodes: usize,
    reactors: usize,
    content_workers: usize,
    connections: usize,
    initial_connections: usize,
    final_connections: usize,
    connection_capacity: usize,
    fixture_pages: u64,
    sample_every: u64,
    samples: Vec<SampleRecord>,
    connects: u64,
    reused: u64,
    retries: u64,
    timeouts: u64,
    queue_backpressure: u64,
    response_buffer_allocations: u64,
    content_queued: u64,
    reactor_inline_parsed: u64,
    local_batches: u64,
    local_jobs: u64,
    local_steal_jobs: u64,
    steal_attempts: u64,
    steal_successes: u64,
    stolen_jobs: u64,
    initial_pipeline_allocation: Option<PipelineAllocation>,
    final_pipeline_allocation: Option<PipelineAllocation>,
    pipeline_adjustments: Vec<PipelineAllocation>,
    initial_distribution_allocation: Option<DistributionAllocation>,
    final_distribution_allocation: Option<DistributionAllocation>,
    distribution_adjustments: Vec<DistributionAllocation>,
    capacity_adjustments: Vec<CapacityDecision>,
    concurrency_measurements: Vec<ConcurrencyMeasurement>,
    node_reports: Vec<NodeReport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionState {
    Idle,
    Connecting,
    Writing,
    Reading,
    Dispatching,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyFraming {
    ContentLength(usize),
    Chunked,
    CloseDelimited,
}

#[derive(Debug, Clone, Copy)]
struct ResponseMeta {
    header_len: usize,
    framing: BodyFraming,
    close_after: bool,
    success_status: bool,
}

struct Connection {
    token: Token,
    activation_rank: usize,
    endpoint_index: usize,
    address_index: usize,
    stream: Option<TcpStream>,
    state: ConnectionState,
    task_id: u64,
    attempts: u32,
    request: Vec<u8>,
    written: usize,
    response: Vec<u8>,
    meta: Option<ResponseMeta>,
    pending_dispatch: Option<PendingDispatch>,
    deadline: Instant,
}

struct ParseJob {
    bytes: Vec<u8>,
    body: Range<usize>,
    endpoint_index: usize,
    task_id: u64,
    source_node: usize,
    recycler: Sender<Vec<u8>>,
}

struct PendingDispatch {
    job: ParseJob,
    close_after: bool,
}

#[derive(Debug)]
enum ProtocolError {
    InvalidResponse(&'static str),
    Header(httparse::Error),
    ResponseTooLarge,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidResponse(message) => f.write_str(message),
            Self::Header(error) => write!(f, "HTTP header parse failed: {error}"),
            Self::ResponseTooLarge => f.write_str("HTTP response exceeded configured limit"),
        }
    }
}

impl std::error::Error for ProtocolError {}

struct ReactorConfig {
    reactor_index: usize,
    node_index: usize,
    /// Zero-based reactor index inside its NUMA node. Connection activation
    /// quotas are spread across these local reactors, never across the
    /// node-global slot rank space.
    local_reactor: usize,
    node_reactors: usize,
    cpu_set: Vec<usize>,
    slot_ranks: Vec<usize>,
    total_tasks: u64,
    fixture_pages: u64,
    endpoints: Arc<Vec<Endpoint>>,
    timeout: Duration,
    retries: u32,
    max_response_bytes: usize,
    process_content: bool,
    parse_sender: Option<Sender<ParseJob>>,
    recycle_sender: Option<Sender<Vec<u8>>>,
    recycle_receiver: Option<Receiver<Vec<u8>>>,
    inline_parse_percent: Arc<AtomicUsize>,
    active_connections_by_node: Arc<Vec<AtomicUsize>>,
    next_task: Arc<AtomicU64>,
    sample_every: u64,
    samples: Arc<Mutex<Vec<SampleRecord>>>,
    counters: Arc<Vec<NodeCounters>>,
    diagnostics: Arc<ReactorDiagnostics>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = Opts::parse();
    let topology = HardwareTopology::detect();
    let clusters = topology.cpu_topology.clusters.clone();
    if clusters.is_empty() {
        return Err("no NUMA locality clusters detected".into());
    }
    let endpoints = Arc::new(resolve_endpoints(&opts.origin_url, opts.fixture_pages)?);
    let split_pipeline = opts.split_pipeline && opts.mode == ProbeMode::Full;
    validate_run_contract(&opts, split_pipeline)?;
    let adaptive_capacity = opts.connections == 0;
    let initial_connections = if adaptive_capacity {
        topology.base_concurrency
    } else {
        opts.connections
    }
    .max(clusters.len());
    let connection_capacity = if adaptive_capacity {
        topology.max_concurrency
    } else {
        initial_connections
    }
    .max(initial_connections);
    let requested_reactors = if opts.reactors == 0 {
        if split_pipeline {
            automatic_reactor_count(
                topology.logical_cores,
                topology.physical_cores,
                clusters.len(),
                initial_connections,
            )
        } else {
            topology.logical_cores
        }
    } else {
        opts.reactors
    };
    let maximum_reactors = if split_pipeline {
        topology
            .logical_cores
            .saturating_sub(clusters.len())
            .max(clusters.len())
    } else {
        connection_capacity.max(clusters.len())
    };
    let reactor_count = requested_reactors.clamp(
        clusters.len(),
        maximum_reactors.min(connection_capacity.max(clusters.len())),
    );
    let connections = connection_capacity.max(reactor_count);
    let node_weights = clusters
        .iter()
        .map(|cluster| cluster.logical_cpus().count())
        .collect::<Vec<_>>();
    let reactor_counts = distribute_by_weights(reactor_count, &node_weights);
    let connection_counts = distribute_by_weights(connections, &node_weights);
    let initial_connection_counts = distribute_by_weights(initial_connections, &node_weights);
    let active_connections_by_node = Arc::new(
        initial_connection_counts
            .iter()
            .copied()
            .map(AtomicUsize::new)
            .collect::<Vec<_>>(),
    );
    let next_task = Arc::new(AtomicU64::new(0));
    let samples = Arc::new(Mutex::new(Vec::<SampleRecord>::new()));
    let capacity_controller = adaptive_capacity.then(|| {
        Arc::new(AdaptiveCapacityController::new_with_initial_worker(
            vec![reactor_count],
            reactor_count,
            initial_connections,
            CapacityControllerConfig {
                settle_samples: 1,
                min_probe_samples: 3,
                max_probe_samples: 5,
                min_concurrency: topology.min_concurrency,
                max_concurrency: connection_capacity,
                initial_concurrency_step_tiers: 2,
                max_inconclusive_probes: 1,
                ..CapacityControllerConfig::default()
            },
        ))
    });
    let initial_capacity_decision = capacity_controller
        .as_ref()
        .map(|controller| controller.decision());
    let content_worker_count = if split_pipeline {
        topology
            .logical_cores
            .saturating_sub(reactor_count)
            .max(clusters.len())
    } else {
        0
    };
    let content_worker_counts = if split_pipeline {
        distribute_by_weights(content_worker_count, &node_weights)
    } else {
        vec![0; clusters.len()]
    };
    let pipeline_controller = split_pipeline.then(|| {
        Arc::new(PipelineController::new(PipelineControllerConfig {
            total_workers: topology.logical_cores,
            initial_reactors: reactor_count,
            numa_nodes: clusters.len(),
            adjustment_quantum: clusters.len().saturating_mul(4),
            // Inline parsing borrows reactor cycles from socket servicing. The
            // controller explores the full content range; only statistical
            // evidence decides where the search stops.
            settle_ticks: 5,
            lock_ticks_before_reprobe: 60,
            min_effect_ratio: 0.005,
            noise_sigmas: 2.0,
            min_window_samples: 3,
            max_window_samples: 5,
            relative_sem_target: 0.01,
        }))
    });
    let initial_pipeline_allocation = pipeline_controller
        .as_ref()
        .map(|controller| controller.allocation());
    let distribution_controller = (split_pipeline && opts.adaptive_distribution).then(|| {
        Arc::new(DistributionController::new(DistributionControllerConfig {
            workers_per_node: content_worker_count.div_ceil(clusters.len()),
            settle_ticks: 2,
            lock_ticks_before_reprobe: 8,
            min_effect_ratio: 0.005,
            noise_sigmas: 2.0,
            min_window_samples: 3,
            max_window_samples: 5,
            relative_sem_target: 0.01,
        }))
    });
    let initial_distribution_allocation = distribution_controller
        .as_ref()
        .map(|controller| controller.allocation());
    let inline_parse_percent = Arc::new(AtomicUsize::new(
        initial_pipeline_allocation
            .map(|allocation| allocation.inline_parse_percent)
            .unwrap_or(0),
    ));
    let active_content_by_node = Arc::new(
        content_worker_counts
            .iter()
            .copied()
            .map(AtomicUsize::new)
            .collect::<Vec<_>>(),
    );
    let local_batch_size = Arc::new(AtomicUsize::new(
        initial_distribution_allocation
            .map(|allocation| allocation.local_batch_size)
            .unwrap_or(1),
    ));
    let empty_polls_before_steal = Arc::new(AtomicUsize::new(
        initial_distribution_allocation
            .map(|allocation| allocation.empty_polls_before_steal)
            .unwrap_or(usize::MAX),
    ));
    let steal_batch_size = Arc::new(AtomicUsize::new(
        initial_distribution_allocation
            .map(|allocation| allocation.steal_batch_size)
            .unwrap_or(1),
    ));
    let stop_content_workers = Arc::new(AtomicBool::new(false));
    let counters = Arc::new(
        (0..clusters.len())
            .map(|_| NodeCounters::default())
            .collect::<Vec<_>>(),
    );
    let finished_reactors = Arc::new(AtomicU64::new(0));
    let finished_at_ns = Arc::new(AtomicU64::new(0));
    let diagnostics = Arc::new(ReactorDiagnostics::default());

    println!("============================================================");
    println!("  PageSift P2 NUMA-Local HTTP Reactor");
    println!(
        "  Topology: {} logical / {} physical / {} sockets / {} NUMA",
        topology.logical_cores, topology.physical_cores, topology.sockets, topology.numa_nodes
    );
    println!(
        "  Tasks: {} / reactors: {} / content workers: {} / connections: {} initial, {} capacity",
        opts.total_tasks,
        reactor_count,
        content_worker_count,
        initial_connections,
        connection_capacity,
    );
    for (index, cluster) in clusters.iter().enumerate() {
        println!(
            "  Node {} (NUMA {}, socket {}): {} reactors / {} content workers / {} connections",
            index,
            cluster.numa_node,
            cluster.socket_id,
            reactor_counts[index],
            content_worker_counts[index],
            initial_connection_counts[index]
        );
    }
    println!("============================================================");

    let cpu_start = read_process_cpu_ticks().unwrap_or(0);
    let started = Instant::now();
    let mut parser_threads = Vec::with_capacity(content_worker_count);
    let mut threads = Vec::with_capacity(reactor_count);
    let mut node_parse_senders = Vec::<Vec<Sender<ParseJob>>>::with_capacity(clusters.len());
    let mut node_parse_receivers = Vec::<Vec<Receiver<ParseJob>>>::with_capacity(clusters.len());
    if split_pipeline {
        for node_index in 0..clusters.len() {
            let mut senders = Vec::with_capacity(reactor_counts[node_index]);
            let mut receivers = Vec::with_capacity(reactor_counts[node_index]);
            for slot_count in
                distribute_evenly(connection_counts[node_index], reactor_counts[node_index])
            {
                let (sender, receiver) = bounded(slot_count.saturating_mul(2).max(16));
                senders.push(sender);
                receivers.push(receiver);
            }
            node_parse_senders.push(senders);
            node_parse_receivers.push(receivers);
        }
        let all_receivers = Arc::new(node_parse_receivers.clone());
        for (node_index, cluster) in clusters.iter().enumerate() {
            for worker_index in 0..content_worker_counts[node_index] {
                let primary_lane = worker_index % node_parse_receivers[node_index].len();
                let receiver = node_parse_receivers[node_index][primary_lane].clone();
                let receivers = all_receivers.clone();
                let endpoints = endpoints.clone();
                let counters = counters.clone();
                let cpu_set = cluster.logical_cpus().collect::<Vec<_>>();
                let finished_at = finished_at_ns.clone();
                let active_content = active_content_by_node.clone();
                let stop = stop_content_workers.clone();
                let samples = samples.clone();
                let batch = local_batch_size.clone();
                let empty_before_steal = empty_polls_before_steal.clone();
                let steal_batch = steal_batch_size.clone();
                parser_threads.push(
                    std::thread::Builder::new()
                        .name(format!("p2-content-n{node_index}-w{worker_index}"))
                        .spawn(move || {
                            run_content_worker(
                                node_index,
                                worker_index,
                                primary_lane,
                                cpu_set,
                                receiver,
                                receivers,
                                endpoints,
                                opts.fixture_pages,
                                counters,
                                started,
                                finished_at,
                                active_content,
                                batch,
                                empty_before_steal,
                                steal_batch,
                                opts.adaptive_distribution,
                                stop,
                                opts.sample_every,
                                samples,
                            )
                        })?,
                );
            }
        }
    }
    let mut reactor_index = 0usize;
    for (node_index, cluster) in clusters.iter().enumerate() {
        let local_reactors = reactor_counts[node_index];
        let local_connections = connection_counts[node_index];
        let per_reactor = distribute_evenly(local_connections, local_reactors);
        for (local_reactor, slot_count) in per_reactor.into_iter().enumerate() {
            let (parse_sender, recycle_receiver) = if split_pipeline {
                let capacity = slot_count.saturating_mul(2).max(16);
                let (recycle_tx, recycle_rx) = bounded(capacity);
                (
                    Some((
                        node_parse_senders[node_index][local_reactor].clone(),
                        recycle_tx,
                    )),
                    Some(recycle_rx),
                )
            } else {
                (None, None)
            };
            let config = ReactorConfig {
                reactor_index,
                node_index,
                local_reactor,
                node_reactors: local_reactors,
                cpu_set: cluster.logical_cpus().collect(),
                // Ranks restart per reactor so an active-connection quota is
                // applied evenly to every reactor on the node.
                slot_ranks: (0..slot_count).collect(),
                total_tasks: opts.total_tasks,
                fixture_pages: opts.fixture_pages,
                endpoints: endpoints.clone(),
                timeout: Duration::from_millis(opts.timeout_ms.max(1)),
                retries: opts.retries,
                max_response_bytes: opts.max_response_bytes.max(1024),
                process_content: opts.mode == ProbeMode::Full,
                parse_sender: parse_sender.as_ref().map(|(sender, _)| sender.clone()),
                recycle_sender: parse_sender.map(|(_, recycler)| recycler),
                recycle_receiver,
                inline_parse_percent: inline_parse_percent.clone(),
                active_connections_by_node: active_connections_by_node.clone(),
                next_task: next_task.clone(),
                sample_every: opts.sample_every,
                samples: samples.clone(),
                counters: counters.clone(),
                diagnostics: diagnostics.clone(),
            };
            let finished = finished_reactors.clone();
            let finished_at = finished_at_ns.clone();
            threads.push(
                std::thread::Builder::new()
                    .name(format!("p2-http-r{reactor_index}"))
                    .spawn(move || {
                        let result = run_reactor(config);
                        finished_at.fetch_max(
                            started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                            Ordering::Release,
                        );
                        finished.fetch_add(1, Ordering::Release);
                        result
                    })?,
            );
            reactor_index += 1;
        }
    }

    let interval = Duration::from_millis(opts.telemetry_interval_ms.max(100));
    let mut last = Snapshot::default();
    let mut sampled_at = Instant::now();
    let mut last_cpu_ticks = cpu_start;
    let mut pipeline_adjustments = initial_pipeline_allocation.into_iter().collect::<Vec<_>>();
    let mut distribution_adjustments = initial_distribution_allocation
        .into_iter()
        .collect::<Vec<_>>();
    let mut capacity_adjustments = initial_capacity_decision.into_iter().collect::<Vec<_>>();
    let mut latest_capacity_decision = initial_capacity_decision;
    let mut latest_pipeline_allocation = initial_pipeline_allocation;
    let mut latest_distribution_allocation = initial_distribution_allocation;
    let mut distribution_started = false;
    loop {
        std::thread::sleep(interval);
        let current = snapshot(&counters);
        let elapsed = sampled_at.elapsed().as_secs_f64().max(1e-9);
        let qps = current.completed.saturating_sub(last.completed) as f64 / elapsed;
        let cpu_ticks = read_process_cpu_ticks()
            .unwrap_or(cpu_start)
            .saturating_sub(cpu_start);
        let cpu_time = cpu_ticks as f64 / clock_ticks_per_second();
        let avg_cores = cpu_time / started.elapsed().as_secs_f64().max(1e-9);
        let interval_cpu_pct = cpu_ticks.saturating_sub(last_cpu_ticks.saturating_sub(cpu_start))
            as f64
            / clock_ticks_per_second()
            / elapsed
            / topology.logical_cores.max(1) as f64
            * 100.0;
        last_cpu_ticks = cpu_start.saturating_add(cpu_ticks);
        let completed_delta = current.completed.saturating_sub(last.completed);
        let failed_delta = current.failed.saturating_sub(last.failed);
        if let Some(controller) = &capacity_controller {
            let decision = controller.evaluate(CapacitySample {
                qps,
                queue_p95_ms: 0.0,
                error_rate_pct: failed_delta as f64 / completed_delta.max(1) as f64 * 100.0,
                process_cpu_pct: interval_cpu_pct,
            });
            let per_node = distribute_by_weights(decision.total_concurrency, &node_weights);
            for (target, value) in active_connections_by_node.iter().zip(per_node) {
                target.store(value, Ordering::Release);
            }
            if capacity_adjustments.last() != Some(&decision) {
                println!(
                    "  capacity: {} active connections / {:?}",
                    decision.total_concurrency, decision.phase
                );
                capacity_adjustments.push(decision);
            }
            latest_capacity_decision = Some(decision);
        }
        let capacity_is_stable =
            latest_capacity_decision.is_none_or(|decision| decision.phase == CapacityPhase::Locked);
        // Controllers take turns instead of adapting at the same time: a
        // concurrent change would make it impossible to attribute a QPS shift
        // to either controller. The distribution controller therefore starts
        // once the pipeline controller has confirmed a local optimum.
        let distribution_is_stable = !distribution_started
            || latest_distribution_allocation
                .is_none_or(|allocation| allocation.state == DistributionControlState::Locked);
        if capacity_is_stable
            && distribution_is_stable
            && let Some(controller) = &pipeline_controller
        {
            let allocation = controller.tick(PipelineSample {
                qps,
                process_cpu_pct: interval_cpu_pct,
                completed: completed_delta,
                queue_depth: current.content_queued as usize,
                queue_capacity: connections.saturating_mul(2),
                queue_backpressure: current
                    .queue_backpressure
                    .saturating_sub(last.queue_backpressure),
                error_rate_pct: failed_delta as f64 / completed_delta.max(1) as f64 * 100.0,
            });
            inline_parse_percent.store(allocation.inline_parse_percent, Ordering::Relaxed);
            let per_node = distribute_by_weights(allocation.content_budget, &node_weights);
            for (target, value) in active_content_by_node.iter().zip(per_node) {
                target.store(value, Ordering::Release);
            }
            if pipeline_adjustments.last() != Some(&allocation) {
                let evidence = controller.last_evidence();
                println!(
                    "  controller: reactor budget {} / content budget {} / inline {}% / {:?} | evidence {} samples, mean {:.1} pages/s, rel SEM {:.3}%",
                    allocation.reactor_budget,
                    allocation.content_budget,
                    allocation.inline_parse_percent,
                    allocation.state,
                    evidence.samples,
                    evidence.mean,
                    evidence.relative_sem * 100.0
                );
                pipeline_adjustments.push(allocation);
            }
            latest_pipeline_allocation = Some(allocation);
        }
        let pipeline_is_stable = latest_pipeline_allocation
            .is_none_or(|allocation| allocation.state == PipelineControlState::Locked);
        if pipeline_is_stable && let Some(controller) = &distribution_controller {
            distribution_started = true;
            let completed_delta = current.completed.saturating_sub(last.completed);
            let depths = counters
                .iter()
                .map(|node| node.content_queued.load(Ordering::Relaxed) as usize)
                .collect::<Vec<_>>();
            let minimum_depth = depths.iter().copied().min().unwrap_or(0);
            let maximum_depth = depths.iter().copied().max().unwrap_or(0);
            let average_capacity = connections
                .saturating_mul(2)
                .div_ceil(clusters.len().max(1))
                .max(1);
            let allocation = controller.tick(DistributionSample {
                qps,
                completed: completed_delta,
                queue_depth: current.content_queued as usize,
                queue_capacity: connections.saturating_mul(2),
                queue_imbalance_ratio: maximum_depth.saturating_sub(minimum_depth) as f64
                    / average_capacity as f64,
                queue_backpressure: current
                    .queue_backpressure
                    .saturating_sub(last.queue_backpressure),
                steal_attempts: current.steal_attempts.saturating_sub(last.steal_attempts),
                steal_successes: current.steal_successes.saturating_sub(last.steal_successes),
                stolen_jobs: current.stolen_jobs.saturating_sub(last.stolen_jobs),
                error_rate_pct: current.failed.saturating_sub(last.failed) as f64
                    / completed_delta.max(1) as f64
                    * 100.0,
            });
            local_batch_size.store(allocation.local_batch_size, Ordering::Relaxed);
            empty_polls_before_steal.store(allocation.empty_polls_before_steal, Ordering::Relaxed);
            steal_batch_size.store(allocation.steal_batch_size, Ordering::Relaxed);
            if distribution_adjustments.last() != Some(&allocation) {
                let evidence = controller.last_evidence();
                println!(
                    "  distributor: local batch {} / steal after {} empty polls / steal batch {} / {:?} | evidence {} samples, mean {:.1} pages/s, rel SEM {:.3}%",
                    allocation.local_batch_size,
                    allocation.empty_polls_before_steal,
                    allocation.steal_batch_size,
                    allocation.state,
                    evidence.samples,
                    evidence.mean,
                    evidence.relative_sem * 100.0
                );
                distribution_adjustments.push(allocation);
            }
            latest_distribution_allocation = Some(allocation);
        }
        println!(
            "[{:.2}%] {:>9.1} pages/s | completed {} | failed {} | CPU {:>6.2} cores | connections {} | reuse {:.1}%",
            current.completed as f64 / opts.total_tasks.max(1) as f64 * 100.0,
            qps,
            current.completed,
            current.failed,
            avg_cores,
            latest_capacity_decision
                .map(|decision| decision.total_concurrency)
                .unwrap_or(connections),
            current.reused as f64 / current.completed.max(1) as f64 * 100.0,
        );
        last = current;
        sampled_at = Instant::now();
        if current.completed >= opts.total_tasks
            || finished_reactors.load(Ordering::Acquire) as usize >= reactor_count
        {
            break;
        }
    }

    for thread in threads {
        match thread.join() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => return Err("HTTP reactor thread panicked".into()),
        }
    }
    stop_content_workers.store(true, Ordering::Release);
    for thread in parser_threads {
        match thread.join() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => return Err("content worker thread panicked".into()),
        }
    }

    let completed_ns = finished_at_ns.load(Ordering::Acquire);
    let wall_time_s = if completed_ns == 0 {
        started.elapsed().as_secs_f64()
    } else {
        completed_ns as f64 / 1_000_000_000.0
    };
    let total = snapshot(&counters);
    if total.completed != opts.total_tasks {
        return Err(format!(
            "reactors stopped after {} of {} tasks",
            total.completed, opts.total_tasks
        )
        .into());
    }
    let mut sample_records = samples.lock().expect("sample ledger lock poisoned").clone();
    sample_records.sort_unstable_by_key(|sample| sample.task_id);
    sample_records.dedup_by_key(|sample| sample.task_id);
    let expected_samples = (opts.formal && opts.sample_every > 0)
        .then(|| opts.total_tasks.div_ceil(opts.sample_every) as usize);
    let process_cpu_ticks = read_process_cpu_ticks()
        .unwrap_or(cpu_start)
        .saturating_sub(cpu_start);
    let process_cpu_time_s = process_cpu_ticks as f64 / clock_ticks_per_second();
    let process_cpu_cores = process_cpu_time_s / wall_time_s.max(1e-9);
    let node_reports = clusters
        .iter()
        .enumerate()
        .map(|(index, cluster)| {
            let node = node_snapshot(&counters[index]);
            NodeReport {
                node_index: index,
                numa_node: cluster.numa_node,
                socket_id: cluster.socket_id,
                reactors: reactor_counts[index],
                content_workers: content_worker_counts[index],
                connections: connection_counts[index],
                completed: node.completed,
                valid: node.valid,
                failed: node.failed,
                content_failed: node.content_failed,
                transport_failed: node.transport_failed,
                bytes: node.bytes,
                connects: node.connects,
                reused: node.reused,
                retries: node.retries,
                timeouts: node.timeouts,
                queue_backpressure: node.queue_backpressure,
                response_buffer_allocations: node.response_buffer_allocations,
                content_queued: node.content_queued,
                reactor_inline_parsed: node.reactor_inline_parsed,
                local_batches: node.local_batches,
                local_jobs: node.local_jobs,
                local_steal_jobs: node.local_steal_jobs,
                steal_attempts: node.steal_attempts,
                steal_successes: node.steal_successes,
                stolen_jobs: node.stolen_jobs,
            }
        })
        .collect();
    let report = FinalReport {
        schema: 5,
        formal: opts.formal,
        executor: if split_pipeline {
            "numa_local_hybrid_pipeline"
        } else {
            "numa_local_fixed_ownership"
        },
        transport: "pagesift_mio_http1",
        mode: match opts.mode {
            ProbeMode::Transport => "transport",
            ProbeMode::Full => "end_to_end",
        },
        total_tasks: opts.total_tasks,
        completed: total.completed,
        valid: total.valid,
        failed: total.failed,
        content_failed: total.content_failed,
        transport_failed: total.transport_failed,
        bytes: total.bytes,
        wall_time_s,
        sustained_qps: total.valid as f64 / wall_time_s.max(1e-9),
        throughput_mb_s: total.bytes as f64 / 1_048_576.0 / wall_time_s.max(1e-9),
        process_cpu_time_s,
        process_cpu_cores,
        process_cpu_pct_of_assigned: process_cpu_cores / topology.logical_cores as f64 * 100.0,
        peak_rss_bytes: read_peak_rss_bytes().unwrap_or(0),
        assigned_cpus: topology.logical_cores,
        physical_cores: topology.physical_cores,
        sockets: topology.sockets,
        numa_nodes: topology.numa_nodes,
        reactors: reactor_count,
        content_workers: content_worker_count,
        connections,
        initial_connections,
        final_connections: capacity_controller
            .as_ref()
            .map(|controller| controller.decision().total_concurrency)
            .unwrap_or(connections),
        connection_capacity,
        fixture_pages: opts.fixture_pages,
        sample_every: opts.sample_every,
        samples: sample_records,
        connects: total.connects,
        reused: total.reused,
        retries: total.retries,
        timeouts: total.timeouts,
        queue_backpressure: total.queue_backpressure,
        response_buffer_allocations: total.response_buffer_allocations,
        content_queued: total.content_queued,
        reactor_inline_parsed: total.reactor_inline_parsed,
        local_batches: total.local_batches,
        local_jobs: total.local_jobs,
        local_steal_jobs: total.local_steal_jobs,
        steal_attempts: total.steal_attempts,
        steal_successes: total.steal_successes,
        stolen_jobs: total.stolen_jobs,
        initial_pipeline_allocation,
        final_pipeline_allocation: pipeline_controller
            .as_ref()
            .map(|controller| controller.allocation()),
        pipeline_adjustments,
        initial_distribution_allocation,
        final_distribution_allocation: distribution_controller
            .as_ref()
            .map(|controller| controller.allocation()),
        distribution_adjustments,
        capacity_adjustments,
        concurrency_measurements: capacity_controller
            .as_ref()
            .map(|controller| controller.concurrency_measurements())
            .unwrap_or_default(),
        node_reports,
    };
    fs::write(&opts.output, serde_json::to_vec_pretty(&report)?)?;
    println!("============================================================");
    println!("  Sustained QPS: {:.2}", report.sustained_qps);
    println!("  Completed: {} / {}", report.completed, report.total_tasks);
    println!("  Failed: {}", report.failed);
    println!(
        "  Failure classes: {} content / {} transport",
        report.content_failed, report.transport_failed
    );
    println!(
        "  Connections: {} opened / {} reused / {} retries / {} timeouts",
        report.connects, report.reused, report.retries, report.timeouts
    );
    if split_pipeline {
        println!(
            "  Pipeline: {} queue backpressure events / {} response-buffer allocations",
            report.queue_backpressure, report.response_buffer_allocations
        );
        println!(
            "  Distribution: {} local jobs in {} batches ({} lane-helped) / {} cross-NUMA jobs from {} successful steals",
            report.local_jobs,
            report.local_batches,
            report.local_steal_jobs,
            report.stolen_jobs,
            report.steal_successes
        );
    }
    println!(
        "  Runner CPU: {:.2} core-s/s ({:.2}% of {})",
        report.process_cpu_cores, report.process_cpu_pct_of_assigned, report.assigned_cpus
    );
    let diag_connecting = diagnostics.timeouts_connecting.load(Ordering::Relaxed);
    let diag_writing = diagnostics.timeouts_writing.load(Ordering::Relaxed);
    let diag_reading = diagnostics.timeouts_reading.load(Ordering::Relaxed);
    println!(
        "  Timeout states: {} connecting / {} writing / {} reading",
        diag_connecting, diag_writing, diag_reading
    );
    println!(
        "  Timeout detail: {} disabled / {} reading-empty / {} reading-partial / {} writing-partial / {} first / {} retry",
        diagnostics.timeouts_disabled.load(Ordering::Relaxed),
        diagnostics.timeouts_reading_empty.load(Ordering::Relaxed),
        diagnostics.timeouts_reading_partial.load(Ordering::Relaxed),
        diagnostics.timeouts_writing_partial.load(Ordering::Relaxed),
        diagnostics.timeouts_first_attempt.load(Ordering::Relaxed),
        diagnostics.timeouts_retry.load(Ordering::Relaxed)
    );
    println!(
        "  Timeout socket: {} buffered / {} eof / {} blocked / {} error",
        diagnostics.timeouts_peek_data.load(Ordering::Relaxed),
        diagnostics.timeouts_peek_eof.load(Ordering::Relaxed),
        diagnostics.timeouts_peek_blocked.load(Ordering::Relaxed),
        diagnostics.timeouts_peek_error.load(Ordering::Relaxed)
    );
    println!(
        "  Inline parse: {} pages / {:.1} ms total / max reactor work {:.1} ms",
        report.reactor_inline_parsed,
        diagnostics.inline_parse_nanos.load(Ordering::Relaxed) as f64 / 1e6,
        diagnostics.max_loop_work_nanos.load(Ordering::Relaxed) as f64 / 1e6
    );
    println!("  Report: {}", opts.output.display());
    println!("============================================================");
    if opts.formal {
        let expected_samples = expected_samples.unwrap_or(0);
        if report.failed != 0
            || report.valid != report.total_tasks
            || report.samples.len() != expected_samples
        {
            return Err(format!(
                "formal integrity gate failed: valid {}, failed {}, samples {}/{}; diagnostic report preserved at {}",
                report.valid,
                report.failed,
                report.samples.len(),
                expected_samples,
                opts.output.display()
            )
            .into());
        }
    }
    Ok(())
}

fn validate_run_contract(opts: &Opts, split_pipeline: bool) -> Result<(), String> {
    if opts.formal
        && (opts.connections != 0
            || opts.reactors != 0
            || !split_pipeline
            || !opts.adaptive_distribution
            || opts.fixture_pages != 5
            || opts.sample_every != 10_000)
    {
        return Err("P3 formal mode requires --mode full --split-pipeline, five fixtures and a 1/10,000 sample rate; it forbids manual --connections/--reactors or disabling adaptive distribution".to_string());
    }
    Ok(())
}

fn resolve_endpoints(
    origins: &[String],
    fixture_pages: u64,
) -> Result<Vec<Endpoint>, Box<dyn std::error::Error>> {
    origins
        .iter()
        .map(|origin| {
            let base_url = Url::parse(origin)?;
            if base_url.scheme() != "http" {
                return Err(format!(
                    "Step-3 plaintext reactor requires http:// origin, got {origin}"
                )
                .into());
            }
            let host = base_url
                .host_str()
                .ok_or_else(|| format!("origin has no host: {origin}"))?;
            let port = base_url.port_or_known_default().unwrap_or(80);
            let mut addresses = (host, port).to_socket_addrs()?.collect::<Vec<_>>();
            addresses.sort_unstable();
            addresses.dedup();
            if addresses.is_empty() {
                return Err(format!("origin resolved to no addresses: {origin}").into());
            }
            let host_header = match base_url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_string(),
            };
            let fixture_urls = (0..fixture_pages)
                .map(|page_id| base_url.join(&format!("p/{page_id}")))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Endpoint {
                base_url,
                addresses,
                host_header,
                fixture_urls,
            })
        })
        .collect()
}

fn run_reactor(config: ReactorConfig) -> Result<(), String> {
    if !pin_current_thread_to_set(&config.cpu_set) {
        return Err(format!(
            "reactor {} failed to apply NUMA affinity",
            config.reactor_index
        ));
    }
    let mut poll = Poll::new().map_err(|error| error.to_string())?;
    let mut events = Events::with_capacity(config.slot_ranks.len().max(8) * 2);
    let mut content_workspace = pagesift_content::ProcessWorkspace::default();
    let fixture_options = build_local_fixture_options(&config.endpoints);
    let now = Instant::now();
    let mut connections = config
        .slot_ranks
        .iter()
        .enumerate()
        .map(|(local_index, rank)| {
            Connection::new(Token(local_index), *rank, &config, now, poll.registry())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut done = connections
        .iter()
        .filter(|connection| connection.state == ConnectionState::Done)
        .count();

    while done < connections.len() {
        let now = Instant::now();
        for connection in &mut connections {
            if connection.state == ConnectionState::Idle {
                let was_done = connection.state == ConnectionState::Done;
                connection.activate_if_allowed(&config, poll.registry(), now)?;
                if !was_done && connection.state == ConnectionState::Done {
                    done += 1;
                }
            }
        }
        for connection in &mut connections {
            if connection.state == ConnectionState::Dispatching
                && flush_pending_dispatch(connection, &config, poll.registry(), now)?
            {
                done += usize::from(connection.state == ConnectionState::Done);
            }
        }
        if done >= connections.len() {
            break;
        }
        let wait = if connections
            .iter()
            .any(|connection| connection.state == ConnectionState::Dispatching)
        {
            Duration::from_millis(1)
        } else {
            Duration::from_millis(10)
        };
        match poll.poll(&mut events, Some(wait)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.to_string()),
        }
        // Work performed between two polls. A large value means the reactor is
        // falling behind and inline parsing is starving socket servicing.
        let work_started = Instant::now();
        for event in events.iter() {
            let index = event.token().0;
            if index >= connections.len()
                || matches!(
                    connections[index].state,
                    ConnectionState::Idle | ConnectionState::Dispatching | ConnectionState::Done
                )
            {
                continue;
            }
            let was_done = connections[index].state == ConnectionState::Done;
            drive_connection(
                &mut connections[index],
                event,
                &config,
                poll.registry(),
                &mut content_workspace,
                &fixture_options,
            )?;
            if !was_done && connections[index].state == ConnectionState::Done {
                done += 1;
            }
        }
        let now = Instant::now();
        for connection in &mut connections {
            if !matches!(
                connection.state,
                ConnectionState::Idle | ConnectionState::Dispatching | ConnectionState::Done
            ) && now >= connection.deadline
            {
                config.counters[config.node_index]
                    .timeouts
                    .fetch_add(1, Ordering::Relaxed);
                match connection.state {
                    ConnectionState::Connecting => &config.diagnostics.timeouts_connecting,
                    ConnectionState::Writing => &config.diagnostics.timeouts_writing,
                    ConnectionState::Reading => &config.diagnostics.timeouts_reading,
                    _ => &config.diagnostics.timeouts_reading,
                }
                .fetch_add(1, Ordering::Relaxed);
                if !connection.is_enabled(&config) {
                    config
                        .diagnostics
                        .timeouts_disabled
                        .fetch_add(1, Ordering::Relaxed);
                }
                if connection.state == ConnectionState::Reading {
                    if connection.response.is_empty() {
                        config
                            .diagnostics
                            .timeouts_reading_empty
                            .fetch_add(1, Ordering::Relaxed);
                    } else {
                        config
                            .diagnostics
                            .timeouts_reading_partial
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    let mut probe = [0u8; 1];
                    let peek = connection
                        .stream
                        .as_ref()
                        .map(|stream| stream.peek(&mut probe));
                    match peek {
                        Some(Ok(0)) => &config.diagnostics.timeouts_peek_eof,
                        Some(Ok(_)) => &config.diagnostics.timeouts_peek_data,
                        Some(Err(error)) if error.kind() == io::ErrorKind::WouldBlock => {
                            &config.diagnostics.timeouts_peek_blocked
                        }
                        _ => &config.diagnostics.timeouts_peek_error,
                    }
                    .fetch_add(1, Ordering::Relaxed);
                }
                if connection.state == ConnectionState::Writing
                    && connection.written < connection.request.len()
                {
                    config
                        .diagnostics
                        .timeouts_writing_partial
                        .fetch_add(1, Ordering::Relaxed);
                }
                if connection.attempts == 0 {
                    config
                        .diagnostics
                        .timeouts_first_attempt
                        .fetch_add(1, Ordering::Relaxed);
                } else {
                    config
                        .diagnostics
                        .timeouts_retry
                        .fetch_add(1, Ordering::Relaxed);
                }
                let was_done = connection.state == ConnectionState::Done;
                fail_or_retry(connection, &config, poll.registry(), now)?;
                if !was_done && connection.state == ConnectionState::Done {
                    done += 1;
                }
            }
        }
        config
            .diagnostics
            .max_loop_work_nanos
            .fetch_max(work_started.elapsed().as_nanos().min(u64::MAX as u128) as u64, Ordering::Relaxed);
    }
    Ok(())
}

impl Connection {
    fn new(
        token: Token,
        slot_rank: usize,
        config: &ReactorConfig,
        now: Instant,
        registry: &mio::Registry,
    ) -> Result<Self, String> {
        let endpoint_index = slot_rank % config.endpoints.len();
        let mut connection = Self {
            token,
            activation_rank: slot_rank,
            endpoint_index,
            address_index: slot_rank,
            stream: None,
            state: ConnectionState::Idle,
            task_id: 0,
            attempts: 0,
            request: Vec::with_capacity(160),
            written: 0,
            response: Vec::with_capacity(8 * 1024),
            meta: None,
            pending_dispatch: None,
            deadline: now + config.timeout,
        };
        connection.activate_if_allowed(config, registry, now)?;
        Ok(connection)
    }

    fn is_enabled(&self, config: &ReactorConfig) -> bool {
        let active = config.active_connections_by_node[config.node_index].load(Ordering::Acquire);
        // Distribute the node quota over every reactor on that node. Comparing
        // a node-global slot rank against the quota would leave all but the
        // first few reactors permanently idle.
        self.activation_rank < reactor_connection_quota(active, config.node_reactors, config.local_reactor)
    }

    fn claim_task(&mut self, config: &ReactorConfig) -> bool {
        let task_id = config.next_task.fetch_add(1, Ordering::Relaxed);
        if task_id >= config.total_tasks {
            return false;
        }
        self.task_id = task_id;
        self.endpoint_index = task_id as usize % config.endpoints.len();
        true
    }

    fn activate_if_allowed(
        &mut self,
        config: &ReactorConfig,
        registry: &mio::Registry,
        now: Instant,
    ) -> Result<(), String> {
        if self.state != ConnectionState::Idle {
            return Ok(());
        }
        if config.next_task.load(Ordering::Acquire) >= config.total_tasks {
            self.state = ConnectionState::Done;
            return Ok(());
        }
        if !self.is_enabled(config) {
            return Ok(());
        }
        if !self.claim_task(config) {
            self.state = ConnectionState::Done;
            return Ok(());
        }
        self.attempts = 0;
        self.prepare_request(config);
        self.connect(config, registry, now)
    }

    fn prepare_request(&mut self, config: &ReactorConfig) {
        let endpoint = &config.endpoints[self.endpoint_index];
        let page_id = if config.fixture_pages == 0 {
            self.task_id
        } else {
            self.task_id % config.fixture_pages
        };
        self.request.clear();
        let prefix = endpoint.base_url.path().trim_end_matches('/');
        self.request.extend_from_slice(b"GET ");
        if !prefix.is_empty() {
            self.request.extend_from_slice(prefix.as_bytes());
        }
        self.request.extend_from_slice(b"/p/");
        append_decimal(&mut self.request, page_id);
        self.request.extend_from_slice(b" HTTP/1.1\r\nHost: ");
        self.request
            .extend_from_slice(endpoint.host_header.as_bytes());
        self.request.extend_from_slice(
            b"\r\nAccept: text/html\r\nAccept-Encoding: identity\r\nConnection: keep-alive\r\n\r\n",
        );
        self.written = 0;
        self.response.clear();
        self.meta = None;
    }

    fn connect(
        &mut self,
        config: &ReactorConfig,
        registry: &mio::Registry,
        now: Instant,
    ) -> Result<(), String> {
        let endpoint = &config.endpoints[self.endpoint_index];
        let address = endpoint.addresses[self.address_index % endpoint.addresses.len()];
        self.address_index = self.address_index.wrapping_add(1);
        let mut stream = TcpStream::connect(address).map_err(|error| error.to_string())?;
        let _ = stream.set_nodelay(true);
        registry
            .register(
                &mut stream,
                self.token,
                Interest::READABLE | Interest::WRITABLE,
            )
            .map_err(|error| error.to_string())?;
        self.stream = Some(stream);
        self.state = ConnectionState::Connecting;
        self.deadline = now + config.timeout;
        config.counters[config.node_index]
            .connects
            .fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

fn append_decimal(output: &mut Vec<u8>, mut value: u64) {
    let mut digits = [0u8; 20];
    let mut cursor = digits.len();
    loop {
        cursor -= 1;
        digits[cursor] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    output.extend_from_slice(&digits[cursor..]);
}

fn drive_connection(
    connection: &mut Connection,
    event: &Event,
    config: &ReactorConfig,
    registry: &mio::Registry,
    content_workspace: &mut pagesift_content::ProcessWorkspace,
    fixture_options: &[Vec<pagesift_content::Options>],
) -> Result<(), String> {
    let now = Instant::now();
    if event.is_error() {
        return fail_or_retry(connection, config, registry, now);
    }
    if event.is_writable()
        && matches!(
            connection.state,
            ConnectionState::Connecting | ConnectionState::Writing
        )
    {
        if connection.state == ConnectionState::Connecting {
            let error = connection
                .stream
                .as_ref()
                .and_then(|stream| stream.take_error().ok())
                .flatten();
            if error.is_some() {
                return fail_or_retry(connection, config, registry, now);
            }
            connection.state = ConnectionState::Writing;
        }
        match write_request(connection) {
            Ok(true) => {
                connection.state = ConnectionState::Reading;
                connection.deadline = now + config.timeout;
                if let Some(stream) = connection.stream.as_mut() {
                    registry
                        .reregister(stream, connection.token, Interest::READABLE)
                        .map_err(|error| error.to_string())?;
                }
            }
            Ok(false) => {}
            Err(_) => return fail_or_retry(connection, config, registry, now),
        }
    }
    if event.is_readable() && connection.state == ConnectionState::Reading {
        match read_response(connection, config.max_response_bytes) {
            Ok(ReadOutcome::Pending) => {}
            Ok(ReadOutcome::Complete { body, close_after }) => {
                if let Some(sender) = &config.parse_sender {
                    let inline_percent = config.inline_parse_percent.load(Ordering::Relaxed);
                    let controller_inline = inline_percent > 0
                        && (connection.task_id.wrapping_mul(37) % 100) < inline_percent as u64;
                    // If the local content lane is saturated, use the owning
                    // reactor as a parser for this page instead of sleeping the
                    // socket. The decision is immediate and NUMA-local.
                    if controller_inline || sender.is_full() {
                        config.counters[config.node_index]
                            .queue_backpressure
                            .fetch_add(u64::from(sender.is_full()), Ordering::Relaxed);
                        config.counters[config.node_index]
                            .reactor_inline_parsed
                            .fetch_add(1, Ordering::Relaxed);
                        let parse_started = Instant::now();
                        let (valid, body_len) = process_body(
                            connection,
                            config,
                            &body,
                            content_workspace,
                            fixture_options,
                        );
                        config.diagnostics.inline_parse_nanos.fetch_add(
                            parse_started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                            Ordering::Relaxed,
                        );
                        return finish_request(
                            connection,
                            config,
                            registry,
                            valid,
                            body_len,
                            close_after,
                            now,
                        );
                    }
                    let job = take_parse_job(connection, config, body);
                    config.counters[config.node_index]
                        .content_queued
                        .fetch_add(1, Ordering::Release);
                    match sender.try_send(job) {
                        Ok(()) => {
                            return advance_request(connection, config, registry, close_after, now);
                        }
                        Err(TrySendError::Full(job)) => {
                            config.counters[config.node_index]
                                .queue_backpressure
                                .fetch_add(1, Ordering::Relaxed);
                            config.counters[config.node_index]
                                .content_queued
                                .fetch_sub(1, Ordering::Release);
                            connection.pending_dispatch =
                                Some(PendingDispatch { job, close_after });
                            connection.state = ConnectionState::Dispatching;
                            return Ok(());
                        }
                        Err(TrySendError::Disconnected(_)) => {
                            config.counters[config.node_index]
                                .content_queued
                                .fetch_sub(1, Ordering::Release);
                            return Err("NUMA content queue disconnected".to_string());
                        }
                    }
                } else {
                    let (valid, body_len) = process_body(
                        connection,
                        config,
                        &body,
                        content_workspace,
                        fixture_options,
                    );
                    return finish_request(
                        connection,
                        config,
                        registry,
                        valid,
                        body_len,
                        close_after,
                        now,
                    );
                }
            }
            Ok(ReadOutcome::Closed) | Err(_) => {
                return fail_or_retry(connection, config, registry, now);
            }
        }
    }
    if (event.is_read_closed() || event.is_write_closed())
        && connection.state != ConnectionState::Done
    {
        return fail_or_retry(connection, config, registry, now);
    }
    Ok(())
}

fn take_parse_job(
    connection: &mut Connection,
    config: &ReactorConfig,
    body: CompletedBody,
) -> ParseJob {
    match body {
        CompletedBody::ResponseRange(range) => {
            let recycled =
                config
                    .recycle_receiver
                    .as_ref()
                    .and_then(|receiver| match receiver.try_recv() {
                        Ok(mut bytes) => {
                            bytes.clear();
                            Some(bytes)
                        }
                        Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
                    });
            let replacement = recycled.unwrap_or_else(|| {
                config.counters[config.node_index]
                    .response_buffer_allocations
                    .fetch_add(1, Ordering::Relaxed);
                Vec::with_capacity(connection.response.capacity().max(8 * 1024))
            });
            ParseJob {
                bytes: std::mem::replace(&mut connection.response, replacement),
                body: range,
                endpoint_index: connection.endpoint_index,
                task_id: connection.task_id,
                source_node: config.node_index,
                recycler: config
                    .recycle_sender
                    .as_ref()
                    .expect("queued parse job requires a recycler")
                    .clone(),
            }
        }
        CompletedBody::Decoded(bytes) => {
            let body = 0..bytes.len();
            ParseJob {
                bytes,
                body,
                endpoint_index: connection.endpoint_index,
                task_id: connection.task_id,
                source_node: config.node_index,
                recycler: config
                    .recycle_sender
                    .as_ref()
                    .expect("queued parse job requires a recycler")
                    .clone(),
            }
        }
    }
}

fn flush_pending_dispatch(
    connection: &mut Connection,
    config: &ReactorConfig,
    registry: &mio::Registry,
    now: Instant,
) -> Result<bool, String> {
    let Some(pending) = connection.pending_dispatch.take() else {
        return Ok(false);
    };
    let sender = config
        .parse_sender
        .as_ref()
        .ok_or_else(|| "dispatching connection has no content queue".to_string())?;
    config.counters[config.node_index]
        .content_queued
        .fetch_add(1, Ordering::Release);
    match sender.try_send(pending.job) {
        Ok(()) => {
            advance_request(connection, config, registry, pending.close_after, now)?;
            Ok(true)
        }
        Err(TrySendError::Full(job)) => {
            config.counters[config.node_index]
                .content_queued
                .fetch_sub(1, Ordering::Release);
            config.counters[config.node_index]
                .queue_backpressure
                .fetch_add(1, Ordering::Relaxed);
            connection.pending_dispatch = Some(PendingDispatch {
                job,
                close_after: pending.close_after,
            });
            Ok(false)
        }
        Err(TrySendError::Disconnected(_)) => {
            config.counters[config.node_index]
                .content_queued
                .fetch_sub(1, Ordering::Release);
            Err("NUMA content queue disconnected".to_string())
        }
    }
}

fn write_request(connection: &mut Connection) -> io::Result<bool> {
    let Some(stream) = connection.stream.as_mut() else {
        return Err(io::Error::new(io::ErrorKind::NotConnected, "no socket"));
    };
    while connection.written < connection.request.len() {
        match stream.write(&connection.request[connection.written..]) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "socket closed")),
            Ok(written) => connection.written += written,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}

enum ReadOutcome {
    Pending,
    Complete {
        body: CompletedBody,
        close_after: bool,
    },
    Closed,
}

enum CompletedBody {
    /// Body remains in the connection receive buffer; no per-response copy.
    ResponseRange(Range<usize>),
    /// Chunked framing must currently be decoded into a contiguous buffer.
    Decoded(Vec<u8>),
}

impl CompletedBody {
    fn as_slice<'a>(&'a self, connection: &'a Connection) -> &'a [u8] {
        match self {
            Self::ResponseRange(range) => &connection.response[range.clone()],
            Self::Decoded(body) => body,
        }
    }
}

fn read_response(
    connection: &mut Connection,
    max_response_bytes: usize,
) -> Result<ReadOutcome, ProtocolError> {
    let Some(stream) = connection.stream.as_mut() else {
        return Ok(ReadOutcome::Closed);
    };
    let mut scratch = [0u8; READ_CHUNK];
    let mut closed = false;
    loop {
        match stream.read(&mut scratch) {
            Ok(0) => {
                closed = true;
                break;
            }
            Ok(read) => {
                if connection.response.len() + read > max_response_bytes {
                    return Err(ProtocolError::ResponseTooLarge);
                }
                connection.response.extend_from_slice(&scratch[..read]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Ok(ReadOutcome::Closed),
        }
    }
    if connection.meta.is_none() {
        connection.meta = parse_response_meta(&connection.response)?;
    }
    let Some(meta) = connection.meta else {
        return if closed {
            Ok(ReadOutcome::Closed)
        } else {
            Ok(ReadOutcome::Pending)
        };
    };
    if !meta.success_status {
        return Err(ProtocolError::InvalidResponse("non-success HTTP status"));
    }
    let encoded_body = &connection.response[meta.header_len..];
    match meta.framing {
        BodyFraming::ContentLength(length) if encoded_body.len() >= length => {
            Ok(ReadOutcome::Complete {
                body: CompletedBody::ResponseRange(meta.header_len..meta.header_len + length),
                close_after: meta.close_after,
            })
        }
        BodyFraming::ContentLength(_) => {
            if closed {
                Ok(ReadOutcome::Closed)
            } else {
                Ok(ReadOutcome::Pending)
            }
        }
        BodyFraming::Chunked => match decode_chunked(encoded_body)? {
            Some(body) => Ok(ReadOutcome::Complete {
                body: CompletedBody::Decoded(body),
                close_after: meta.close_after,
            }),
            None if closed => Ok(ReadOutcome::Closed),
            None => Ok(ReadOutcome::Pending),
        },
        BodyFraming::CloseDelimited if closed => Ok(ReadOutcome::Complete {
            body: CompletedBody::ResponseRange(meta.header_len..connection.response.len()),
            close_after: true,
        }),
        BodyFraming::CloseDelimited => Ok(ReadOutcome::Pending),
    }
}

fn parse_response_meta(input: &[u8]) -> Result<Option<ResponseMeta>, ProtocolError> {
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut response = httparse::Response::new(&mut headers);
    let status = response.parse(input).map_err(ProtocolError::Header)?;
    let httparse::Status::Complete(header_len) = status else {
        return Ok(None);
    };
    let status_code = response
        .code
        .ok_or(ProtocolError::InvalidResponse("missing HTTP status"))?;
    let mut content_length = None;
    let mut chunked = false;
    let mut close_after = false;
    for header in response.headers.iter() {
        if header.name.eq_ignore_ascii_case("content-length") {
            let value = std::str::from_utf8(header.value)
                .map_err(|_| ProtocolError::InvalidResponse("invalid Content-Length"))?;
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| ProtocolError::InvalidResponse("invalid Content-Length"))?,
            );
        } else if header.name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = header
                .value
                .split(|byte| *byte == b',')
                .any(|value| value.trim_ascii().eq_ignore_ascii_case(b"chunked"));
        } else if header.name.eq_ignore_ascii_case("connection") {
            close_after = header.value.eq_ignore_ascii_case(b"close");
        }
    }
    let framing = if chunked {
        BodyFraming::Chunked
    } else if let Some(length) = content_length {
        BodyFraming::ContentLength(length)
    } else {
        BodyFraming::CloseDelimited
    };
    Ok(Some(ResponseMeta {
        header_len,
        framing,
        close_after,
        success_status: (200..300).contains(&status_code),
    }))
}

fn decode_chunked(input: &[u8]) -> Result<Option<Vec<u8>>, ProtocolError> {
    let mut cursor = 0usize;
    let mut decoded = Vec::new();
    loop {
        let Some(line_end) = find_bytes(&input[cursor..], b"\r\n") else {
            return Ok(None);
        };
        let line_end = cursor + line_end;
        let size_field = input[cursor..line_end]
            .split(|byte| *byte == b';')
            .next()
            .unwrap_or_default();
        let size_text = std::str::from_utf8(size_field)
            .map_err(|_| ProtocolError::InvalidResponse("invalid chunk size"))?;
        let size = usize::from_str_radix(size_text.trim(), 16)
            .map_err(|_| ProtocolError::InvalidResponse("invalid chunk size"))?;
        cursor = line_end + 2;
        if size == 0 {
            if input.len() < cursor + 2 {
                return Ok(None);
            }
            if &input[cursor..cursor + 2] == b"\r\n" {
                return Ok(Some(decoded));
            }
            let Some(trailer_end) = find_bytes(&input[cursor..], b"\r\n\r\n") else {
                return Ok(None);
            };
            let _ = trailer_end;
            return Ok(Some(decoded));
        }
        if input.len() < cursor + size + 2 {
            return Ok(None);
        }
        decoded.extend_from_slice(&input[cursor..cursor + size]);
        cursor += size;
        if &input[cursor..cursor + 2] != b"\r\n" {
            return Err(ProtocolError::InvalidResponse("invalid chunk terminator"));
        }
        cursor += 2;
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn build_local_fixture_options(endpoints: &[Endpoint]) -> Vec<Vec<pagesift_content::Options>> {
    endpoints
        .iter()
        .map(|endpoint| {
            endpoint
                .fixture_urls
                .iter()
                .cloned()
                .map(|url| pagesift_content::Options {
                    base_url: Some(url),
                    ..Default::default()
                })
                .collect()
        })
        .collect()
}

fn process_body(
    connection: &Connection,
    config: &ReactorConfig,
    body: &CompletedBody,
    workspace: &mut pagesift_content::ProcessWorkspace,
    fixture_options: &[Vec<pagesift_content::Options>],
) -> (bool, usize) {
    let body = body.as_slice(connection);
    let endpoint = &config.endpoints[connection.endpoint_index];
    let page_id = if config.fixture_pages == 0 {
        connection.task_id
    } else {
        connection.task_id % config.fixture_pages
    };
    let valid = if config.process_content {
        std::str::from_utf8(body)
            .ok()
            .map(|html| {
                if config.fixture_pages == 0 {
                    let options = pagesift_content::Options {
                        base_url: endpoint.base_url.join(&format!("p/{page_id}")).ok(),
                        ..Default::default()
                    };
                    !pagesift_content::process_with_workspace(html, &options, workspace)
                        .markdown
                        .is_empty()
                } else {
                    fixture_options
                        .get(connection.endpoint_index)
                        .and_then(|options| options.get(page_id as usize))
                        .map(|options| {
                            !pagesift_content::process_with_workspace(html, options, workspace)
                                .markdown
                                .is_empty()
                        })
                        .unwrap_or(false)
                }
            })
            .unwrap_or(false)
    } else {
        !body.is_empty()
    };
    record_sample(
        connection.task_id,
        body,
        config.sample_every,
        &config.samples,
    );
    (valid, body.len())
}

fn record_sample(task_id: u64, body: &[u8], sample_every: u64, samples: &Mutex<Vec<SampleRecord>>) {
    if sample_every == 0 || task_id % sample_every != 0 {
        return;
    }
    samples
        .lock()
        .expect("sample ledger lock poisoned")
        .push(SampleRecord {
            task_id,
            body_bytes: body.len(),
            body_blake3: blake3::hash(body).to_hex().to_string(),
        });
}

#[allow(clippy::too_many_arguments)]
fn run_content_worker(
    node_index: usize,
    worker_index: usize,
    primary_lane: usize,
    cpu_set: Vec<usize>,
    receiver: Receiver<ParseJob>,
    all_receivers: Arc<Vec<Vec<Receiver<ParseJob>>>>,
    endpoints: Arc<Vec<Endpoint>>,
    fixture_pages: u64,
    counters: Arc<Vec<NodeCounters>>,
    started: Instant,
    finished_at_ns: Arc<AtomicU64>,
    active_content_by_node: Arc<Vec<AtomicUsize>>,
    local_batch_size: Arc<AtomicUsize>,
    empty_polls_before_steal: Arc<AtomicUsize>,
    steal_batch_size: Arc<AtomicUsize>,
    adaptive_distribution: bool,
    stop: Arc<AtomicBool>,
    sample_every: u64,
    samples: Arc<Mutex<Vec<SampleRecord>>>,
) -> Result<(), String> {
    if !pin_current_thread_to_set(&cpu_set) {
        return Err(format!(
            "content worker for node {node_index} failed to apply NUMA affinity"
        ));
    }
    let mut workspace = pagesift_content::ProcessWorkspace::default();
    let fixture_options = build_local_fixture_options(&endpoints);
    let mut jobs = Vec::with_capacity(16);
    let mut empty_polls = 0usize;
    let mut steal_cursor = worker_index;
    loop {
        while worker_index >= active_content_by_node[node_index].load(Ordering::Acquire)
            && !stop.load(Ordering::Acquire)
        {
            std::thread::sleep(Duration::from_millis(1));
        }
        jobs.clear();
        match receiver.recv_timeout(Duration::from_millis(1)) {
            Ok(job) => {
                empty_polls = 0;
                jobs.push(job);
                let target = local_batch_size.load(Ordering::Relaxed).clamp(1, 16);
                while jobs.len() < target {
                    match receiver.try_recv() {
                        Ok(job) => jobs.push(job),
                        Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                    }
                }
                counters[node_index]
                    .local_batches
                    .fetch_add(1, Ordering::Relaxed);
                counters[node_index]
                    .local_jobs
                    .fetch_add(jobs.len() as u64, Ordering::Relaxed);
            }
            Err(RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Acquire)
                    && counters
                        .iter()
                        .map(|node| node.content_queued.load(Ordering::Acquire))
                        .sum::<u64>()
                        == 0
                {
                    return Ok(());
                }
                if !adaptive_distribution {
                    continue;
                }
                // Help another shard on the same NUMA node before declaring
                // local starvation. This preserves memory locality while
                // removing the old Reactor-to-worker queue island.
                let local_lanes = &all_receivers[node_index];
                let local_target = local_batch_size.load(Ordering::Relaxed).clamp(1, 16);
                for offset in 1..local_lanes.len() {
                    let lane = (primary_lane + worker_index + offset) % local_lanes.len();
                    if lane == primary_lane {
                        continue;
                    }
                    if let Ok(job) = local_lanes[lane].try_recv() {
                        jobs.push(job);
                        while jobs.len() < local_target {
                            match local_lanes[lane].try_recv() {
                                Ok(job) => jobs.push(job),
                                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                            }
                        }
                        break;
                    }
                }
                if !jobs.is_empty() {
                    empty_polls = 0;
                    counters[node_index]
                        .local_batches
                        .fetch_add(1, Ordering::Relaxed);
                    counters[node_index]
                        .local_jobs
                        .fetch_add(jobs.len() as u64, Ordering::Relaxed);
                    counters[node_index]
                        .local_steal_jobs
                        .fetch_add(jobs.len() as u64, Ordering::Relaxed);
                } else {
                    empty_polls = empty_polls.saturating_add(1);
                    let threshold = empty_polls_before_steal.load(Ordering::Relaxed).max(1);
                    if empty_polls < threshold || all_receivers.len() <= 1 {
                        continue;
                    }
                    counters[node_index]
                        .steal_attempts
                        .fetch_add(1, Ordering::Relaxed);
                    for offset in 1..all_receivers.len() {
                        let victim = (node_index + steal_cursor + offset) % all_receivers.len();
                        if victim == node_index {
                            continue;
                        }
                        for lane_offset in 0..all_receivers[victim].len() {
                            let lane = (worker_index + lane_offset) % all_receivers[victim].len();
                            let victim_receiver = &all_receivers[victim][lane];
                            if let Ok(job) = victim_receiver.try_recv() {
                                jobs.push(job);
                                let target = steal_batch_size.load(Ordering::Relaxed).clamp(1, 16);
                                while jobs.len() < target {
                                    match victim_receiver.try_recv() {
                                        Ok(job) => jobs.push(job),
                                        Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                                            break;
                                        }
                                    }
                                }
                                break;
                            }
                        }
                        if !jobs.is_empty() {
                            steal_cursor = (victim + 1) % all_receivers.len();
                            break;
                        }
                    }
                    empty_polls = 0;
                    if jobs.is_empty() {
                        continue;
                    }
                    counters[node_index]
                        .steal_successes
                        .fetch_add(1, Ordering::Relaxed);
                    counters[node_index]
                        .stolen_jobs
                        .fetch_add(jobs.len() as u64, Ordering::Relaxed);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }

        for mut job in jobs.drain(..) {
            counters[job.source_node]
                .content_queued
                .fetch_sub(1, Ordering::Release);
            let (valid, body_len) = process_parse_job(
                &job,
                &endpoints,
                fixture_pages,
                &fixture_options,
                &mut workspace,
            )?;
            if let Some(body) = job.bytes.get(job.body.clone()) {
                record_sample(job.task_id, body, sample_every, &samples);
            }
            let node = &counters[node_index];
            node.completed.fetch_add(1, Ordering::Relaxed);
            node.bytes.fetch_add(body_len as u64, Ordering::Relaxed);
            if valid {
                node.valid.fetch_add(1, Ordering::Relaxed);
            } else {
                node.failed.fetch_add(1, Ordering::Relaxed);
                node.content_failed.fetch_add(1, Ordering::Relaxed);
            }
            finished_at_ns.fetch_max(
                started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                Ordering::Release,
            );
            job.bytes.clear();
            let _ = job.recycler.try_send(job.bytes);
        }
    }
}

fn process_parse_job(
    job: &ParseJob,
    endpoints: &[Endpoint],
    fixture_pages: u64,
    fixture_options: &[Vec<pagesift_content::Options>],
    workspace: &mut pagesift_content::ProcessWorkspace,
) -> Result<(bool, usize), String> {
    let body = job
        .bytes
        .get(job.body.clone())
        .ok_or_else(|| "content job body range is outside its response buffer".to_string())?;
    let endpoint = endpoints
        .get(job.endpoint_index)
        .ok_or_else(|| "content job endpoint is missing".to_string())?;
    let page_id = if fixture_pages == 0 {
        job.task_id
    } else {
        job.task_id % fixture_pages
    };
    let valid = std::str::from_utf8(body)
        .ok()
        .map(|html| {
            if fixture_pages == 0 {
                let options = pagesift_content::Options {
                    base_url: endpoint.base_url.join(&format!("p/{page_id}")).ok(),
                    ..Default::default()
                };
                !pagesift_content::process_with_workspace(html, &options, workspace)
                    .markdown
                    .is_empty()
            } else {
                fixture_options
                    .get(job.endpoint_index)
                    .and_then(|options| options.get(page_id as usize))
                    .map(|options| {
                        !pagesift_content::process_with_workspace(html, options, workspace)
                            .markdown
                            .is_empty()
                    })
                    .unwrap_or(false)
            }
        })
        .unwrap_or(false);
    Ok((valid, body.len()))
}

#[allow(clippy::too_many_arguments)]
fn finish_request(
    connection: &mut Connection,
    config: &ReactorConfig,
    registry: &mio::Registry,
    valid: bool,
    body_len: usize,
    close_after: bool,
    now: Instant,
) -> Result<(), String> {
    let node = &config.counters[config.node_index];
    node.completed.fetch_add(1, Ordering::Relaxed);
    node.bytes.fetch_add(body_len as u64, Ordering::Relaxed);
    if valid {
        node.valid.fetch_add(1, Ordering::Relaxed);
    } else {
        node.failed.fetch_add(1, Ordering::Relaxed);
        node.content_failed.fetch_add(1, Ordering::Relaxed);
    }
    advance_request(connection, config, registry, close_after, now)
}

fn advance_request(
    connection: &mut Connection,
    config: &ReactorConfig,
    registry: &mio::Registry,
    close_after: bool,
    now: Instant,
) -> Result<(), String> {
    let node = &config.counters[config.node_index];
    connection.attempts = 0;
    if !connection.is_enabled(config) {
        close_connection(connection, registry);
        connection.state = ConnectionState::Idle;
        return Ok(());
    }
    if !connection.claim_task(config) {
        close_connection(connection, registry);
        connection.state = ConnectionState::Done;
        return Ok(());
    }
    connection.prepare_request(config);
    if close_after {
        close_connection(connection, registry);
        connection.connect(config, registry, now)
    } else {
        node.reused.fetch_add(1, Ordering::Relaxed);
        connection.state = ConnectionState::Writing;
        connection.deadline = now + config.timeout;
        // A keep-alive socket is normally still writable. Push the next
        // request immediately instead of adding a full epoll wakeup between
        // every response and its successor.
        match write_request(connection) {
            Ok(true) => {
                connection.state = ConnectionState::Reading;
                let stream = connection
                    .stream
                    .as_mut()
                    .ok_or_else(|| "keep-alive socket disappeared".to_string())?;
                registry
                    .reregister(stream, connection.token, Interest::READABLE)
                    .map_err(|error| error.to_string())
            }
            Ok(false) => {
                let stream = connection
                    .stream
                    .as_mut()
                    .ok_or_else(|| "keep-alive socket disappeared".to_string())?;
                registry
                    .reregister(
                        stream,
                        connection.token,
                        Interest::READABLE | Interest::WRITABLE,
                    )
                    .map_err(|error| error.to_string())
            }
            Err(_) => fail_or_retry(connection, config, registry, now),
        }
    }
}

fn fail_or_retry(
    connection: &mut Connection,
    config: &ReactorConfig,
    registry: &mio::Registry,
    now: Instant,
) -> Result<(), String> {
    close_connection(connection, registry);
    if connection.attempts < config.retries {
        connection.attempts += 1;
        config.counters[config.node_index]
            .retries
            .fetch_add(1, Ordering::Relaxed);
        connection.prepare_request(config);
        return connection.connect(config, registry, now);
    }
    let node = &config.counters[config.node_index];
    node.completed.fetch_add(1, Ordering::Relaxed);
    node.failed.fetch_add(1, Ordering::Relaxed);
    node.transport_failed.fetch_add(1, Ordering::Relaxed);
    connection.attempts = 0;
    if !connection.is_enabled(config) {
        connection.state = ConnectionState::Idle;
        return Ok(());
    }
    if !connection.claim_task(config) {
        connection.state = ConnectionState::Done;
        return Ok(());
    }
    connection.prepare_request(config);
    connection.connect(config, registry, now)
}

fn close_connection(connection: &mut Connection, registry: &mio::Registry) {
    if let Some(mut stream) = connection.stream.take() {
        let _ = registry.deregister(&mut stream);
    }
}

/// Evenly spreads one NUMA node's active-connection quota over its reactors.
/// The remainder goes to the lowest-indexed reactors so the sum is exact.
fn reactor_connection_quota(active: usize, node_reactors: usize, local_reactor: usize) -> usize {
    let node_reactors = node_reactors.max(1);
    let base = active / node_reactors;
    let remainder = active % node_reactors;
    base + usize::from(local_reactor < remainder)
}

fn distribute_by_weights(total: usize, weights: &[usize]) -> Vec<usize> {
    let weight_sum = weights.iter().sum::<usize>().max(1);
    let mut result = weights
        .iter()
        .map(|weight| total * *weight / weight_sum)
        .collect::<Vec<_>>();
    for value in &mut result {
        *value = (*value).max(1);
    }
    while result.iter().sum::<usize>() > total {
        if let Some((index, _)) = result
            .iter()
            .enumerate()
            .rev()
            .find(|(_, value)| **value > 1)
        {
            result[index] -= 1;
        } else {
            break;
        }
    }
    let mut cursor = 0usize;
    while result.iter().sum::<usize>() < total {
        let index = cursor % result.len();
        result[index] += 1;
        cursor += 1;
    }
    result
}

fn automatic_reactor_count(
    logical_cores: usize,
    physical_cores: usize,
    numa_nodes: usize,
    connections: usize,
) -> usize {
    let nodes = numa_nodes.max(1);
    let by_connection_depth = connections.max(nodes).div_ceil(32);
    let physical_budget = physical_cores.div_ceil(2).max(nodes);
    let raw = by_connection_depth.clamp(nodes, physical_budget);
    raw.div_ceil(nodes)
        .saturating_mul(nodes)
        .min(logical_cores.saturating_sub(nodes).max(nodes))
        .max(nodes)
}

fn distribute_evenly(total: usize, buckets: usize) -> Vec<usize> {
    (0..buckets)
        .map(|index| total / buckets + usize::from(index < total % buckets))
        .collect()
}

fn node_snapshot(node: &NodeCounters) -> Snapshot {
    Snapshot {
        completed: node.completed.load(Ordering::Relaxed),
        valid: node.valid.load(Ordering::Relaxed),
        failed: node.failed.load(Ordering::Relaxed),
        content_failed: node.content_failed.load(Ordering::Relaxed),
        transport_failed: node.transport_failed.load(Ordering::Relaxed),
        bytes: node.bytes.load(Ordering::Relaxed),
        connects: node.connects.load(Ordering::Relaxed),
        reused: node.reused.load(Ordering::Relaxed),
        retries: node.retries.load(Ordering::Relaxed),
        timeouts: node.timeouts.load(Ordering::Relaxed),
        queue_backpressure: node.queue_backpressure.load(Ordering::Relaxed),
        response_buffer_allocations: node.response_buffer_allocations.load(Ordering::Relaxed),
        content_queued: node.content_queued.load(Ordering::Relaxed),
        reactor_inline_parsed: node.reactor_inline_parsed.load(Ordering::Relaxed),
        local_batches: node.local_batches.load(Ordering::Relaxed),
        local_jobs: node.local_jobs.load(Ordering::Relaxed),
        local_steal_jobs: node.local_steal_jobs.load(Ordering::Relaxed),
        steal_attempts: node.steal_attempts.load(Ordering::Relaxed),
        steal_successes: node.steal_successes.load(Ordering::Relaxed),
        stolen_jobs: node.stolen_jobs.load(Ordering::Relaxed),
    }
}

fn snapshot(counters: &[NodeCounters]) -> Snapshot {
    counters
        .iter()
        .fold(Snapshot::default(), |mut total, node| {
            let current = node_snapshot(node);
            total.completed += current.completed;
            total.valid += current.valid;
            total.failed += current.failed;
            total.content_failed += current.content_failed;
            total.transport_failed += current.transport_failed;
            total.bytes += current.bytes;
            total.connects += current.connects;
            total.reused += current.reused;
            total.retries += current.retries;
            total.timeouts += current.timeouts;
            total.queue_backpressure += current.queue_backpressure;
            total.response_buffer_allocations += current.response_buffer_allocations;
            total.content_queued += current.content_queued;
            total.reactor_inline_parsed += current.reactor_inline_parsed;
            total.local_batches += current.local_batches;
            total.local_jobs += current.local_jobs;
            total.local_steal_jobs += current.local_steal_jobs;
            total.steal_attempts += current.steal_attempts;
            total.steal_successes += current.steal_successes;
            total.stolen_jobs += current.stolen_jobs;
            total
        })
}

fn read_process_cpu_ticks() -> Option<u64> {
    let stat = fs::read_to_string("/proc/self/stat").ok()?;
    let close = stat.rfind(')')?;
    let rest = stat
        .get(close + 1..)?
        .split_ascii_whitespace()
        .collect::<Vec<_>>();
    Some(rest.get(11)?.parse::<u64>().ok()? + rest.get(12)?.parse::<u64>().ok()?)
}

fn clock_ticks_per_second() -> f64 {
    let output = std::process::Command::new("getconf")
        .arg("CLK_TCK")
        .output();
    output
        .ok()
        .filter(|value| value.status.success())
        .and_then(|value| String::from_utf8(value.stdout).ok())
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| *value > 0.0)
        .unwrap_or(100.0)
}

fn read_peak_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))?
        .split_ascii_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    Some(kib * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream as StdTcpStream};
    use std::sync::atomic::AtomicUsize;

    static TEST_ID: AtomicUsize = AtomicUsize::new(0);

    fn read_request(stream: &mut StdTcpStream) {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 512];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0);
            request.extend_from_slice(&buffer[..read]);
        }
    }

    fn reactor_config(address: SocketAddr, tasks: u64, timeout: Duration) -> ReactorConfig {
        let topology = HardwareTopology::detect();
        let cpu = topology
            .cpu_topology
            .runtime_worker_cpus()
            .into_iter()
            .next()
            .unwrap_or(0);
        let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
        ReactorConfig {
            reactor_index: id,
            node_index: 0,
            local_reactor: 0,
            node_reactors: 1,
            cpu_set: vec![cpu],
            slot_ranks: vec![0],
            total_tasks: tasks,
            fixture_pages: 5,
            endpoints: Arc::new(vec![Endpoint {
                base_url: Url::parse(&format!("http://{address}")).unwrap(),
                addresses: vec![address],
                host_header: address.to_string(),
                fixture_urls: (0..5)
                    .map(|page_id| Url::parse(&format!("http://{address}/p/{page_id}")).unwrap())
                    .collect(),
            }]),
            timeout,
            retries: 1,
            max_response_bytes: 64 * 1024,
            process_content: false,
            parse_sender: None,
            recycle_sender: None,
            recycle_receiver: None,
            inline_parse_percent: Arc::new(AtomicUsize::new(0)),
            active_connections_by_node: Arc::new(vec![AtomicUsize::new(1)]),
            next_task: Arc::new(AtomicU64::new(0)),
            sample_every: 0,
            samples: Arc::new(Mutex::new(Vec::new())),
            counters: Arc::new(vec![NodeCounters::default()]),
            diagnostics: Arc::new(ReactorDiagnostics::default()),
        }
    }

    #[test]
    fn parses_content_length_response() {
        let input = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: keep-alive\r\n\r\nhello";
        let meta = parse_response_meta(input).unwrap().unwrap();
        assert_eq!(meta.framing, BodyFraming::ContentLength(5));
        assert!(!meta.close_after);
        assert!(meta.success_status);
        assert_eq!(&input[meta.header_len..], b"hello");
    }

    #[test]
    fn decodes_chunked_body_and_trailers() {
        let input = b"4\r\nWiki\r\n5;ext=yes\r\npedia\r\n0\r\nX-Test: yes\r\n\r\n";
        assert_eq!(decode_chunked(input).unwrap().unwrap(), b"Wikipedia");
    }

    #[test]
    fn incomplete_chunked_body_waits() {
        assert!(decode_chunked(b"5\r\nabc").unwrap().is_none());
    }

    #[test]
    fn weighted_and_even_distributions_are_exact() {
        assert_eq!(distribute_by_weights(192, &[48, 48, 48, 48]), vec![48; 4]);
        assert_eq!(distribute_evenly(10, 3), vec![4, 3, 3]);
    }

    #[test]
    fn connection_quota_is_spread_over_every_reactor_on_the_node() {
        // 1,536 active connections over 48 reactors means 32 per reactor,
        // not 1,536 connections piled onto the first six reactors.
        for active in [1_536usize, 4_438, 5_548, 12_288] {
            let quotas = (0..48)
                .map(|reactor| reactor_connection_quota(active, 48, reactor))
                .collect::<Vec<_>>();
            assert_eq!(quotas.iter().sum::<usize>(), active);
            assert!(quotas.iter().all(|quota| *quota > 0));
            let spread = quotas.iter().copied().max().unwrap() - quotas.iter().copied().min().unwrap();
            assert!(spread <= 1, "quota spread {spread} for {active}");
        }
        assert_eq!(reactor_connection_quota(384, 12, 0), 32);
        assert_eq!(reactor_connection_quota(384, 12, 11), 32);
    }

    #[test]
    fn automatic_split_reserves_workers_and_balances_numa_nodes() {
        assert_eq!(automatic_reactor_count(192, 96, 4, 1_536), 48);
        assert_eq!(automatic_reactor_count(16, 8, 2, 64), 2);
        assert_eq!(automatic_reactor_count(4, 2, 1, 10_000), 1);
    }

    #[test]
    fn atomic_task_claims_cover_tasks_once() {
        let total_tasks = 101u64;
        let slots = 16usize;
        let mut seen = vec![0u8; total_tasks as usize];
        let next = AtomicU64::new(0);
        for _ in 0..slots {
            loop {
                let id = next.fetch_add(1, Ordering::Relaxed);
                if id >= total_tasks {
                    break;
                }
                seen[id as usize] += 1;
            }
        }
        assert!(seen.into_iter().all(|count| count == 1));
    }

    #[test]
    fn formal_contract_rejects_manual_capacity() {
        let automatic =
            Opts::try_parse_from(["runner", "--formal", "--mode", "full", "--split-pipeline"])
                .unwrap();
        assert!(validate_run_contract(&automatic, true).is_ok());

        let manual = Opts::try_parse_from([
            "runner",
            "--formal",
            "--mode",
            "full",
            "--split-pipeline",
            "--connections",
            "1536",
        ])
        .unwrap();
        assert!(validate_run_contract(&manual, true).is_err());
    }

    #[test]
    fn deterministic_sample_ledger_hashes_only_selected_tasks() {
        let samples = Mutex::new(Vec::new());
        record_sample(9_999, b"skip", 10_000, &samples);
        record_sample(10_000, b"keep", 10_000, &samples);
        let records = samples.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].task_id, 10_000);
        assert_eq!(
            records[0].body_blake3,
            blake3::hash(b"keep").to_hex().to_string()
        );
    }

    #[test]
    fn reactor_reuses_one_keep_alive_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            for _ in 0..3 {
                read_request(&mut stream);
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .unwrap();
            }
        });
        let config = reactor_config(address, 3, Duration::from_secs(1));
        let counters = config.counters.clone();
        run_reactor(config).unwrap();
        server.join().unwrap();
        let result = node_snapshot(&counters[0]);
        assert_eq!(result.completed, 3);
        assert_eq!(result.valid, 3);
        assert_eq!(result.failed, 0);
        assert_eq!(result.connects, 1);
        assert_eq!(result.reused, 2);
    }

    #[test]
    fn reactor_retries_disconnect_and_decodes_chunked() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            read_request(&mut first);
            drop(first);
            let (mut second, _) = listener.accept().unwrap();
            read_request(&mut second);
            second
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\nok\r\n0\r\n\r\n",
                )
                .unwrap();
        });
        let config = reactor_config(address, 1, Duration::from_secs(1));
        let counters = config.counters.clone();
        run_reactor(config).unwrap();
        server.join().unwrap();
        let result = node_snapshot(&counters[0]);
        assert_eq!(result.completed, 1);
        assert_eq!(result.valid, 1);
        assert_eq!(result.failed, 0);
        assert_eq!(result.connects, 2);
        assert_eq!(result.retries, 1);
    }

    #[test]
    fn reactor_times_out_and_records_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                read_request(&mut stream);
                std::thread::sleep(Duration::from_millis(80));
            }
        });
        let config = reactor_config(address, 1, Duration::from_millis(30));
        let counters = config.counters.clone();
        run_reactor(config).unwrap();
        server.join().unwrap();
        let result = node_snapshot(&counters[0]);
        assert_eq!(result.completed, 1);
        assert_eq!(result.valid, 0);
        assert_eq!(result.failed, 1);
        assert_eq!(result.retries, 1);
        assert_eq!(result.timeouts, 2);
    }
}
