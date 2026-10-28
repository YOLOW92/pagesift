//! 进程级调优。整个 pagesift 只有这里有 unsafe：调用 glibc 的 `mallopt`，以及小块分配缓存
//! （[`SmallCache`]，全局分配器）。
//!
//! 只应该由 pagesift 自己的可执行程序调用；Python、Node 绑定跑在别人的进程里，
//! 不能替宿主改分配器参数。

#![deny(unsafe_code)]

mod alloc;
pub use alloc::SmallCache;
#[doc(hidden)]
pub use alloc::small_cache_simulate_numa;

/// 固定 glibc 的 mmap 阈值。
///
/// glibc 默认会在释放大块内存后把 mmap 阈值调高（最高 32 MB），之后页面大小的缓冲区
/// （几百 KB 到几 MB 的 HTML、DOM、Markdown）都改从堆里分配，用完不还给系统。
/// 并发爬取时 RSS 因此是实际使用量的两倍多。固定在 256 KB 后这些块用完立即归还：
/// 本地 20 站点爬取的峰值 RSS 从 188 MB 降到约 75 MB，CPU 多约 8%。
/// 非 glibc 平台什么也不做。
///
/// 同时把 CPU 拓扑（NUMA 节点、每个逻辑 CPU 分到的 L2）告诉 [`SmallCache`]。
pub fn tune_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        let (cpu_node, l2_share) = topology::detect();
        alloc::configure(&cpu_node, l2_share);
        /// 显式设置后 glibc 不再动态调整阈值。
        const MMAP_THRESHOLD: libc::c_int = 256 * 1024;
        const ARENA_MAX: libc::c_int = 4;
        // SAFETY: mallopt 只修改分配器参数，任何时候调用都安全；返回 0 表示参数不被接受，忽略即可。
        #[allow(unsafe_code)]
        unsafe {
            libc::mallopt(libc::M_MMAP_THRESHOLD, MMAP_THRESHOLD);
            libc::mallopt(libc::M_ARENA_MAX, ARENA_MAX);
        }
    }
}

/// 显式通知 glibc 分配器向操作系统释放无用堆页面。
pub fn trim_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        #[allow(unsafe_code)]
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

/// 从 sysfs 读分配缓存要的拓扑。单节点机器只读一个文件。
#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod topology {
    use std::fs;

    const SYS: &str = "/sys/devices/system";

    pub(crate) fn detect() -> (Vec<(usize, usize)>, Option<usize>) {
        let mut cpu_node = Vec::new();
        let nodes = read(&format!("{SYS}/node/online")).map_or_else(Vec::new, |s| cpu_list(&s));
        if nodes.len() > 1 {
            for node in nodes {
                if let Some(cpus) = read(&format!("{SYS}/node/node{node}/cpulist")) {
                    cpu_node.extend(cpu_list(&cpus).into_iter().map(|cpu| (cpu, node)));
                }
            }
        }
        (cpu_node, l2_share())
    }

    /// cpu0 的 L2 大小除以共用它的逻辑 CPU 数（SMT 兄弟共用一份 L2）。
    fn l2_share() -> Option<usize> {
        for index in 0.. {
            let dir = format!("{SYS}/cpu/cpu0/cache/index{index}");
            let level = read(&format!("{dir}/level"))?;
            if level.trim() != "2" {
                continue;
            }
            let size = size(&read(&format!("{dir}/size"))?)?;
            let sharing = read(&format!("{dir}/shared_cpu_list"))
                .map_or(1, |s| cpu_list(&s).len())
                .max(1);
            return Some(size / sharing);
        }
        None
    }

    fn read(path: &str) -> Option<String> {
        fs::read_to_string(path).ok()
    }

    /// "1024K"、"2M"、"512"。
    pub(crate) fn size(s: &str) -> Option<usize> {
        let s = s.trim();
        let (digits, unit) = match s.as_bytes().last()? {
            b'K' => (&s[..s.len() - 1], 1 << 10),
            b'M' => (&s[..s.len() - 1], 1 << 20),
            b'G' => (&s[..s.len() - 1], 1 << 30),
            _ => (s, 1),
        };
        digits.parse::<usize>().ok().map(|n| n * unit)
    }

    /// "0-3,8,10-11" → [0, 1, 2, 3, 8, 10, 11]。
    pub(crate) fn cpu_list(s: &str) -> Vec<usize> {
        let mut out = Vec::new();
        for part in s.trim().split(',').filter(|p| !p.is_empty()) {
            let mut ends = part.splitn(2, '-').map(|n| n.trim().parse::<usize>());
            match (ends.next(), ends.next()) {
                (Some(Ok(a)), None) => out.push(a),
                (Some(Ok(a)), Some(Ok(b))) if a <= b => out.extend(a..=b),
                _ => {}
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn sysfs_lists_and_sizes_parse() {
            assert_eq!(cpu_list("0-3,8,10-11\n"), vec![0, 1, 2, 3, 8, 10, 11]);
            assert_eq!(cpu_list("0"), vec![0]);
            assert_eq!(cpu_list(""), Vec::<usize>::new());
            assert_eq!(size("1024K\n"), Some(1 << 20));
            assert_eq!(size("2M"), Some(2 << 20));
            assert_eq!(size("512"), Some(512));
            assert_eq!(size("x"), None);
        }

        #[test]
        fn this_machine_reports_an_l2_share() {
            let (_, l2) = detect();
            if std::path::Path::new("/sys/devices/system/cpu/cpu0/cache/index2").exists() {
                assert!(l2.is_some_and(|b| b >= 4096), "{l2:?}");
            }
        }
    }
}
