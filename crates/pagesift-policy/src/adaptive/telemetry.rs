//! Hardware topology, system memory budget, CPU saturation, and rolling latency telemetry.

use std::collections::VecDeque;
use std::fs;
use std::sync::Mutex;
use std::time::Instant;

use super::topology::CpuTopology;

/// Classified hardware topology profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopologyCategory {
    /// High cores (>=16) and high RAM (>=1 GB/core). E.g. 32C/64G, 80C/160G.
    HighRamHighCores,
    /// Low/Medium cores (<16) and high RAM (>=2 GB/core). E.g. 4C/32G, 8C/64G.
    HighRamLowCores,
    /// High cores (>=8) but tight RAM (<1 GB/core). E.g. 16T/16GB developer machine.
    LowRamHighCores,
    /// Constrained environment (<=4 cores, <=4 GB RAM, or container limit).
    Constrained,
}

/// Dynamic snapshot of process and system memory in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemorySnapshot {
    /// Total system or container memory limit in bytes.
    pub total_bytes: u64,
    /// Available memory remaining in bytes.
    pub available_bytes: u64,
    /// Current resident set size (RSS) of this process in bytes.
    pub process_rss_bytes: u64,
}

/// Dynamic rolling quantile tracker for dispatch latency and RTT.
#[derive(Debug)]
pub struct QuantileLatencyTracker {
    samples: Mutex<VecDeque<f64>>,
    max_samples: usize,
}

impl QuantileLatencyTracker {
    pub fn new(max_samples: usize) -> Self {
        Self {
            samples: Mutex::new(VecDeque::with_capacity(max_samples)),
            max_samples: max_samples.max(64),
        }
    }

    pub fn record(&self, latency_ms: f64) {
        if latency_ms.is_finite() && latency_ms >= 0.0 {
            let mut s = self.samples.lock().unwrap();
            if s.len() >= self.max_samples {
                s.pop_front();
            }
            s.push_back(latency_ms);
        }
    }

    /// Computes (p50, p95, p99) latencies in milliseconds.
    pub fn quantiles(&self) -> (f64, f64, f64) {
        let s = self.samples.lock().unwrap();
        if s.is_empty() {
            return (1.0, 2.0, 5.0);
        }
        let mut sorted: Vec<f64> = s.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let len = sorted.len();
        let p50 = sorted[(len * 50) / 100];
        let p95 = sorted[((len * 95) / 100).min(len - 1)];
        let p99 = sorted[((len * 99) / 100).min(len - 1)];
        (p50, p95, p99)
    }
}

/// Process-only CPU utilization sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessCpuSample {
    /// CPU utilization normalized by the process' effective worker capacity.
    pub utilization_pct: f64,
    /// Cumulative user + system CPU time consumed by this process.
    pub process_cpu_time_s: f64,
    /// False until two process tick samples have enough wall time between them.
    pub valid: bool,
}

#[derive(Debug)]
struct ProcessCpuState {
    previous_ticks: Option<u64>,
    previous_at: Instant,
    last_utilization_pct: f64,
}

/// Dynamic CPU tracker using only `/proc/self/stat` utime + stime.
///
/// System-wide `/proc/stat` is intentionally not used: unrelated processes and
/// a co-located benchmark origin must not influence PageSift's control loop.
#[derive(Debug)]
pub struct CpuSaturationTracker {
    state: Mutex<ProcessCpuState>,
    clock_ticks_per_second: f64,
    assigned_cores: usize,
}

impl Default for CpuSaturationTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuSaturationTracker {
    pub fn new() -> Self {
        Self::with_assigned_cores(CpuTopology::detect().worker_cores)
    }

    pub fn with_assigned_cores(assigned_cores: usize) -> Self {
        Self {
            state: Mutex::new(ProcessCpuState {
                previous_ticks: None,
                previous_at: Instant::now(),
                last_utilization_pct: 0.0,
            }),
            clock_ticks_per_second: clock_ticks_per_second(),
            assigned_cores: assigned_cores.max(1),
        }
    }

