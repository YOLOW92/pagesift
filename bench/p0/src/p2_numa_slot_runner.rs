//! P2 Step 2: fixed-slot, NUMA-local executor experiment.
//!
//! The HTTP and content components intentionally match `p2_component_probe`.
//! Only execution changes: one runtime per NUMA domain, workers pinned to that
//! domain, fixed long-lived slots, and deterministic task ownership.

use clap::Parser;
use pagesift_policy::adaptive::{
    HardwareTopology, LocalityCluster, pin_current_thread, pin_current_thread_to_set,
};
use serde::Serialize;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use url::Url;

#[derive(Debug, Parser)]
#[command(about = "PageSift P2 NUMA-local fixed-slot runner")]
struct Opts {
    #[arg(long, default_value = "http://127.0.0.1:19800")]
    origin_url: String,
    #[arg(long, default_value_t = 100_000_000)]
    total_tasks: u64,
    /// Fixed long-lived request slots. Zero uses topology base concurrency.
    #[arg(long, default_value_t = 0)]
    concurrency: usize,
    /// Independent connection pools. Zero uses the physical-core count.
    #[arg(long, default_value_t = 0)]
    shards: usize,
    /// Add SMT siblings as executor workers. Physical cores are the default.
    #[arg(long, default_value_t = false)]
    use_smt: bool,
    /// Pin each worker to an exact CPU instead of its NUMA-local CPU set.
    #[arg(long, default_value_t = false)]
    exact_pin: bool,
    #[arg(long, default_value_t = 1_000)]
    telemetry_interval_ms: u64,
    /// Reuse this many deterministic page fixtures. Zero uses a unique page id.
    /// Step-2 A/B defaults to the same five fixtures as p2_component_probe.
    #[arg(long, default_value_t = 5)]
    fixture_pages: u64,
    #[arg(long, default_value = "p2_numa_slot_runner.json")]
    output: PathBuf,
}

#[repr(align(128))]
#[derive(Default)]
struct NodeCounters {
    completed: AtomicU64,
    valid: AtomicU64,
    failed: AtomicU64,
    bytes: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
struct Snapshot {
    completed: u64,
    valid: u64,
    failed: u64,
    bytes: u64,
}

#[derive(Debug, Serialize)]
struct NodeReport {
    node_index: usize,
    numa_node: usize,
    socket_id: usize,
    cpus: usize,
    slots: usize,
    shards: usize,
    completed: u64,
    valid: u64,
    failed: u64,
    bytes: u64,
}

#[derive(Debug, Serialize)]
struct FinalReport {
    schema: u32,
    executor: &'static str,
    total_tasks: u64,
    completed: u64,
    valid: u64,
    failed: u64,
    bytes: u64,
    wall_time_s: f64,
    sustained_qps: f64,
    throughput_mb_s: f64,
    process_cpu_time_s: f64,
    process_cpu_cores: f64,
    process_cpu_pct_of_assigned: f64,
    assigned_cpus: usize,
    runtime_workers: usize,
    use_smt: bool,
    exact_pin: bool,
    physical_cores: usize,
    sockets: usize,
    numa_nodes: usize,
    concurrency: usize,
    shards: usize,
    fixture_pages: u64,
    node_reports: Vec<NodeReport>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = Opts::parse();
    let topology = HardwareTopology::detect();
    let clusters = topology.cpu_topology.clusters.clone();
    if clusters.is_empty() {
        return Err("no NUMA locality clusters detected".into());
    }
    let node_count = clusters.len();
    let concurrency = if opts.concurrency == 0 {
        topology.base_concurrency
    } else {
        opts.concurrency
    }
    .max(node_count);
    let shards = if opts.shards == 0 {
        topology.physical_cores
    } else {
        opts.shards
    }
    .clamp(node_count, concurrency);
    let worker_counts = clusters
        .iter()
        .map(|cluster| {
            if opts.use_smt {
                cluster.logical_cpus().count()
            } else {
                cluster.physical_cpus.len()
            }
        })
        .collect::<Vec<_>>();
    let runtime_workers = worker_counts.iter().sum::<usize>();
    let slot_counts = distribute_by_weights(concurrency, &worker_counts);
    let shard_counts = distribute_by_weights(shards, &worker_counts);
    let slot_offsets = prefix_offsets(&slot_counts);
    let counters = Arc::new(
        (0..node_count)
            .map(|_| NodeCounters::default())
            .collect::<Vec<_>>(),
    );

    println!("============================================================");
    println!("  PageSift P2 NUMA-Local Fixed-Slot Runner");
    println!(
        "  Topology: {} logical / {} physical / {} sockets / {} NUMA",
        topology.logical_cores, topology.physical_cores, topology.sockets, topology.numa_nodes
    );
    println!("  Tasks: {}", opts.total_tasks);
    println!(
        "  Fixed slots: {} / connection pools: {}",
        concurrency, shards
    );
    for (index, cluster) in clusters.iter().enumerate() {
        println!(
            "  Node {} (NUMA {}, socket {}): {} CPUs / {} slots / {} pools",
            index,
            cluster.numa_node,
            cluster.socket_id,
            worker_counts[index],
            slot_counts[index],
            shard_counts[index]
        );
    }
    println!("============================================================");

