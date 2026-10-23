//! 物理内存占用（RSS）实时监控与 12 GiB 绝对熔断守卫（100% Safe Rust）。

use std::sync::atomic::{AtomicUsize, Ordering};

/// 内存告警阈值（10 GiB）
pub const DEFAULT_WARN_BYTES: usize = 10 * 1024 * 1024 * 1024;
/// 内存绝对熔断硬上限（12 GiB）
pub const DEFAULT_MAX_BYTES: usize = 12 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetStatus {
    /// 内存健康（< 10 GiB）
    Ok,
    /// 内存进入高位告警区（10 GiB ~ 12 GiB）：施加背压并加速刷盘
    Warning,
    /// 达到 12 GiB 绝对熔断线：立即暂停派发并拒绝新任务，防止系统 OOM 斩杀
    CircuitBreaker,
}

/// 物理内存安全守卫
#[derive(Debug)]
pub struct MemGuard {
    warn_bytes: usize,
    max_bytes: usize,
    page_size: usize,
    cached_rss: AtomicUsize,
}

impl Default for MemGuard {
    fn default() -> Self {
        Self::new(DEFAULT_WARN_BYTES, DEFAULT_MAX_BYTES)
    }
}

impl MemGuard {
    pub fn new(warn_bytes: usize, max_bytes: usize) -> Self {
        Self {
            warn_bytes,
            max_bytes,
            page_size: 4096,
            cached_rss: AtomicUsize::new(0),
        }
    }

    /// 读取当前进程物理内存占用（RSS，单位字节）。
    /// 在非 Linux 系统（如 macOS/Windows，无 /proc 文件系统）且无 unsafe C-FFI 时，
    /// 提供 32 MiB 基础模拟水位，确保跨平台测试与调度安全。
    pub fn current_rss_bytes(&self) -> usize {
        let rss = read_proc_rss_bytes(self.page_size).unwrap_or(32 * 1024 * 1024);
        self.cached_rss.store(rss, Ordering::Relaxed);
        rss
    }

    /// 评估当前内存水位状态。
    pub fn check(&self) -> BudgetStatus {
        let rss = self.current_rss_bytes();
        if rss >= self.max_bytes {
            BudgetStatus::CircuitBreaker
        } else if rss >= self.warn_bytes {
            BudgetStatus::Warning
        } else {
            BudgetStatus::Ok
        }
    }

    /// 上次采样的 RSS 字节数。
    pub fn last_rss_bytes(&self) -> usize {
        self.cached_rss.load(Ordering::Relaxed)
    }

    pub fn warn_bytes(&self) -> usize {
        self.warn_bytes
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }
}

/// Linux 读取 /proc/self/statm 第二项（resident pages）。非 Linux 系统返回 None。
fn read_proc_rss_bytes(page_size: usize) -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        use std::fs::File;
        use std::io::Read;
        let mut file = File::open("/proc/self/statm").ok()?;
        let mut buf = [0u8; 128];
        let n = file.read(&mut buf).ok()?;
        let s = std::str::from_utf8(&buf[..n]).ok()?;
        let mut parts = s.split_whitespace();
        let _size = parts.next()?;
        let resident: usize = parts.next()?.parse().ok()?;
        Some(resident * page_size)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = page_size;
        None
    }
}
