//! P2 component-attribution probe.
//!
//! Every mode consumes the same five synthetic pages. CPU-only modes preload
//! them before the measurement window; network modes use the same sharded
//! reqwest clients and fixed long-lived slots so transport and end-to-end
//! results differ only by PageSift processing.

use clap::{Parser, ValueEnum};
use pagesift_policy::adaptive::HardwareTopology;
use serde::Serialize;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ProbeMode {
    All,
    Url,
    Transport,
    Dom,
    Content,
    ContentWorkspace,
    Full,
}

#[derive(Debug, Parser)]
#[command(about = "PageSift P2 fixed-workload component attribution probe")]
struct Opts {
    #[arg(long, default_value = "http://127.0.0.1:19800")]
    origin_url: String,
    #[arg(long, value_enum, default_value_t = ProbeMode::All)]
    mode: ProbeMode,
    #[arg(long, default_value_t = 10_000_000)]
    iterations: u64,
    /// Long-lived network slots. Zero uses the detected initial concurrency.
    #[arg(long, default_value_t = 0)]
    concurrency: usize,
    /// Number of independent clients/connection pools. Zero uses physical cores.
    #[arg(long, default_value_t = 0)]
    shards: usize,
    /// CPU workers for URL/DOM/content probes. Zero uses all visible logical CPUs.
    #[arg(long, default_value_t = 0)]
    cpu_workers: usize,
    #[arg(long, default_value = "p2_component_probe.json")]
    output: PathBuf,
    /// Profile one network operation out of every N. Zero disables stage samples.
    #[arg(long, default_value_t = 4096)]
    stage_sample_every: u64,
}

#[derive(Debug, Serialize)]
struct ProbeResult {
    mode: &'static str,
    operations: u64,
    valid: u64,
    failed: u64,
    bytes: u64,
    wall_time_s: f64,
    operations_per_s: f64,
    throughput_mb_s: f64,
    process_cpu_time_s: f64,
    process_cpu_cores: f64,
    process_cpu_pct_of_assigned: f64,
    assigned_cpus: usize,
    workers_or_slots: usize,
    shards: usize,
    stage_profile: Option<StageProfile>,
    peak_rss_bytes: u64,
}

#[derive(Debug, Serialize)]
struct StageProfile {
    samples: usize,
    url_build_us: DurationProfile,
    headers_wait_us: DurationProfile,
    body_read_us: DurationProfile,
    content_us: DurationProfile,
    recycle_us: DurationProfile,
}

#[derive(Debug, Serialize)]
struct DurationProfile {
    mean: f64,
    p50: f64,
    p95: f64,
    p99: f64,
}

#[derive(Debug, Default)]
struct StageSample {
    url_build_ns: u64,
    headers_wait_ns: u64,
    body_read_ns: u64,
    content_ns: u64,
    recycle_ns: u64,
}

#[derive(Default)]
struct NetworkCounters {
    valid: AtomicU64,
    failed: AtomicU64,
    bytes: AtomicU64,
    stage_samples: Mutex<Vec<StageSample>>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = Opts::parse();
    let topology = HardwareTopology::detect();
    let assigned_cpus = topology.logical_cores.max(1);
    let concurrency = if opts.concurrency == 0 {
        topology.base_concurrency
    } else {
        opts.concurrency
    }
    .max(1);
    let shards = if opts.shards == 0 {
        topology.physical_cores
    } else {
        opts.shards
    }
    .clamp(1, concurrency);
    let cpu_workers = if opts.cpu_workers == 0 {
        assigned_cpus
    } else {
        opts.cpu_workers
    }
    .max(1);

    let fixtures = Arc::new(load_fixtures(&opts.origin_url).await?);
    let mut results = Vec::new();
    for mode in selected_modes(opts.mode) {
        let result = match mode {
            ProbeMode::Url => run_cpu_probe(
                "url_dispatch",
                opts.iterations,
                cpu_workers,
                assigned_cpus,
                fixtures.clone(),
                CpuWork::Url,
            )?,
            ProbeMode::Dom => run_cpu_probe(
                "html_dom",
                opts.iterations,
                cpu_workers,
                assigned_cpus,
                fixtures.clone(),
                CpuWork::Dom,
            )?,
            ProbeMode::Content => run_cpu_probe(
                "content_total",
                opts.iterations,
                cpu_workers,
                assigned_cpus,
                fixtures.clone(),
                CpuWork::Content,
            )?,
            ProbeMode::ContentWorkspace => run_cpu_probe(
                "content_workspace",
                opts.iterations,
                cpu_workers,
                assigned_cpus,
                fixtures.clone(),
                CpuWork::ContentWorkspace,
            )?,
            ProbeMode::Transport => {
                run_network_probe(
                    "transport_only",
                    &opts.origin_url,
                    opts.iterations,
                    concurrency,
                    shards,
                    assigned_cpus,
                    false,
                    opts.stage_sample_every,
                )
                .await?
            }
            ProbeMode::Full => {
                run_network_probe(
                    "end_to_end",
                    &opts.origin_url,
                    opts.iterations,
                    concurrency,
                    shards,
                    assigned_cpus,
                    true,
                    opts.stage_sample_every,
                )
                .await?
            }
            ProbeMode::All => unreachable!(),
        };
        print_result(&result);
        results.push(result);
    }