    let cpu_start = read_process_cpu_ticks().unwrap_or(0);
    let started = Instant::now();
    let mut node_threads = Vec::with_capacity(node_count);
    for (node_index, cluster) in clusters.iter().cloned().enumerate() {
        let origin = opts.origin_url.clone();
        let counters = counters.clone();
        let total_tasks = opts.total_tasks;
        let total_slots = concurrency;
        let slots = slot_counts[node_index];
        let slot_offset = slot_offsets[node_index];
        let local_shards = shard_counts[node_index];
        let fixture_pages = opts.fixture_pages;
        let use_smt = opts.use_smt;
        let exact_pin = opts.exact_pin;
        node_threads.push(
            std::thread::Builder::new()
                .name(format!("p2-node-{node_index}"))
                .spawn(move || {
                    run_node(
                        node_index,
                        cluster,
                        origin,
                        total_tasks,
                        total_slots,
                        slot_offset,
                        slots,
                        local_shards,
                        fixture_pages,
                        use_smt,
                        exact_pin,
                        counters,
                    )
                })?,
        );
    }

    let telemetry_interval = Duration::from_millis(opts.telemetry_interval_ms.max(100));
    let mut last = Snapshot::default();
    let mut sampled_at = Instant::now();
    loop {
        std::thread::sleep(telemetry_interval);
        let current = snapshot(&counters);
        let elapsed = sampled_at.elapsed().as_secs_f64().max(1e-9);
        let qps = current.completed.saturating_sub(last.completed) as f64 / elapsed;
        let cpu_ticks = read_process_cpu_ticks()
            .unwrap_or(cpu_start)
            .saturating_sub(cpu_start);
        let cpu_time = cpu_ticks as f64 / clock_ticks_per_second();
        let avg_cores = cpu_time / started.elapsed().as_secs_f64().max(1e-9);
        println!(
            "[{:.2}%] {:>9.1} pages/s | completed {} | failed {} | process CPU {:>6.2} cores",
            current.completed as f64 / opts.total_tasks.max(1) as f64 * 100.0,
            qps,
            current.completed,
            current.failed,
            avg_cores,
        );
        last = current;
        sampled_at = Instant::now();
        if current.completed >= opts.total_tasks {
            break;
        }
    }

    for thread in node_threads {
        match thread.join() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => return Err("NUMA executor thread panicked".into()),
        }
    }