    /// Returns current process CPU utilization in percent of assigned worker capacity.
    pub fn sample_utilization(&self) -> f64 {
        self.sample().utilization_pct
    }

    pub fn sample(&self) -> ProcessCpuSample {
        let now = Instant::now();
        let ticks = read_process_cpu_ticks();
        let mut state = self.state.lock().unwrap();
        let total_seconds = ticks.unwrap_or(0) as f64 / self.clock_ticks_per_second;
        let mut valid = false;

        if let (Some(previous), Some(current)) = (state.previous_ticks, ticks) {
            let elapsed = now.duration_since(state.previous_at).as_secs_f64();
            if elapsed >= 0.001 {
                state.last_utilization_pct = process_utilization_pct(
                    current.saturating_sub(previous),
                    elapsed,
                    self.clock_ticks_per_second,
                    self.assigned_cores,
                );
                valid = true;
            }
        }
        if ticks.is_some() {
            state.previous_ticks = ticks;
            state.previous_at = now;
        }

        ProcessCpuSample {
            utilization_pct: state.last_utilization_pct,
            process_cpu_time_s: total_seconds,
            valid,
        }
    }
}

fn read_process_cpu_ticks() -> Option<u64> {
    parse_process_cpu_ticks(&fs::read_to_string("/proc/self/stat").ok()?)
}

fn parse_process_cpu_ticks(stat: &str) -> Option<u64> {
    let command_end = stat.rfind(')')?;
    let fields = stat
        .get(command_end + 1..)?
        .split_whitespace()
        .collect::<Vec<_>>();
    // Fields after comm start at proc field 3 (state); utime/stime are fields 14/15.
    let utime = fields.get(11)?.parse::<u64>().ok()?;
    let stime = fields.get(12)?.parse::<u64>().ok()?;
    Some(utime.saturating_add(stime))
}

fn process_utilization_pct(
    delta_ticks: u64,
    elapsed_s: f64,
    ticks_per_second: f64,
    assigned_cores: usize,
) -> f64 {
    let capacity = elapsed_s.max(1e-6) * ticks_per_second.max(1.0) * assigned_cores.max(1) as f64;
    (delta_ticks as f64 / capacity * 100.0).clamp(0.0, 100.0)
}

#[cfg(target_os = "linux")]
fn clock_ticks_per_second() -> f64 {
    rustix::param::clock_ticks_per_second().max(1) as f64
}

#[cfg(not(target_os = "linux"))]
fn clock_ticks_per_second() -> f64 {
    100.0
}

/// Real-time memory consumption rate of change (dRSS/dt) tracker.
#[derive(Debug)]
pub struct RssDerivativeTracker {
    last_rss_bytes: Mutex<u64>,
    last_time: Mutex<Instant>,
}

impl Default for RssDerivativeTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl RssDerivativeTracker {
    pub fn new() -> Self {
        Self {
            last_rss_bytes: Mutex::new(0),
            last_time: Mutex::new(Instant::now()),
        }
    }

    /// Samples current RSS and returns (rss_mb, d_rss_mb_per_sec).
    pub fn sample(&self) -> (f64, f64) {
        let current_bytes = HardwareTopology::read_process_rss();
        let now = Instant::now();

        let mut l_bytes = self.last_rss_bytes.lock().unwrap();
        let mut l_time = self.last_time.lock().unwrap();

        let dt = now.duration_since(*l_time).as_secs_f64().max(0.001);
        let diff = (current_bytes as f64 - *l_bytes as f64) / (1024.0 * 1024.0);
        let rate_mb_per_sec = diff / dt;

        *l_bytes = current_bytes;
        *l_time = now;

        (current_bytes as f64 / (1024.0 * 1024.0), rate_mb_per_sec)
    }
}

