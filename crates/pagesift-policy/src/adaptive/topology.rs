//! Linux CPU topology discovery and topology-aware worker planning.
//!
//! The planner deliberately separates physical cores from SMT siblings and keeps
//! NUMA domains explicit. Callers can use the returned CPU groups and
//! [`pin_current_thread`] when constructing topology-aware executors.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread::available_parallelism;

type SocketNode = (usize, usize);
type PhysicalCore = (usize, usize);
type CoreThreads = BTreeMap<PhysicalCore, Vec<usize>>;

/// One logical CPU visible to this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuThread {
    pub cpu_id: usize,
    pub socket_id: usize,
    pub numa_node: usize,
    pub core_id: usize,
}

/// CPUs belonging to one NUMA locality domain, physical threads first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalityCluster {
    pub socket_id: usize,
    pub numa_node: usize,
    pub physical_cpus: Vec<usize>,
    pub smt_siblings: Vec<usize>,
}

impl LocalityCluster {
    pub fn logical_cpus(&self) -> impl Iterator<Item = usize> + '_ {
        self.physical_cpus
            .iter()
            .chain(self.smt_siblings.iter())
            .copied()
    }
}

/// Topology and cgroup capacity available to the current process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuTopology {
    pub logical_cpus: usize,
    pub physical_cores: usize,
    pub sockets: usize,
    pub numa_nodes: usize,
    pub threads_per_core: usize,
    /// CPU quota rounded up to schedulable CPU units, if a finite cgroup quota exists.
    pub quota_cores: Option<usize>,
    /// Safe default number of CPU workers. SMT siblings are capacity reserve, not defaults.
    pub worker_cores: usize,
    pub clusters: Vec<LocalityCluster>,
}

impl CpuTopology {
    pub fn detect() -> Self {
        let fallback = available_parallelism().map(|n| n.get()).unwrap_or(1).max(1);
        let allowed = read_allowed_cpu_list().unwrap_or_else(|| (0..fallback).collect());
        let records = read_linux_topology(&allowed);
        Self::from_threads(records, read_cpu_quota_cores())
    }

    fn from_threads(mut threads: Vec<CpuThread>, quota_cores: Option<usize>) -> Self {
        if threads.is_empty() {
            threads.push(CpuThread {
                cpu_id: 0,
                socket_id: 0,
                numa_node: 0,
                core_id: 0,
            });
        }
        threads.sort_by_key(|cpu| cpu.cpu_id);

        let logical_cpus = threads.len();
        let physical_keys: BTreeSet<_> = threads
            .iter()
            .map(|cpu| (cpu.socket_id, cpu.core_id))
            .collect();
        let physical_cores = physical_keys.len().max(1);
        let sockets = threads
            .iter()
            .map(|cpu| cpu.socket_id)
            .collect::<BTreeSet<_>>()
            .len()
            .max(1);
        let numa_nodes = threads
            .iter()
            .map(|cpu| cpu.numa_node)
            .collect::<BTreeSet<_>>()
            .len()
            .max(1);
        let threads_per_core = logical_cpus.div_ceil(physical_cores).max(1);

        let mut grouped: BTreeMap<SocketNode, CoreThreads> = BTreeMap::new();
        for cpu in threads {
            grouped
                .entry((cpu.socket_id, cpu.numa_node))
                .or_default()
                .entry((cpu.socket_id, cpu.core_id))
                .or_default()
                .push(cpu.cpu_id);
        }

        let clusters = grouped
            .into_iter()
            .map(|((socket_id, numa_node), cores)| {
                let mut physical_cpus = Vec::with_capacity(cores.len());
                let mut smt_siblings = Vec::new();
                for mut cpus in cores.into_values() {
                    cpus.sort_unstable();
                    if let Some(primary) = cpus.first().copied() {
                        physical_cpus.push(primary);
                    }
                    smt_siblings.extend(cpus.into_iter().skip(1));
                }
                LocalityCluster {
                    socket_id,
                    numa_node,
                    physical_cpus,
                    smt_siblings,
                }
            })
            .collect::<Vec<_>>();

        let quota_capacity = quota_cores.unwrap_or(logical_cpus).clamp(1, logical_cpus);
        let worker_cores = physical_cores.min(quota_capacity).max(1);

        Self {
            logical_cpus,
            physical_cores,
            sockets,
            numa_nodes,
            threads_per_core,
            quota_cores,
            worker_cores,
            clusters,
        }
    }

