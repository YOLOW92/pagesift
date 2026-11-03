//! 进程资源：内存预算、文件描述符余量、由内存预算推出的单页 body 上限。
//!
//! - 内存预算 = 可用内存 − 系统其余部分占用的 `1−δ` 预测上界相对当前值的增量。"其余部分" =
//!   已用 − 本进程 RSS；每次刷新记一个样本，用正态预测区间 `μ + t(1−δ, n−1)·s·√(1+1/n)`
//!   估计它下次可能涨到多少，涨出来的部分不算进预算。只有一个样本时增量取 0（先验强度一份观测）。
//! - 单页 body 上限 = `min(用户上限, 内存预算 / 期望在途页数)`；用户显式配置的值始终是硬上限。
//!   刷新之前（冷启动）等于用户上限，和 0.81 一样。
//! - 文件描述符余量：软上限减去已打开数，按"每个在途请求平均占几个描述符"换算成还能加几个在途请求。
//!
//! 可用内存按 cgroup（v2 与 v1）和地址空间上限收紧：容器或 systemd scope 里真正的上限在 cgroup 里，
//! 只看 `/proc/meminfo` 会高估。刷新由事件驱动（全局并发控制器每个观测窗调一次 [`refresh`]）。

use std::fs;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use pagesift_stat::Delta;
use pagesift_stat::moments::Moments;
use pagesift_stat::special::t_quantile;

/// 一次内存读数（字节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemorySnapshot {
    /// 本进程可用的总内存（物理内存、cgroup 上限、地址空间上限中最小的）。
    pub total_bytes: u64,
    /// 现在还能用的内存。
    pub available_bytes: u64,
    /// 本进程 RSS。
    pub process_rss_bytes: u64,
}

/// 系统其余部分占用的样本。
static REST: Mutex<Option<Moments>> = Mutex::new(None);
/// 最近一次算出的内存预算；`u64::MAX` 表示还没算过或读不到。
static BUDGET: AtomicU64 = AtomicU64::new(u64::MAX);
/// 内存预算推出的 body 上限；`usize::MAX` 表示不约束（冷启动）。
static BODY_CAP: AtomicUsize = AtomicUsize::new(usize::MAX);
/// 见过的最大用户 body 上限（全局并发往上探时按它算每个在途请求最多占多少内存）。
static USER_BODY_CAP: AtomicUsize = AtomicUsize::new(0);

/// 读当前内存状况；读不到（非 Linux）时为 `None`。
pub fn read_memory() -> Option<MemorySnapshot> {
    let meminfo = fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = None;
    let mut available = None;
    for line in meminfo.lines() {
        if let Some(v) = line.strip_prefix("MemTotal:") {
            total = parse_kb(v).map(|kb| kb * 1024);
        } else if let Some(v) = line.strip_prefix("MemAvailable:") {
            available = parse_kb(v).map(|kb| kb * 1024);
        }
    }
    let (mut total, mut available) = (total?, available?);
    let (cgroup_limit, cgroup_current) = read_cgroup_memory();
    let address_space = fs::read_to_string("/proc/self/limits")
        .ok()
        .and_then(|l| parse_limit(&l, "Max address space"));
    let limit = match (cgroup_limit, address_space) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    if let Some(limit) = limit {
        total = total.min(limit);
        available = match cgroup_current {
            Some(current) => available.min(limit.saturating_sub(current)),
            None => available.min(limit),
        };
    }
    Some(MemorySnapshot {
        total_bytes: total,
        available_bytes: available,
        process_rss_bytes: process_rss().unwrap_or(0),
    })
}

/// 本进程 RSS（`/proc/self/statm` 第二列 × 页大小）。
pub fn process_rss() -> Option<u64> {
    let statm = fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    // [协议] statm 以页为单位；x86-64 与 aarch64 Linux 的基本页是 4 KiB
    Some(pages * 4096)
}