/// Hardware topology and continuous auto-tuning parameters.
#[derive(Debug, Clone)]
pub struct HardwareTopology {
    /// Number of logical CPU threads available.
    pub logical_cores: usize,
    /// Number of distinct physical cores visible to the process.
    pub physical_cores: usize,
    /// Number of CPU sockets visible to the process.
    pub sockets: usize,
    /// Number of NUMA locality domains visible to the process.
    pub numa_nodes: usize,
    /// Hardware threads per physical core.
    pub threads_per_core: usize,
    /// Physical-core-first worker count after cgroup quota constraints.
    pub worker_cores: usize,
    /// Tokio workers including the bounded, NUMA-balanced SMT reserve.
    pub runtime_workers: usize,
    /// Full topology used for NUMA-local executor construction.
    pub cpu_topology: CpuTopology,
    /// System file descriptor limit.
    pub fd_limit: usize,
    /// Topology category classification.
    pub category: TopologyCategory,
    /// RAM-to-core ratio in megabytes per core.
    pub ram_per_core_mb: usize,
    /// Initial baseline concurrency.
    pub base_concurrency: usize,
    /// Maximum safe concurrency ceiling.
    pub max_concurrency: usize,
    /// Minimum concurrency floor.
    pub min_concurrency: usize,
    /// Recommended number of concurrently active sites.
    pub active_sites_cap: usize,
    /// Maximum in-flight unparsed payload bytes.
    pub inflight_payload_budget_bytes: usize,
    /// Recommended sequential I/O write batch buffer size.
    pub batch_io_bytes: usize,
}

/// Autonomous scheduling parameters determined dynamically by the engine.
#[derive(Debug, Clone)]
pub struct AdaptiveSchedule {
    pub global_concurrency: usize,
    pub per_host_concurrency: usize,
    pub active_workers: usize,
    pub inflight_budget_bytes: usize,
    pub batch_io_bytes: usize,
}

impl HardwareTopology {
    /// Auto-tunes scheduler parameters given graph topology (hosts, per_host).
    pub fn tune_for_graph(&self, hosts: usize, per_host: usize) -> AdaptiveSchedule {
        let per_host_conc = if per_host >= 5000 {
            self.worker_cores.clamp(8, 32)
        } else {
            (self.worker_cores / 2).clamp(2, 16)
        };

        let saturation_workers =
            (self.max_concurrency.div_ceil(per_host_conc) * 4).clamp(16, self.active_sites_cap);
        let active_workers = saturation_workers.min(hosts.max(1));

        AdaptiveSchedule {
            global_concurrency: self.base_concurrency,
            per_host_concurrency: per_host_conc,
            active_workers,
            inflight_budget_bytes: self.inflight_payload_budget_bytes,
            batch_io_bytes: self.batch_io_bytes,
        }
    }