    let report = serde_json::json!({
        "schema": 1,
        "origin_url": opts.origin_url,
        "fixture_modalities": fixtures.len(),
        "logical_cpus": topology.logical_cores,
        "physical_cores": topology.physical_cores,
        "sockets": topology.sockets,
        "numa_nodes": topology.numa_nodes,
        "iterations_per_mode": opts.iterations,
        "results": results,
    });
    fs::write(&opts.output, serde_json::to_vec_pretty(&report)?)?;
    println!("report={}", opts.output.display());
    Ok(())
}

fn selected_modes(mode: ProbeMode) -> Vec<ProbeMode> {
    match mode {
        ProbeMode::All => vec![
            ProbeMode::Url,
            ProbeMode::Transport,
            ProbeMode::Dom,
            ProbeMode::Content,
            ProbeMode::ContentWorkspace,
            ProbeMode::Full,
        ],
        one => vec![one],
    }
}

async fn load_fixtures(origin: &str) -> Result<Vec<Arc<str>>, Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder().tcp_nodelay(true).build()?;
    let mut pages = Vec::with_capacity(5);
    for id in 0..5 {
        let url = format!("{origin}/p/{id}");
        let response = client.get(url).send().await?.error_for_status()?;
        pages.push(Arc::<str>::from(response.text().await?));
    }
    Ok(pages)
}

#[derive(Clone, Copy)]
enum CpuWork {
    Url,
    Dom,
    Content,
    ContentWorkspace,
}

fn run_cpu_probe(
    name: &'static str,
    operations: u64,
    workers: usize,
    assigned_cpus: usize,
    fixtures: Arc<Vec<Arc<str>>>,
    work: CpuWork,
) -> Result<ProbeResult, Box<dyn std::error::Error>> {
    let cpu_start = read_process_cpu_ticks().unwrap_or(0);
    let started = Instant::now();
    let mut handles = Vec::with_capacity(workers);
    for rank in 0..workers {
        let fixtures = fixtures.clone();
        let begin = operations * rank as u64 / workers as u64;
        let end = operations * (rank + 1) as u64 / workers as u64;
        handles.push(std::thread::spawn(move || -> (u64, u64) {
            let mut valid = 0u64;
            let mut bytes = 0u64;
            let mut workspace = pagesift_content::ProcessWorkspace::default();
            let base = Url::parse("http://127.0.0.1:19800/").expect("static base URL");
            for id in begin..end {
                let fixture = &fixtures[id as usize % fixtures.len()];
                match work {
                    CpuWork::Url => {
                        let value = format!("http://127.0.0.1:19800/p/{id}");
                        if Url::parse(&value).is_ok() {
                            valid += 1;
                            bytes += value.len() as u64;
                        }
                    }
                    CpuWork::Dom => {
                        let dom = pagesift_html::Html::parse(fixture);
                        valid += u64::from(dom.root().descendants().next().is_some());
                        bytes += fixture.len() as u64;
                    }
                    CpuWork::Content => {
                        let options = pagesift_content::Options {
                            base_url: Some(base.clone()),
                            ..Default::default()
                        };
                        let document = pagesift_content::process(fixture, &options);
                        valid += u64::from(!document.markdown.is_empty());
                        bytes += fixture.len() as u64;
                    }
                    CpuWork::ContentWorkspace => {
                        let options = pagesift_content::Options {
                            base_url: Some(base.clone()),
                            ..Default::default()
                        };
                        let document = pagesift_content::process_with_workspace(
                            fixture,
                            &options,
                            &mut workspace,
                        );
                        valid += u64::from(!document.markdown.is_empty());
                        bytes += fixture.len() as u64;
                    }
                }
            }
            (valid, bytes)
        }));
    }
    let mut valid = 0u64;
    let mut bytes = 0u64;
    for handle in handles {
        let (worker_valid, worker_bytes) = handle.join().map_err(|_| "CPU probe panicked")?;
        valid += worker_valid;
        bytes += worker_bytes;
    }
    Ok(finish_result(
        name,
        operations,
        valid,
        operations.saturating_sub(valid),
        bytes,
        started.elapsed().as_secs_f64(),
        cpu_start,
        assigned_cpus,
        workers,
        0,
        None,
    ))
}