/// 用一次新读数刷新内存预算和 body 上限；`expected_inflight` 是期望的在途页数（全局并发目标）。
/// 返回新的内存预算（字节）。
pub fn refresh(expected_inflight: usize) -> Option<u64> {
    let m = read_memory()?;
    let rest = m
        .total_bytes
        .saturating_sub(m.available_bytes)
        .saturating_sub(m.process_rss_bytes) as f64;
    let headroom = {
        let mut samples = REST.lock().unwrap_or_else(|e| e.into_inner());
        let samples = samples.get_or_insert_with(Moments::default);
        samples.push(rest);
        rest_headroom(samples, rest, Delta::DEFAULT)
    };
    let budget = (m.available_bytes as f64 - headroom).max(0.0) as u64;
    BUDGET.store(budget, Ordering::Release);
    let per_page = budget / expected_inflight.max(1) as u64;
    BODY_CAP.store(
        usize::try_from(per_page).unwrap_or(usize::MAX),
        Ordering::Release,
    );
    Some(budget)
}

/// 系统其余部分下一次读数的 `1−δ` 预测上界比当前值高出多少（字节，≥ 0）。
fn rest_headroom(samples: &Moments, now: f64, delta: Delta) -> f64 {
    let n = samples.count();
    let s = samples.std_dev();
    if n < 2 || !s.is_finite() {
        return 0.0;
    }
    let nf = n as f64;
    let upper =
        samples.mean() + t_quantile(1.0 - delta.get(), nf - 1.0) * s * (1.0 + 1.0 / nf).sqrt();
    (upper - now).max(0.0)
}

/// 最近一次算出的内存预算（字节）；还没算过或读不到时为 `None`。
pub fn budget() -> Option<u64> {
    match BUDGET.load(Ordering::Acquire) {
        u64::MAX => None,
        b => Some(b),
    }
}

/// 单页 body 上限：`min(user, 内存预算 / 期望在途页数)`。
pub fn body_cap(user: usize) -> usize {
    USER_BODY_CAP.fetch_max(user, Ordering::Relaxed);
    user.min(BODY_CAP.load(Ordering::Acquire))
}

/// 按内存预算，全局在途数最多还能加几个：每个在途请求按最大用户 body 上限算。没有读数时不约束。
pub fn memory_room(current_inflight: f64) -> f64 {
    let Some(budget) = budget() else {
        return f64::INFINITY;
    };
    let per = USER_BODY_CAP.load(Ordering::Relaxed);
    if per == 0 {
        return f64::INFINITY;
    }
    (budget as f64 / per as f64 - current_inflight).max(0.0)
}

/// 打开文件数的软上限（`RLIMIT_NOFILE`）。
pub fn fd_soft_limit() -> Option<usize> {
    let limits = fs::read_to_string("/proc/self/limits").ok()?;
    parse_limit(&limits, "Max open files").map(|v| v as usize)
}

/// 本进程已打开的文件描述符数。
pub fn open_fds() -> Option<usize> {
    fs::read_dir("/proc/self/fd").ok().map(|d| d.count())
}

/// 按文件描述符余量，全局在途数最多还能加几个：`(软上限 − 已打开) / 每个在途请求平均占的描述符数`，
/// 后者 = 已打开 / 在途数（含空闲连接池、日志、浏览器管道等摊到每个请求上的份额）。读不到时不约束。
pub fn fd_room(inflight: f64) -> f64 {
    let (Some(limit), Some(open)) = (fd_soft_limit(), open_fds()) else {
        return f64::INFINITY;
    };
    let per = (open as f64 / inflight.max(1.0)).max(1.0);
    (limit.saturating_sub(open) as f64 / per).max(0.0)
}

fn parse_kb(s: &str) -> Option<u64> {
    s.split_whitespace().next()?.parse().ok()
}

/// `/proc/self/limits` 里某一行的软上限；`unlimited` 为 `None`。
fn parse_limit(limits: &str, name: &str) -> Option<u64> {
    let line = limits.lines().find(|l| l.starts_with(name))?;
    let rest = line[name.len()..].split_whitespace().next()?;
    rest.parse::<u64>().ok().filter(|v| *v > 0)
}