    /// Physical CPUs grouped by NUMA domain. SMT siblings are intentionally excluded.
    pub fn physical_worker_groups(&self) -> Vec<Vec<usize>> {
        let mut remaining = self.worker_cores;
        let mut groups = Vec::new();
        for cluster in &self.clusters {
            if remaining == 0 {
                break;
            }
            let take = remaining.min(cluster.physical_cpus.len());
            if take > 0 {
                groups.push(cluster.physical_cpus[..take].to_vec());
                remaining -= take;
            }
        }
        groups
    }

    /// All logical CPU IDs available to the async runtime, ordered physical
    /// cores first and SMT siblings second within each NUMA domain. A finite
    /// cgroup quota caps the list. No P1 benchmark optimum is encoded here.
    pub fn runtime_worker_cpus(&self) -> Vec<usize> {
        self.worker_activation_tiers()
            .pop()
            .unwrap_or_else(|| vec![0])
    }

    /// Cumulative worker activation tiers. The first tier contains one thread
    /// per physical core. Later tiers add at most one SMT sibling per NUMA node
    /// per step, so every probe expands locality domains evenly until all
    /// allowed logical CPUs are available.
    pub fn worker_activation_tiers(&self) -> Vec<Vec<usize>> {
        let groups = self.physical_worker_groups();
        // Interleave NUMA domains so every prefix of the worker list is
        // locality-balanced. This matters when total concurrency is smaller
        // than the worker count: assigning the remainder to the first ranks
        // must not concentrate all runnable work on one socket or NUMA node.
        let mut active = interleave_groups(&groups);
        let maximum = self.quota_cores.unwrap_or(self.logical_cpus);
        let mut tiers = vec![active.clone()];
        let mut sibling_index = 0usize;
        while active.len() < maximum {
            let before = active.len();
            for cluster in &self.clusters {
                if active.len() >= maximum {
                    break;
                }
                if let Some(cpu) = cluster.smt_siblings.get(sibling_index) {
                    active.push(*cpu);
                }
            }
            if active.len() == before {
                break;
            }
            tiers.push(active.clone());
            sibling_index += 1;
        }
        tiers
    }

    pub fn physical_cores_per_numa(&self) -> usize {
        self.clusters
            .iter()
            .map(|cluster| cluster.physical_cpus.len())
            .max()
            .unwrap_or(self.worker_cores)
            .min(self.worker_cores)
            .max(1)
    }

    /// One dispatcher per eight physical workers inside each NUMA domain.
    ///
    /// This preserves the useful shape of the old `ceil(N/8)` rule for a
    /// single-socket host, but prevents one global shard count from erasing
    /// NUMA boundaries on multi-socket systems.
    pub fn recommended_dispatch_shards(&self) -> usize {
        self.physical_worker_groups()
            .iter()
            .map(|group| group.len().div_ceil(8))
            .sum::<usize>()
            .clamp(1, 64)
    }
}

fn interleave_groups(groups: &[Vec<usize>]) -> Vec<usize> {
    let mut interleaved = Vec::with_capacity(groups.iter().map(Vec::len).sum());
    let longest = groups.iter().map(Vec::len).max().unwrap_or(0);
    for index in 0..longest {
        for group in groups {
            if let Some(cpu) = group.get(index) {
                interleaved.push(*cpu);
            }
        }
    }
    interleaved
}

/// Pins the current OS thread to one logical CPU. Returns false when affinity
/// is unavailable or the requested CPU is outside the platform CPU-set range.
#[cfg(target_os = "linux")]
pub fn pin_current_thread(cpu_id: usize) -> bool {
    use rustix::thread::{CpuSet, sched_setaffinity};

    if cpu_id >= CpuSet::MAX_CPU {
        return false;
    }
    let mut set = CpuSet::new();
    set.set(cpu_id);
    sched_setaffinity(None, &set).is_ok()
}