    /// Detects current hardware and memory environment using the continuous C* model:
    /// C* = f(CPU, RAM, FD, Workload) without artificial step capping.
    pub fn detect() -> Self {
        let cpu_topology = CpuTopology::detect();
        let cores = cpu_topology.worker_cores;
        let runtime_workers = cpu_topology.runtime_worker_cpus().len();
        let mem = Self::read_memory();
        let fd_limit = detect_fd_limit();

        let total_mb = (mem.total_bytes / (1024 * 1024)) as usize;
        let ram_per_core_mb = total_mb.checked_div(cores).unwrap_or(512);

        let category = if cores >= 16 && ram_per_core_mb >= 1024 {
            TopologyCategory::HighRamHighCores
        } else if cores < 16 && ram_per_core_mb >= 2048 {
            TopologyCategory::HighRamLowCores
        } else if cores >= 8 && ram_per_core_mb < 1024 {
            TopologyCategory::LowRamHighCores
        } else {
            TopologyCategory::Constrained
        };

        // Continuous C* model:
        // Memory cap: 2MB per in-flight connection lifecycle budget
        let mem_cap = (mem.total_bytes / (2 * 1024 * 1024)) as usize;
        // FD cap: 70% of system file descriptors reserved for crawler sockets
        let fd_cap = (fd_limit * 7) / 10;
        // Start from physical-core capacity. One local NUMA domain can sustain deeper
        // in-flight work, while additional domains expand workers rather than blindly
        // multiplying every logical SMT thread.
        let local_physical_cores = cpu_topology.physical_cores_per_numa();
        // Begin large hosts with enough I/O depth to keep every physical core
        // fed. This is an initial condition, not a ceiling; the online
        // controller immediately challenges it in both directions.
        let cpu_base = (cores * 16).max(local_physical_cores * 24);
        // This is a resource safety boundary for adaptive exploration, not an
        // assumed optimum. Slow or highly multiplexed I/O can need far more
        // in-flight work than CPU-bound local parsing. The controller measures
        // the response surface and may settle anywhere below this ceiling.
        let cpu_max = (cores * 128).max(cpu_base * 2);

        let base_concurrency = cpu_base.min(mem_cap).min(fd_cap).max(4);
        let max_concurrency = cpu_max.min(mem_cap).min(fd_cap).max(base_concurrency);
        let min_concurrency = (cores * 2).clamp(2, 32);

        // Active sites scaling: proportional to concurrency
        let active_sites = (cores * 16).min(max_concurrency * 2).clamp(16, 2048);

        // Inflight buffer budget (proportional to total memory, clamped between 32MB and 2GB)
        let inflight_budget =
            ((mem.total_bytes / 32) as usize).clamp(32 * 1024 * 1024, 2 * 1024 * 1024 * 1024);
        let batch_io = (cores * 512 * 1024).clamp(1024 * 1024, 64 * 1024 * 1024);

        Self {
            logical_cores: cpu_topology.logical_cpus,
            physical_cores: cpu_topology.physical_cores,
            sockets: cpu_topology.sockets,
            numa_nodes: cpu_topology.numa_nodes,
            threads_per_core: cpu_topology.threads_per_core,
            worker_cores: cpu_topology.worker_cores,
            runtime_workers,
            cpu_topology,
            fd_limit,
            category,
            ram_per_core_mb,
            base_concurrency,
            max_concurrency,
            min_concurrency,
            active_sites_cap: active_sites,
            inflight_payload_budget_bytes: inflight_budget,
            batch_io_bytes: batch_io,
        }
    }

    /// Reads memory metrics from Linux procfs/cgroups or defaults to standard assumptions.
    pub fn read_memory() -> MemorySnapshot {
        let mut total_bytes = 8 * 1024 * 1024 * 1024u64; // Default 8 GiB
        let mut available_bytes = 4 * 1024 * 1024 * 1024u64;

        // Try reading /proc/meminfo
        if let Ok(meminfo) = fs::read_to_string("/proc/meminfo") {
            for line in meminfo.lines() {
                if let Some(val) = line.strip_prefix("MemTotal:") {
                    if let Some(kb) = parse_kb(val) {
                        total_bytes = kb * 1024;
                    }
                } else if let Some(val) = line.strip_prefix("MemAvailable:")
                    && let Some(kb) = parse_kb(val)
                {
                    available_bytes = kb * 1024;
                }
            }
        }

        // Resolve the memory boundary that actually applies to this process.
        // A systemd scope or container puts the real limit below the cgroup
        // root, so reading only `/sys/fs/cgroup/memory.max` silently ignores
        // it. Prefer the process's own cgroup, then fall back to the root
        // (container) view.
        let (cgroup_limit, cgroup_current) = read_cgroup_memory();
        let address_space_limit = fs::read_to_string("/proc/self/limits")
            .ok()
            .and_then(|limits| parse_address_space_limit(&limits));
        let effective_limit = match (cgroup_limit, address_space_limit) {
            (Some(cgroup), Some(address_space)) => Some(cgroup.min(address_space)),
            (Some(cgroup), None) => Some(cgroup),
            (None, Some(address_space)) => Some(address_space),
            (None, None) => None,
        };
        if let Some(limit) = effective_limit {
            total_bytes = total_bytes.min(limit);
            if let Some(current) = cgroup_current {
                available_bytes = available_bytes.min(limit.saturating_sub(current));
            } else {
                available_bytes = available_bytes.min(limit);
            }
        }

        let rss_bytes = Self::read_process_rss();

        MemorySnapshot {
            total_bytes,
            available_bytes,
            process_rss_bytes: rss_bytes,
        }
    }

