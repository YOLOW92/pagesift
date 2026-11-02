//! 系统资源读数：可用内存、CPU 时间，以及按它们估算的自动浏览器数。

use super::*;

/// 自动的浏览器数：可用内存（扣掉 `reserve_mb`）够开几个满载的浏览器，最多每 4 个核一个
/// （每个浏览器的 UI 线程、渲染进程和 GPU 进程大约要占满一个多核）。读不到内存时按核数算。
pub fn auto_browsers(tabs_per_browser: usize, reserve_mb: u64) -> usize {
    auto_browsers_for(EngineKind::Chrome.footprint(), tabs_per_browser, reserve_mb)
}

/// 同 [`auto_browsers`]，按给定内核的资源占用估算。
pub fn auto_browsers_for(fp: Footprint, tabs_per_browser: usize, reserve_mb: u64) -> usize {
    let cores = std::thread::available_parallelism().map_or(1, usize::from);
    let by_cpu = (cores * fp.browsers_per_4_cores / 4).max(1);
    let per_browser = (fp.browser_mb + fp.tab_mb * tabs_per_browser as u64).max(1);
    let by_memory = mem_available_mb().map_or(by_cpu, |avail| {
        (avail.saturating_sub(reserve_mb) / per_browser) as usize
    });
    by_memory.clamp(1, by_cpu)
}

/// Linux `/proc/stat` 的（空闲 + iowait，总计）jiffies；读不到时返回 `None`。
pub(super) fn cpu_times() -> Option<(u64, u64)> {
    let s = std::fs::read_to_string("/proc/stat").ok()?;
    let line = s.lines().next()?.strip_prefix("cpu ")?;
    let v: Vec<u64> = line
        .split_whitespace()
        .filter_map(|x| x.parse().ok())
        .collect();
    let idle = v.get(3)? + v.get(4).copied().unwrap_or(0);
    Some((idle, v.iter().sum()))
}

/// 系统可用内存（MB）；读不到时返回 `None`，不做限制。
/// Linux 读 MemAvailable，macOS 用 `vm_stat` 的 free + inactive + speculative 页。
pub(super) fn mem_available_mb() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("vm_stat").output().ok()?;
        parse_vm_stat(&String::from_utf8_lossy(&out.stdout))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let s = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kb: u64 = s
            .lines()
            .find_map(|l| l.strip_prefix("MemAvailable:"))?
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse()
            .ok()?;
        Some(kb / 1024)
    }
}

/// 解析 macOS `vm_stat` 输出，返回可用内存（MB）。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_vm_stat(s: &str) -> Option<u64> {
    let page: u64 = s
        .lines()
        .next()?
        .split("page size of")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |key: &str| -> u64 {
        s.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.trim().trim_end_matches('.').parse().ok())
            .unwrap_or(0)
    };
    let free = pages("Pages free:") + pages("Pages inactive:") + pages("Pages speculative:");
    Some(free * page / (1024 * 1024))
}

#[cfg(test)]
mod vm_stat_tests {
    #[test]
    fn parses_vm_stat() {
        let s = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free:                               1000.\nPages inactive:                           2000.\nPages speculative:                        1000.\n";
        assert_eq!(super::parse_vm_stat(s), Some(4000 * 16384 / 1048576));
    }
}