/// Restricts the current thread to a locality set while leaving Linux free to
/// schedule it on any CPU inside that set.
#[cfg(target_os = "linux")]
pub fn pin_current_thread_to_set(cpu_ids: &[usize]) -> bool {
    use rustix::thread::{CpuSet, sched_setaffinity};

    if cpu_ids.is_empty() || cpu_ids.iter().any(|cpu| *cpu >= CpuSet::MAX_CPU) {
        return false;
    }
    let mut set = CpuSet::new();
    for cpu in cpu_ids {
        set.set(*cpu);
    }
    sched_setaffinity(None, &set).is_ok()
}

/// Applies one affinity mask to every thread currently owned by this process.
///
/// Tokio's worker pool cannot be resized after construction, but Linux can
/// change where those workers are eligible to run. Updating every TID makes
/// CPU-capacity probes real (including NUMA and SMT effects) without restarting
/// the request pipeline. Threads which exit during the `/proc/self/task` walk
/// are harmless and are ignored.
#[cfg(target_os = "linux")]
pub fn set_process_affinity(cpu_ids: &[usize]) -> usize {
    use rustix::thread::{CpuSet, Pid, sched_setaffinity};

    if cpu_ids.is_empty() || cpu_ids.iter().any(|cpu| *cpu >= CpuSet::MAX_CPU) {
        return 0;
    }
    let mut set = CpuSet::new();
    for cpu in cpu_ids {
        set.set(*cpu);
    }

    fs::read_dir("/proc/self/task")
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
        .filter_map(Pid::from_raw)
        .filter(|tid| sched_setaffinity(Some(*tid), &set).is_ok())
        .count()
}

#[cfg(not(target_os = "linux"))]
pub fn pin_current_thread_to_set(_cpu_ids: &[usize]) -> bool {
    false
}

#[cfg(not(target_os = "linux"))]
pub fn set_process_affinity(_cpu_ids: &[usize]) -> usize {
    0
}

#[cfg(not(target_os = "linux"))]
pub fn pin_current_thread(_cpu_id: usize) -> bool {
    false
}

fn read_allowed_cpu_list() -> Option<Vec<usize>> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let value = status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))?;
    parse_cpu_list(value)
}

fn read_linux_topology(allowed: &[usize]) -> Vec<CpuThread> {
    allowed
        .iter()
        .map(|&cpu_id| {
            let base = PathBuf::from(format!("/sys/devices/system/cpu/cpu{cpu_id}"));
            let socket_id = read_usize(base.join("topology/physical_package_id")).unwrap_or(0);
            let core_id = read_usize(base.join("topology/core_id")).unwrap_or(cpu_id);
            let numa_node = fs::read_dir(&base)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .find_map(|name| name.strip_prefix("node")?.parse::<usize>().ok())
                .unwrap_or(socket_id);
            CpuThread {
                cpu_id,
                socket_id,
                numa_node,
                core_id,
            }
        })
        .collect()
}

fn read_usize(path: impl AsRef<Path>) -> Option<usize> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_cpu_quota_cores() -> Option<usize> {
    if let Ok(cpu_max) = fs::read_to_string("/sys/fs/cgroup/cpu.max") {
        return parse_cpu_max(&cpu_max);
    }

    let quota = [
        "/sys/fs/cgroup/cpu/cpu.cfs_quota_us",
        "/sys/fs/cgroup/cpu.cfs_quota_us",
    ]
    .into_iter()
    .find_map(|path| fs::read_to_string(path).ok())?
    .trim()
    .parse::<i64>()
    .ok()?;
    if quota < 0 {
        return None;
    }
    let period = [
        "/sys/fs/cgroup/cpu/cpu.cfs_period_us",
        "/sys/fs/cgroup/cpu.cfs_period_us",
    ]
    .into_iter()
    .find_map(|path| fs::read_to_string(path).ok())?
    .trim()
    .parse::<u64>()
    .ok()?
    .max(1);
    Some((quota as u64).div_ceil(period).max(1) as usize)
}