    /// Reads process RSS in bytes via /proc/self/statm on Linux.
    pub fn read_process_rss() -> u64 {
        if let Ok(statm) = fs::read_to_string("/proc/self/statm") {
            let mut parts = statm.split_whitespace();
            let _total_pages = parts.next();
            if let Some(resident_pages_str) = parts.next()
                && let Ok(pages) = resident_pages_str.parse::<u64>()
            {
                return pages * 4096;
            }
        }
        0
    }
}

pub fn detect_fd_limit() -> usize {
    if let Ok(limits) = fs::read_to_string("/proc/self/limits") {
        for line in limits.lines() {
            if line.starts_with("Max open files") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 5
                    && let Ok(soft) = parts[3].parse::<usize>()
                {
                    return soft;
                }
            }
        }
    }
    4096
}

fn parse_kb(s: &str) -> Option<u64> {
    let mut parts = s.split_whitespace();
    let num_str = parts.next()?;
    num_str.parse::<u64>().ok()
}

fn parse_cgroup_limit(value: &str) -> Option<u64> {
    let value = value.trim();
    if value == "max" {
        None
    } else {
        value.parse::<u64>().ok().filter(|limit| *limit > 0)
    }
}

fn cgroup_v2_base(path: &str) -> String {
    let path = path.trim();
    if path == "/" {
        "/sys/fs/cgroup".to_string()
    } else {
        format!("/sys/fs/cgroup{path}")
    }
}

/// Candidate `(limit_path, current_path)` pairs for this process's memory
/// cgroup, ordered from the most specific view to the container/root view.
fn cgroup_memory_candidates() -> Vec<(String, String)> {
    let mut candidates = Vec::new();
    if let Ok(cgroup) = fs::read_to_string("/proc/self/cgroup") {
        for line in cgroup.lines() {
            let Some(path) = line.strip_prefix("0::") else {
                continue;
            };
            let base = cgroup_v2_base(path);
            candidates.push((
                format!("{base}/memory.max"),
                format!("{base}/memory.current"),
            ));
        }
        for line in cgroup.lines() {
            let mut parts = line.splitn(3, ':');
            let _id = parts.next();
            let controllers = parts.next().unwrap_or("");
            let path = parts.next().unwrap_or("").trim();
            if !controllers
                .split(',')
                .any(|controller| controller == "memory")
            {
                continue;
            }
            let base = format!("/sys/fs/cgroup/memory{path}");
            candidates.push((
                format!("{base}/memory.limit_in_bytes"),
                format!("{base}/memory.usage_in_bytes"),
            ));
        }
    }
    // Container and root fallbacks. In a container with a private cgroup
    // namespace these paths carry the container boundary.
    candidates.push((
        "/sys/fs/cgroup/memory.max".to_string(),
        "/sys/fs/cgroup/memory.current".to_string(),
    ));
    candidates.push((
        "/sys/fs/cgroup/memory/memory.limit_in_bytes".to_string(),
        "/sys/fs/cgroup/memory/memory.usage_in_bytes".to_string(),
    ));
    candidates
}