fn parse_cgroup_limit(value: &str) -> Option<u64> {
    let value = value.trim();
    if value == "max" {
        None
    } else {
        value.parse::<u64>().ok().filter(|limit| *limit > 0)
    }
}

/// 本进程内存 cgroup 的（上限文件，当前用量文件）候选，从最具体到容器根。
fn cgroup_memory_candidates() -> Vec<(String, String)> {
    let mut candidates = Vec::new();
    if let Ok(cgroup) = fs::read_to_string("/proc/self/cgroup") {
        for line in cgroup.lines() {
            let Some(path) = line.strip_prefix("0::") else {
                continue;
            };
            let path = path.trim();
            let base = if path == "/" {
                "/sys/fs/cgroup".to_string()
            } else {
                format!("/sys/fs/cgroup{path}")
            };
            candidates.push((
                format!("{base}/memory.max"),
                format!("{base}/memory.current"),
            ));
        }
        for line in cgroup.lines() {
            // [协议] /proc/self/cgroup 每行 "层级号:控制器:路径"（cgroups(7)）
            let mut parts = line.splitn(3, ':');
            let _id = parts.next();
            let controllers = parts.next().unwrap_or("");
            let path = parts.next().unwrap_or("").trim();
            if !controllers.split(',').any(|c| c == "memory") {
                continue;
            }
            let base = format!("/sys/fs/cgroup/memory{path}");
            candidates.push((
                format!("{base}/memory.limit_in_bytes"),
                format!("{base}/memory.usage_in_bytes"),
            ));
        }
    }
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

/// 各候选里最紧的有限上限，和对应的当前用量。
fn read_cgroup_memory() -> (Option<u64>, Option<u64>) {
    let mut best: Option<(u64, Option<u64>)> = None;
    for (limit_path, current_path) in cgroup_memory_candidates() {
        let limit = fs::read_to_string(limit_path)
            .ok()
            .and_then(|v| parse_cgroup_limit(&v));
        let current = fs::read_to_string(current_path)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok());
        match limit {
            Some(limit) if best.as_ref().is_none_or(|(b, _)| limit < *b) => {
                best = Some((limit, current));
            }
            None if best.is_none() && current.is_some() => best = Some((u64::MAX, current)),
            _ => {}
        }
    }
    match best {
        Some((u64::MAX, current)) => (None, current),
        Some((limit, current)) => (Some(limit), current),
        None => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_limits() {
        let limits = "Limit                     Soft Limit           Hard Limit           Units\n\
                      Max open files            1024                 524288               files\n\
                      Max address space         unlimited            unlimited            bytes\n";
        assert_eq!(parse_limit(limits, "Max open files"), Some(1024));
        assert_eq!(parse_limit(limits, "Max address space"), None);
        assert_eq!(parse_cgroup_limit("max\n"), None);
        assert_eq!(parse_cgroup_limit("1073741824\n"), Some(1 << 30));
    }

    #[test]
    fn headroom_is_the_prediction_bound_above_now() {
        let mut m = Moments::default();
        assert_eq!(
            rest_headroom(&m, 5.0, Delta::DEFAULT),
            0.0,
            "一份样本：不留余量"
        );
        for x in [100.0, 110.0, 90.0, 105.0, 95.0] {
            m.push(x);
        }
        // μ=100、s≈7.9、n=5：t(0.95,4)=2.132，上界 ≈ 100 + 2.132·7.9·1.095 ≈ 118.5
        let h = rest_headroom(&m, 100.0, Delta::DEFAULT);
        assert!((h - 18.5).abs() < 0.5, "{h}");
        assert_eq!(rest_headroom(&m, 200.0, Delta::DEFAULT), 0.0);
    }

    #[test]
    fn body_cap_never_exceeds_the_user_cap_and_starts_at_it() {
        // 冷启动（没刷新过）：等于用户上限
        if budget().is_none() {
            assert_eq!(body_cap(15 << 20), 15 << 20);
        }
        refresh(64);
        assert!(body_cap(15 << 20) <= 15 << 20);
        assert!(memory_room(0.0) >= 0.0 && fd_room(1.0) >= 0.0);
    }
}