    let wall_time_s = started.elapsed().as_secs_f64();
    let final_snapshot = snapshot(&counters);
    let process_cpu_ticks = read_process_cpu_ticks()
        .unwrap_or(cpu_start)
        .saturating_sub(cpu_start);
    let process_cpu_time_s = process_cpu_ticks as f64 / clock_ticks_per_second();
    let process_cpu_cores = process_cpu_time_s / wall_time_s.max(1e-9);
    let node_reports = clusters
        .iter()
        .enumerate()
        .map(|(index, cluster)| NodeReport {
            node_index: index,
            numa_node: cluster.numa_node,
            socket_id: cluster.socket_id,
            cpus: worker_counts[index],
            slots: slot_counts[index],
            shards: shard_counts[index],
            completed: counters[index].completed.load(Ordering::Relaxed),
            valid: counters[index].valid.load(Ordering::Relaxed),
            failed: counters[index].failed.load(Ordering::Relaxed),
            bytes: counters[index].bytes.load(Ordering::Relaxed),
        })
        .collect();
    let report = FinalReport {
        schema: 1,
        executor: "numa_local_fixed_slots",
        total_tasks: opts.total_tasks,
        completed: final_snapshot.completed,
        valid: final_snapshot.valid,
        failed: final_snapshot.failed,
        bytes: final_snapshot.bytes,
        wall_time_s,
        sustained_qps: final_snapshot.valid as f64 / wall_time_s.max(1e-9),
        throughput_mb_s: final_snapshot.bytes as f64 / 1_048_576.0 / wall_time_s.max(1e-9),
        process_cpu_time_s,
        process_cpu_cores,
        process_cpu_pct_of_assigned: process_cpu_cores / topology.logical_cores as f64 * 100.0,
        assigned_cpus: topology.logical_cores,
        runtime_workers,
        use_smt: opts.use_smt,
        exact_pin: opts.exact_pin,
        physical_cores: topology.physical_cores,
        sockets: topology.sockets,
        numa_nodes: topology.numa_nodes,
        concurrency,
        shards,
        fixture_pages: opts.fixture_pages,
        node_reports,
    };
    fs::write(&opts.output, serde_json::to_vec_pretty(&report)?)?;
    println!("============================================================");
    println!("  Sustained QPS: {:.2}", report.sustained_qps);
    println!("  Completed: {} / {}", report.completed, report.total_tasks);
    println!("  Failed: {}", report.failed);
    println!(
        "  Runner CPU: {:.2} core-s/s ({:.2}% of {})",
        report.process_cpu_cores, report.process_cpu_pct_of_assigned, report.assigned_cpus
    );
    println!("  Report: {}", opts.output.display());
    println!("============================================================");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_node(
    node_index: usize,
    cluster: LocalityCluster,
    origin: String,
    total_tasks: u64,
    total_slots: usize,
    slot_offset: usize,
    slots: usize,
    shards: usize,
    fixture_pages: u64,
    use_smt: bool,
    exact_pin: bool,
    counters: Arc<Vec<NodeCounters>>,
) -> Result<(), String> {
    let cpus = if use_smt {
        cluster.logical_cpus().collect::<Vec<_>>()
    } else {
        cluster.physical_cpus.clone()
    };
    if cpus.is_empty() {
        return Err(format!("NUMA node {node_index} has no CPUs"));
    }
    // The coordinator constructs the runtime, connection pools, and all slot
    // futures. Pin it before those allocations so first-touch placement stays
    // with the NUMA domain that owns the workers.
    if !pin_current_thread(cpus[0]) {
        return Err(format!("failed to pin NUMA {node_index} coordinator"));
    }
    let next_cpu = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hook_cpus = cpus.clone();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(cpus.len())
        .thread_name_fn(move || format!("p2-n{node_index}-worker"))
        .on_thread_start(move || {
            if exact_pin {
                let rank = next_cpu.fetch_add(1, Ordering::Relaxed);
                let _ = pin_current_thread(hook_cpus[rank % hook_cpus.len()]);
            } else {
                let _ = pin_current_thread_to_set(&hook_cpus);
            }
        })
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async move {
        let clients = (0..shards)
            .map(|_| {
                reqwest::Client::builder()
                    .pool_max_idle_per_host(slots.div_ceil(shards).max(1) * 2)
                    .pool_idle_timeout(Duration::from_secs(90))
                    .tcp_nodelay(true)
                    .build()
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let clients = Arc::new(clients);
        let mut handles = Vec::with_capacity(slots);
        for local_rank in 0..slots {
            let client = clients[local_rank % shards].clone();
            let origin = origin.clone();
            let counters = counters.clone();
            let global_rank = slot_offset + local_rank;
            handles.push(tokio::spawn(async move {
                let mut task_id = global_rank as u64;
                while task_id < total_tasks {
                    let page_id = if fixture_pages == 0 {
                        task_id
                    } else {
                        task_id % fixture_pages
                    };
                    let url = format!("{origin}/p/{page_id}");
                    let success = match client.get(&url).send().await {
                        Ok(response) => match response.bytes().await {
                            Ok(body) => {
                                counters[node_index]
                                    .bytes
                                    .fetch_add(body.len() as u64, Ordering::Relaxed);
                                match std::str::from_utf8(&body) {
                                    Ok(html) => {
                                        let options = pagesift_content::Options {
                                            base_url: Url::parse(&url).ok(),
                                            ..Default::default()
                                        };
                                        !pagesift_content::process(html, &options)
                                            .markdown
                                            .is_empty()
                                    }
                                    Err(_) => false,
                                }
                            }
                            Err(_) => false,
                        },
                        Err(_) => false,
                    };
                    if success {
                        counters[node_index].valid.fetch_add(1, Ordering::Relaxed);
                    } else {
                        counters[node_index].failed.fetch_add(1, Ordering::Relaxed);
                    }
                    counters[node_index]
                        .completed
                        .fetch_add(1, Ordering::Relaxed);
                    task_id = task_id.saturating_add(total_slots as u64);
                }
            }));
        }
        for handle in handles {
            handle.await.map_err(|error| error.to_string())?;
        }
        Ok::<(), String>(())
    })
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
        let len = result.len();
        let index = cursor % len;
        result[index] += 1;
        cursor += 1;
    }
    result
}

fn prefix_offsets(counts: &[usize]) -> Vec<usize> {
    let mut total = 0usize;
    counts
        .iter()
        .map(|count| {
            let offset = total;
            total += *count;
            offset
        })
        .collect()
}

fn snapshot(counters: &[NodeCounters]) -> Snapshot {
    counters
        .iter()
        .fold(Snapshot::default(), |mut total, node| {
            total.completed += node.completed.load(Ordering::Relaxed);
            total.valid += node.valid.load(Ordering::Relaxed);
            total.failed += node.failed.load(Ordering::Relaxed);
            total.bytes += node.bytes.load(Ordering::Relaxed);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_distribution_is_exact_and_weighted() {
        let result = distribute_by_weights(16, &[2, 1, 1]);
        assert_eq!(result, vec![8, 4, 4]);
        assert_eq!(result.iter().sum::<usize>(), 16);
    }

    #[test]
    fn slot_offsets_cover_one_contiguous_rank_space() {
        assert_eq!(prefix_offsets(&[4, 3, 5]), vec![0, 4, 7]);
    }

    #[test]
    fn strided_slots_cover_every_task_once() {
        let total_tasks = 101u64;
        let total_slots = 16usize;
        let mut seen = vec![0u8; total_tasks as usize];
        for rank in 0..total_slots {
            let mut id = rank as u64;
            while id < total_tasks {
                seen[id as usize] += 1;
                id += total_slots as u64;
            }
        }
        assert!(seen.into_iter().all(|count| count == 1));
    }
}