async fn run_network_probe(
    name: &'static str,
    origin: &str,
    operations: u64,
    concurrency: usize,
    shards: usize,
    assigned_cpus: usize,
    process_content: bool,
    stage_sample_every: u64,
) -> Result<ProbeResult, Box<dyn std::error::Error>> {
    let clients = (0..shards)
        .map(|_| {
            reqwest::Client::builder()
                .pool_max_idle_per_host(concurrency.div_ceil(shards).max(1) * 2)
                .pool_idle_timeout(std::time::Duration::from_secs(90))
                .tcp_nodelay(true)
                .build()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let clients = Arc::new(clients);
    let next = Arc::new(AtomicU64::new(0));
    let counters = Arc::new(NetworkCounters::default());
    let cpu_start = read_process_cpu_ticks().unwrap_or(0);
    let started = Instant::now();
    let mut slots = Vec::with_capacity(concurrency);
    for rank in 0..concurrency {
        let client = clients[rank % shards].clone();
        let next = next.clone();
        let counters = counters.clone();
        let origin = origin.to_string();
        slots.push(tokio::spawn(async move {
            loop {
                let id = next.fetch_add(1, Ordering::Relaxed);
                if id >= operations {
                    break;
                }
                let profile = stage_sample_every > 0 && id % stage_sample_every == 0;
                let stage_started = profile.then(Instant::now);
                let url = format!("{origin}/p/{}", id % 5);
                let url_built = profile.then(Instant::now);
                let response = match client.get(&url).send().await {
                    Ok(response) => response,
                    Err(_) => {
                        counters.failed.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                };
                let headers_received = profile.then(Instant::now);
                let body = match response.bytes().await {
                    Ok(body) => body,
                    Err(_) => {
                        counters.failed.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                };
                let body_received = profile.then(Instant::now);
                counters
                    .bytes
                    .fetch_add(body.len() as u64, Ordering::Relaxed);
                let mut document = None;
                let valid = if process_content {
                    match std::str::from_utf8(&body) {
                        Ok(html) => {
                            let options = pagesift_content::Options {
                                base_url: Url::parse(&url).ok(),
                                ..Default::default()
                            };
                            document = Some(pagesift_content::process(html, &options));
                            !document.as_ref().unwrap().markdown.is_empty()
                        }
                        Err(_) => false,
                    }
                } else {
                    !body.is_empty()
                };
                let content_finished = profile.then(Instant::now);
                if valid {
                    counters.valid.fetch_add(1, Ordering::Relaxed);
                } else {
                    counters.failed.fetch_add(1, Ordering::Relaxed);
                }
                drop(document);
                drop(body);
                if let (
                    Some(stage_started),
                    Some(url_built),
                    Some(headers_received),
                    Some(body_received),
                    Some(content_finished),
                ) = (
                    stage_started,
                    url_built,
                    headers_received,
                    body_received,
                    content_finished,
                ) {
                    let recycled = Instant::now();
                    let sample = StageSample {
                        url_build_ns: nanos(url_built.duration_since(stage_started)),
                        headers_wait_ns: nanos(headers_received.duration_since(url_built)),
                        body_read_ns: nanos(body_received.duration_since(headers_received)),
                        content_ns: nanos(content_finished.duration_since(body_received)),
                        recycle_ns: nanos(recycled.duration_since(content_finished)),
                    };
                    counters.stage_samples.lock().unwrap().push(sample);
                }
            }
        }));
    }
    for slot in slots {
        slot.await?;
    }
    let valid = counters.valid.load(Ordering::Relaxed);
    let failed = counters.failed.load(Ordering::Relaxed);
    let bytes = counters.bytes.load(Ordering::Relaxed);
    let stage_profile = summarize_stages(&counters.stage_samples.lock().unwrap());
    Ok(finish_result(
        name,
        operations,
        valid,
        failed,
        bytes,
        started.elapsed().as_secs_f64(),
        cpu_start,
        assigned_cpus,
        concurrency,
        shards,
        stage_profile,
    ))
}

#[allow(clippy::too_many_arguments)]
fn finish_result(
    mode: &'static str,
    operations: u64,
    valid: u64,
    failed: u64,
    bytes: u64,
    wall_time_s: f64,
    cpu_start_ticks: u64,
    assigned_cpus: usize,
    workers_or_slots: usize,
    shards: usize,
    stage_profile: Option<StageProfile>,
) -> ProbeResult {
    let cpu_ticks = read_process_cpu_ticks()
        .unwrap_or(cpu_start_ticks)
        .saturating_sub(cpu_start_ticks);
    let process_cpu_time_s = cpu_ticks as f64 / clock_ticks_per_second();
    let process_cpu_cores = process_cpu_time_s / wall_time_s.max(1e-9);
    ProbeResult {
        mode,
        operations,
        valid,
        failed,
        bytes,
        wall_time_s,
        operations_per_s: operations as f64 / wall_time_s.max(1e-9),
        throughput_mb_s: bytes as f64 / 1_048_576.0 / wall_time_s.max(1e-9),
        process_cpu_time_s,
        process_cpu_cores,
        process_cpu_pct_of_assigned: process_cpu_cores / assigned_cpus as f64 * 100.0,
        assigned_cpus,
        workers_or_slots,
        shards,
        stage_profile,
        peak_rss_bytes: read_peak_rss_bytes().unwrap_or(0),
    }
}

fn print_result(result: &ProbeResult) {
    println!(
        "{:<16} {:>12.1} ops/s | {:>7.2}s | CPU {:>7.2} core-s/s ({:>5.1}% of {}) | failed {} | {:>8.1} MiB/s",
        result.mode,
        result.operations_per_s,
        result.wall_time_s,
        result.process_cpu_cores,
        result.process_cpu_pct_of_assigned,
        result.assigned_cpus,
        result.failed,
        result.throughput_mb_s,
    );
    if let Some(stages) = &result.stage_profile {
        println!(
            "  sampled={} | url mean/p95 {:.2}/{:.2} us | headers {:.2}/{:.2} us | body {:.2}/{:.2} us | content {:.2}/{:.2} us | recycle {:.2}/{:.2} us",
            stages.samples,
            stages.url_build_us.mean,
            stages.url_build_us.p95,
            stages.headers_wait_us.mean,
            stages.headers_wait_us.p95,
            stages.body_read_us.mean,
            stages.body_read_us.p95,
            stages.content_us.mean,
            stages.content_us.p95,
            stages.recycle_us.mean,
            stages.recycle_us.p95,
        );
    }
}

fn nanos(duration: std::time::Duration) -> u64 {
    duration.as_nanos().min(u64::MAX as u128) as u64
}

fn summarize_stages(samples: &[StageSample]) -> Option<StageProfile> {
    if samples.is_empty() {
        return None;
    }
    Some(StageProfile {
        samples: samples.len(),
        url_build_us: duration_profile(samples.iter().map(|s| s.url_build_ns)),
        headers_wait_us: duration_profile(samples.iter().map(|s| s.headers_wait_ns)),
        body_read_us: duration_profile(samples.iter().map(|s| s.body_read_ns)),
        content_us: duration_profile(samples.iter().map(|s| s.content_ns)),
        recycle_us: duration_profile(samples.iter().map(|s| s.recycle_ns)),
    })
}

fn duration_profile(values: impl Iterator<Item = u64>) -> DurationProfile {
    let mut values = values.collect::<Vec<_>>();
    values.sort_unstable();
    let mean = values.iter().map(|v| *v as f64).sum::<f64>() / values.len() as f64 / 1000.0;
    let percentile = |p: usize| {
        let index = ((values.len() - 1) * p).div_ceil(100);
        values[index] as f64 / 1000.0
    };
    DurationProfile {
        mean,
        p50: percentile(50),
        p95: percentile(95),
        p99: percentile(99),
    }
}

fn read_process_cpu_ticks() -> Option<u64> {
    parse_process_cpu_ticks(&fs::read_to_string("/proc/self/stat").ok()?)
}

fn parse_process_cpu_ticks(stat: &str) -> Option<u64> {
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

    #[test]
    fn parses_ticks_after_parenthesized_command() {
        let stat = "1 (probe worker (x)) R 0 0 0 0 0 0 0 0 0 0 7 11 0 0";
        assert_eq!(parse_process_cpu_ticks(stat), Some(18));
    }

    #[test]
    fn all_mode_has_six_independent_probes() {
        let modes = selected_modes(ProbeMode::All);
        assert_eq!(modes.len(), 6);
        assert!(!modes.contains(&ProbeMode::All));
    }
}