/// Returns the tightest finite memory limit and the matching current usage
/// found across the process's own cgroup and its root fallbacks.
fn read_cgroup_memory() -> (Option<u64>, Option<u64>) {
    let mut best: Option<(u64, Option<u64>)> = None;
    for (limit_path, current_path) in cgroup_memory_candidates() {
        let limit = fs::read_to_string(limit_path)
            .ok()
            .and_then(|value| parse_cgroup_limit(&value));
        let current = fs::read_to_string(current_path)
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok());
        match limit {
            Some(limit) => {
                if best
                    .as_ref()
                    .is_none_or(|(best_limit, _)| limit < *best_limit)
                {
                    best = Some((limit, current));
                }
            }
            None => {
                if best.is_none() && current.is_some() {
                    // Remember usage while still looking for an ancestor limit.
                    best = Some((u64::MAX, current));
                }
            }
        }
    }
    match best {
        Some((u64::MAX, current)) => (None, current),
        Some((limit, current)) => (Some(limit), current),
        None => (None, None),
    }
}

/// Parses the soft `RLIMIT_AS` value from `/proc/self/limits`. `unlimited`
/// yields `None`; a finite byte count is an additional upper bound.
fn parse_address_space_limit(limits: &str) -> Option<u64> {
    for line in limits.lines() {
        if !line.starts_with("Max address space") {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 5 {
            return parts[3].parse::<u64>().ok().filter(|limit| *limit > 0);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hardware_detect() {
        let topo = HardwareTopology::detect();
        assert!(topo.logical_cores >= 1);
        assert!(topo.base_concurrency >= 2);
        assert!(topo.max_concurrency >= topo.base_concurrency);
        assert!(topo.min_concurrency <= topo.base_concurrency);
        assert!(topo.inflight_payload_budget_bytes > 0);
    }

    #[test]
    fn test_latency_tracker() {
        let tracker = QuantileLatencyTracker::new(100);
        for i in 1..=100 {
            tracker.record(i as f64);
        }
        let (p50, p95, p99) = tracker.quantiles();
        assert!((p50 - 51.0).abs() <= 2.0);
        assert!((p95 - 96.0).abs() <= 2.0);
        assert!((p99 - 100.0).abs() <= 2.0);
    }

    #[test]
    fn parses_process_ticks_when_command_contains_spaces_and_parentheses() {
        let stat = "123 (page sift ) worker) R 1 2 3 4 5 6 7 8 9 10 120 30";
        assert_eq!(parse_process_cpu_ticks(stat), Some(150));
    }

    #[test]
    fn normalizes_process_cpu_by_assigned_worker_capacity() {
        assert!((process_utilization_pct(800, 1.0, 100.0, 8) - 100.0).abs() < 1e-9);
        assert!((process_utilization_pct(200, 1.0, 100.0, 8) - 25.0).abs() < 1e-9);
    }

    #[test]
    fn parses_cgroup_memory_limits() {
        assert_eq!(parse_cgroup_limit("1073741824\n"), Some(1_073_741_824));
        assert_eq!(parse_cgroup_limit("max\n"), None);
    }

    #[test]
    fn resolves_own_cgroup_v2_path() {
        assert_eq!(cgroup_v2_base("/"), "/sys/fs/cgroup");
        assert_eq!(
            cgroup_v2_base("/user.slice/user-1001.slice/s1.scope"),
            "/sys/fs/cgroup/user.slice/user-1001.slice/s1.scope"
        );
    }

    #[test]
    fn parses_address_space_limit_from_proc_limits() {
        let unlimited =
            "Max address space         unlimited            unlimited            bytes\n";
        assert_eq!(parse_address_space_limit(unlimited), None);

        let limited = "Max address space         536870912            536870912            bytes\n";
        assert_eq!(parse_address_space_limit(limited), Some(536_870_912));

        let mixed = "Max open files            1048576              1048576              files\n\
                     Max address space         1073741824           1073741824           bytes\n";
        assert_eq!(parse_address_space_limit(mixed), Some(1_073_741_824));
    }
}
