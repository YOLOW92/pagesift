//! 浏览器自己的内存占用（实测 PSS）和给系统其余部分留的余量（DESIGN §5.4-M）。
//!
//! - **每个浏览器、每个标签多少内存**：从浏览器进程树读 PSS（`/proc/<pid>/smaps_rollup` 的 `Pss:`，
//!   子进程从 `/proc/<pid>/task/*/children` 递归；PSS 把共享页按共享进程数分摊，多个浏览器相加不会重复计）。
//!   空闲浏览器的 PSS 进 [`WindowMean`] 得"每个浏览器"；在用浏览器的 `(在用标签数, PSS − 每个浏览器)`
//!   进 [`WindowRatio`] 得"每个标签"（Cochran 比值估计）。先验是 [`Footprint`] 表（0.81 的现值）。
//! - **给系统其余部分留多少**：其余部分的占用 = `MemTotal − MemAvailable − 自己浏览器的 PSS 之和`；
//!   相邻两次采样之间它的增量取 `q(1−δ)` 的置信上界（[`WindowQuantile`]），即一个采样间隔里别的进程
//!   最多涨多少。下限是自己再开一个浏览器的占用（判断之后、下次采样之前自己最多再涨这么多）。
//!   先验是 `min_available_memory_mb`（0.81 的 1024 MB）。
//!
//! 采样只在后台健康检查里做（每个池一个任务，锁不会被争用），读 `/proc` 放在阻塞线程里。
//! 发布值放原子量，准入判断是一次原子读。窗口满足精度才发布，与当前值的差显著才替换（§5.7）。

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use pagesift_stat::Delta;
use pagesift_stat::hazard::WindowQuantile;
use pagesift_stat::shrink::significant;
use pagesift_stat::window::{WindowMean, WindowRatio};

use crate::engine::Footprint;

/// 一个浏览器进程树的 PSS（MB）；进程已经没了或读不到时返回 `None`。
pub(super) fn tree_pss_mb(pid: u32) -> Option<f64> {
    let mut kb = 0u64;
    let mut stack = vec![pid];
    while let Some(p) = stack.pop() {
        if let Ok(s) = std::fs::read_to_string(format!("/proc/{p}/smaps_rollup")) {
            kb += s
                .lines()
                .find_map(|l| l.strip_prefix("Pss:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
                .unwrap_or(0);
        } else if p == pid {
            return None;
        }
        if let Ok(tasks) = std::fs::read_dir(format!("/proc/{p}/task")) {
            for t in tasks.flatten() {
                if let Ok(c) = std::fs::read_to_string(t.path().join("children")) {
                    stack.extend(c.split_whitespace().filter_map(|x| x.parse::<u32>().ok()));
                }
            }
        }
    }
    Some(kb as f64 / 1024.0)
}

/// `(MemTotal, MemAvailable)`，MB。
fn meminfo_mb() -> Option<(f64, f64)> {
    let s = std::fs::read_to_string("/proc/meminfo").ok()?;
    let field = |k: &str| -> Option<f64> {
        s.lines()
            .find_map(|l| l.strip_prefix(k))?
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse::<f64>()
            .ok()
            .map(|kb| kb / 1024.0)
    };
    Some((field("MemTotal:")?, field("MemAvailable:")?))
}

struct Windows {
    idle: WindowMean,
    tab: WindowRatio,
    rest_up: WindowQuantile,
    last_rest: Option<f64>,
}

/// 一个池的内存学习：每个浏览器、每个标签的 PSS，和给系统其余部分留的余量。
pub(super) struct MemLearn {
    browser_mb: AtomicU64,
    tab_mb: AtomicU64,
    reserve_mb: AtomicU64,
    windows: Mutex<Windows>,
}

fn load(a: &AtomicU64) -> f64 {
    f64::from_bits(a.load(Ordering::Relaxed))
}

fn store(a: &AtomicU64, v: f64) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

impl MemLearn {
    pub(super) fn new(prior: Footprint, reserve_mb: u64) -> Self {
        let d = Delta::DEFAULT;
        Self {
            browser_mb: AtomicU64::new((prior.browser_mb as f64).to_bits()),
            tab_mb: AtomicU64::new((prior.tab_mb as f64).to_bits()),
            reserve_mb: AtomicU64::new((reserve_mb as f64).to_bits()),
            windows: Mutex::new(Windows {
                idle: WindowMean::default(),
                tab: WindowRatio::default(),
                rest_up: WindowQuantile::new(1.0 - d.get()),
                last_rest: None,
            }),
        }
    }

    /// 当前的每个浏览器、每个标签的内存（MB），先验是 Footprint 表。
    pub(super) fn browser_mb(&self) -> u64 {
        load(&self.browser_mb).round().max(0.0) as u64
    }

    pub(super) fn tab_mb(&self) -> u64 {
        load(&self.tab_mb).round().max(0.0) as u64
    }

    /// 开新标签或浏览器后，系统至少要剩多少可用内存（MB）。
    pub(super) fn reserve_mb(&self) -> u64 {
        load(&self.reserve_mb).round().max(0.0) as u64
    }

    /// 一次采样：每个本地浏览器的 `(pid, 在用标签数)`。在阻塞线程里调用。
    pub(super) fn sample(&self, slots: &[(u32, usize)]) {
        let d = Delta::DEFAULT;
        let pss: Vec<(f64, usize)> = slots
            .iter()
            .filter_map(|&(pid, n)| tree_pss_mb(pid).map(|m| (m, n)))
            .collect();
        let Ok(mut w) = self.windows.lock() else {
            return;
        };
        for &(m, n) in &pss {
            if n == 0 {
                if let Some(b) = w.idle.push(m, d) {
                    store(&self.browser_mb, b);
                }
            } else {
                let base = load(&self.browser_mb);
                if let Some(t) = w.tab.push(n as f64, m - base, d) {
                    store(&self.tab_mb, t.max(0.0));
                }
            }
        }
        let own: f64 = pss.iter().map(|(m, _)| m).sum();
        let Some((total, avail)) = meminfo_mb() else {
            return;
        };
        let rest = total - avail - own;
        if let Some(last) = w.last_rest.replace(rest)
            && let Some(p) = w.rest_up.push((rest - last).max(0.0), d)
        {
            // 下限：判断之后到下次采样前，自己最多再开一个浏览器
            let floor = load(&self.browser_mb) + load(&self.tab_mb);
            let new = p.upper.max(floor);
            let cur = load(&self.reserve_mb);
            let se = (p.upper - p.lower) / (2.0 * d.z());
            if significant(cur, new, se, d) {
                store(&self.reserve_mb, new);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_start_equals_the_footprint_table_and_reserve() {
        let fp = Footprint {
            browser_mb: 300,
            tab_mb: 200,
            browsers_per_4_cores: 1,
        };
        let m = MemLearn::new(fp, 1024);
        assert_eq!(
            (m.browser_mb(), m.tab_mb(), m.reserve_mb()),
            (300, 200, 1024)
        );
        // 进程不存在：不更新
        m.sample(&[(u32::MAX - 1, 0)]);
        assert_eq!((m.browser_mb(), m.tab_mb()), (300, 200));
    }

    #[test]
    fn reads_the_pss_of_this_process_tree() {
        let pss = tree_pss_mb(std::process::id());
        if std::path::Path::new("/proc/self/smaps_rollup").exists() {
            assert!(pss.unwrap() > 0.0);
        }
    }
}
