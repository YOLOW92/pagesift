//! Hardware topology, system memory budget, CPU saturation, and rolling latency telemetry.

use std::collections::VecDeque;
use std::fs;
use std::sync::Mutex;
use std::thread::available_parallelism;
use std::time::Instant;

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

/// Dynamic CPU utilization tracker calculating delta work over delta total from /proc/stat.
#[derive(Debug)]
pub struct CpuSaturationTracker {
    prev_work: Mutex<u64>,
    prev_total: Mutex<u64>,
}

impl Default for CpuSaturationTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuSaturationTracker {
    pub fn new() -> Self {
        Self {
            prev_work: Mutex::new(0),
            prev_total: Mutex::new(0),
        }
    }

    /// Returns current system-wide CPU utilization in percent (0.0% ~ 100.0%).
    pub fn sample_utilization(&self) -> f64 {
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

                    let mut pw = self.prev_work.lock().unwrap();
                    let mut pt = self.prev_total.lock().unwrap();

                    let d_work = work.saturating_sub(*pw);
                    let d_total = total.saturating_sub(*pt);
                    *pw = work;
                    *pt = total;

                    if d_total > 0 {
                        return ((d_work as f64 / d_total as f64) * 100.0).clamp(0.0, 100.0);
                    }
                }
            }
        }
        0.0
    }
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
            self.logical_cores.clamp(8, 32)
        } else {
            (self.logical_cores / 2).clamp(2, 16)
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
        let cores = available_parallelism().map(|n| n.get()).unwrap_or(4);
        let mem = Self::read_memory();
        let fd_limit = detect_fd_limit();

        let total_mb = (mem.total_bytes / (1024 * 1024)) as usize;
        let ram_per_core_mb = if cores > 0 { total_mb / cores } else { 512 };

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
        // CPU baseline: 16 concurrent streams per core (scales with 2, 4, 32, 80, 384+ cores)
        let cpu_base = cores * 16;
        let cpu_max = cores * 32;

        let base_concurrency = cpu_base.min(mem_cap).min(fd_cap).max(4);
        let max_concurrency = cpu_max.min(mem_cap).min(fd_cap).max(base_concurrency);
        let min_concurrency = (cores * 2).clamp(2, 32);

        // Active sites scaling: proportional to concurrency
        let active_sites = (cores * 16).min(max_concurrency * 2).clamp(16, 2048);

        // Inflight buffer budget (proportional to total memory, clamped between 32MB and 2GB)
        let inflight_budget = ((mem.total_bytes / 32) as usize).clamp(32 * 1024 * 1024, 2 * 1024 * 1024 * 1024);
        let batch_io = (cores * 512 * 1024).clamp(1024 * 1024, 64 * 1024 * 1024);

        Self {
            logical_cores: cores,
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
                } else if let Some(val) = line.strip_prefix("MemAvailable:") {
                    if let Some(kb) = parse_kb(val) {
                        available_bytes = kb * 1024;
                    }
                }
            }
        }

        // Check cgroup v2 memory limit (/sys/fs/cgroup/memory.max)
        if let Ok(cgroup_max) = fs::read_to_string("/sys/fs/cgroup/memory.max") {
            let trimmed = cgroup_max.trim();
            if trimmed != "max" {
                if let Ok(bytes) = trimmed.parse::<u64>() {
                    total_bytes = total_bytes.min(bytes);
                }
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
            if let Some(resident_pages_str) = parts.next() {
                if let Ok(pages) = resident_pages_str.parse::<u64>() {
                    return pages * 4096;
                }
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
                if parts.len() >= 5 {
                    if let Ok(soft) = parts[3].parse::<usize>() {
                        return soft;
                    }
                }
            }
        }
    }
    4096
}

fn parse_kb(s: &str) -> Option<u64> {
    let mut parts = s.trim().split_whitespace();
    let num_str = parts.next()?;
    num_str.parse::<u64>().ok()
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
}