fn parse_cpu_max(value: &str) -> Option<usize> {
    let mut fields = value.split_whitespace();
    let quota = fields.next()?;
    if quota == "max" {
        return None;
    }
    let quota = quota.parse::<u64>().ok()?;
    let period = fields.next()?.parse::<u64>().ok()?.max(1);
    Some(quota.div_ceil(period).max(1) as usize)
}

fn parse_cpu_list(value: &str) -> Option<Vec<usize>> {
    let mut cpus = BTreeSet::new();
    for item in value.trim().split(',').filter(|item| !item.is_empty()) {
        if let Some((start, end)) = item.split_once('-') {
            let start = start.trim().parse::<usize>().ok()?;
            let end = end.trim().parse::<usize>().ok()?;
            if start > end {
                return None;
            }
            cpus.extend(start..=end);
        } else {
            cpus.insert(item.trim().parse::<usize>().ok()?);
        }
    }
    (!cpus.is_empty()).then(|| cpus.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_ranges_and_cgroup_quota() {
        assert_eq!(
            parse_cpu_list("0-2,8,10-11"),
            Some(vec![0, 1, 2, 8, 10, 11])
        );
        assert_eq!(parse_cpu_max("250000 100000"), Some(3));
        assert_eq!(parse_cpu_max("max 100000"), None);
    }

    #[test]
    fn models_dual_socket_snc_and_keeps_smt_out_of_default_workers() {
        let mut threads = Vec::new();
        for socket in 0..2 {
            for core_in_socket in 0..48 {
                let core_id = socket * 48 + core_in_socket;
                let node = socket * 2 + core_in_socket / 24;
                threads.push(CpuThread {
                    cpu_id: core_id,
                    socket_id: socket,
                    numa_node: node,
                    core_id,
                });
                threads.push(CpuThread {
                    cpu_id: core_id + 96,
                    socket_id: socket,
                    numa_node: node,
                    core_id,
                });
            }
        }
        let topo = CpuTopology::from_threads(threads, None);
        assert_eq!(topo.logical_cpus, 192);
        assert_eq!(topo.physical_cores, 96);
        assert_eq!(topo.worker_cores, 96);
        assert_eq!(topo.sockets, 2);
        assert_eq!(topo.numa_nodes, 4);
        assert_eq!(topo.threads_per_core, 2);
        assert_eq!(topo.physical_cores_per_numa(), 24);
        assert_eq!(topo.recommended_dispatch_shards(), 12);
        assert_eq!(topo.runtime_worker_cpus().len(), 192);
        let tiers = topo.worker_activation_tiers();
        assert_eq!(tiers.first().map(Vec::len), Some(96));
        assert_eq!(&tiers[0][..8], &[0, 24, 48, 72, 1, 25, 49, 73]);
        assert_eq!(tiers.get(1).map(Vec::len), Some(100));
        assert_eq!(tiers.last().map(Vec::len), Some(192));
        assert_eq!(
            topo.physical_worker_groups()
                .iter()
                .map(Vec::len)
                .sum::<usize>(),
            96
        );
        assert!(
            topo.physical_worker_groups()
                .iter()
                .flatten()
                .all(|cpu| *cpu < 96)
        );
    }

    #[test]
    fn cgroup_quota_caps_worker_capacity() {
        let threads = (0..32)
            .map(|cpu_id| CpuThread {
                cpu_id,
                socket_id: 0,
                numa_node: 0,
                core_id: cpu_id,
            })
            .collect();
        let topo = CpuTopology::from_threads(threads, Some(4));
        assert_eq!(topo.physical_cores, 32);
        assert_eq!(topo.worker_cores, 4);
        assert_eq!(topo.recommended_dispatch_shards(), 1);
        assert_eq!(topo.physical_worker_groups(), vec![vec![0, 1, 2, 3]]);
    }
}
