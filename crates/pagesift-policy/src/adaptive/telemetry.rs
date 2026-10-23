//! Hardware topology, system memory budget, and cgroups telemetry.

use std::fs;
use std::thread::available_parallelism;

/// Classified hardware topology profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopologyCategory {
    /// High cores (>=16) and high RAM (>=1 GB/core). E.g. 32C/64G, 64C/128G.
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

/// Hardware topology and auto-tuned tuning parameters.
#[derive(Debug, Clone)]
pub struct HardwareTopology {
    /// Number of logical CPU threads available.
    pub logical_cores: usize,
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
    /// Pure autonomous scheduling without manual flags!
    pub fn tune_for_graph(&self, hosts: usize, per_host: usize) -> AdaptiveSchedule {
        let per_host_conc = if per_host >= 5000 {
            // Deep graph: drain heavy single host faster
            self.logical_cores.clamp(8, 24)
        } else {
            // Wide graph: balanced per-host concurrency
            (self.logical_cores / 2).clamp(4, 8)
        };

        // Starting baseline global concurrency (probed upward dynamically via AIMD)
        let global_conc = match self.category {
            TopologyCategory::HighRamHighCores => (self.logical_cores * 8).clamp(64, 256),
            TopologyCategory::LowRamHighCores => (self.logical_cores * 8).clamp(64, 160),
            TopologyCategory::HighRamLowCores => (self.logical_cores * 8).clamp(32, 128),
            TopologyCategory::Constrained => (self.logical_cores * 4).clamp(16, 64),
        };

        // Queuing theory saturation: sized according to max ceiling to ensure zero starvation during probes
        let saturation_workers =
            (self.max_concurrency.div_ceil(per_host_conc) * 4).clamp(16, self.active_sites_cap);
        let active_workers = saturation_workers.min(hosts.max(1));

        AdaptiveSchedule {
            global_concurrency: global_conc,
            per_host_concurrency: per_host_conc,
            active_workers,
            inflight_budget_bytes: self.inflight_payload_budget_bytes,
            batch_io_bytes: self.batch_io_bytes,
        }
    }

    /// Detects current hardware and memory environment, tuning parameters accordingly.
    pub fn detect() -> Self {
        let cores = available_parallelism().map(|n| n.get()).unwrap_or(4);
        let mem = Self::read_memory();

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

        let (base_conc, max_conc, min_conc, active_sites, inflight_budget, batch_io) =
            match category {
                TopologyCategory::HighRamHighCores => (
                    (cores * 8).clamp(64, 512),
                    (cores * 16).clamp(128, 1024),
                    cores.max(16),
                    (cores * 16).clamp(128, 512),
                    256 * 1024 * 1024, // 256 MB inflight buffer
                    16 * 1024 * 1024,  // 16 MB I/O batch
                ),
                TopologyCategory::HighRamLowCores => (
                    (cores * 6).clamp(16, 128),
                    (cores * 12).clamp(32, 256),
                    cores.max(4),
                    (cores * 8).clamp(32, 128),
                    192 * 1024 * 1024, // 192 MB inflight buffer
                    8 * 1024 * 1024,   // 8 MB I/O batch
                ),
                TopologyCategory::LowRamHighCores => (
                    (cores * 8).clamp(64, 256),
                    (cores * 16).clamp(128, 512),
                    cores.max(8),
                    (cores * 16).clamp(128, 384),
                    128 * 1024 * 1024, // 128 MB inflight buffer (defends against OOM)
                    8 * 1024 * 1024,   // 8 MB I/O batch
                ),
                TopologyCategory::Constrained => (
                    (cores * 2).clamp(4, 32),
                    (cores * 4).clamp(8, 64),
                    2,
                    (cores * 4).clamp(8, 32),
                    32 * 1024 * 1024, // 32 MB inflight buffer
                    1 * 1024 * 1024,  // 1 MB I/O batch
                ),
            };

        Self {
            logical_cores: cores,
            category,
            ram_per_core_mb,
            base_concurrency: base_conc,
            max_concurrency: max_conc,
            min_concurrency: min_conc,
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
    fn test_memory_snapshot() {
        let mem = HardwareTopology::read_memory();
        assert!(mem.total_bytes > 0);
    }
}
