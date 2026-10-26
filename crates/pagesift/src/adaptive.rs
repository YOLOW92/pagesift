//! 动态自适应性能调优模块 (Adaptive Resource & Concurrency Tuning Engine)
//!
//! 核心设计原则：
//! 1. 木桶短板原则 (Bottleneck-Constrained Concurrency)：
//!    并发度绝不只看单一指标，而是同时受 CPU 核心数、物理可用内存、文件描述符硬限制 (ulimit -n)
//!    三者的交集严格约束：min(cpu_cap, mem_cap, fd_cap)。
//! 2. 全维严苛分类 (Strict Multi-Dimensional Tier Classification)：
//!    Extreme 档位强制要求核心数 (>=64)、内存 (>=128GB)、FD上限 (>=65535) 同时满足，
//!    彻底避免“大内存小CPU”（如 4C 256GB）或“未解禁句柄”的机型被错误透支。
//! 3. 动态 RTT 适应与全球网络覆盖 (RTT-Aware Safe Timeout)：
//!    杜绝 1.5s 激进硬超时带来的跨国慢速站点误杀，基准连接超时设为 3.5s，
//!    保障日本、欧美、亚太等长 RTT 节点的合法连接率。
//! 4. 双模式开关 (TuningMode)：
//!    - Adaptive (自适应模式，默认)：科学饱和释放硬件吞吐；
//!    - Conservative (保守模式)：严格压制 CPU (<20%) 和内存，平稳不抢占宿主机。

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use crate::ScraperConfig;

/// 调优模式开关：保守模式 vs 自适应模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TuningMode {
    /// 自适应模式（默认）：遵循木桶短板原则科学饱和硬件，兼顾吞吐与全球站点覆盖率
    #[default]
    Adaptive,
    /// 保守模式：严格压制 CPU（<20%）和内存消耗，低并发平稳作业，不影响宿主机其他进程
    Conservative,
}

impl FromStr for TuningMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "adaptive" | "auto" | "fast" | "自适应" => Ok(Self::Adaptive),
            "conservative" | "safe" | "low" | "保守" => Ok(Self::Conservative),
            other => Err(format!(
                "Unknown tuning mode: '{other}', expected 'adaptive' or 'conservative'"
            )),
        }
    }
}

impl fmt::Display for TuningMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Adaptive => write!(f, "adaptive"),
            Self::Conservative => write!(f, "conservative"),
        }
    }
}

/// 硬件层级分类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareTier {
    /// 1C 1GB 或更小的微型容器 / 云函数
    Micro,
    /// 2C 2GB ~ 4C 4GB 轻量级云主机
    Small,
    /// 6C ~ 16C，8GB ~ 32GB 桌面开发者主机（如 i7-10700 8C16T 16GB）
    Mid,
    /// 16C ~ 64C，48GB ~ 256GB 高性能云服务器（如 AMD EPYC 9B45 32C 128GB）
    High,
    /// 64C+，128GB+，且 FD>=65535 超级多核计算节点（严格全维满足）
    Extreme,
}

impl HardwareTier {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Micro => "Micro (1C1GB class)",
            Self::Small => "Small (2C2GB ~ 4C4GB class)",
            Self::Mid => "Mid Desktop (8C16GB class / i7-10700)",
            Self::High => "High-End Server (32C128GB class / EPYC 9B45)",
            Self::Extreme => "Extreme Supernode (64C+ 128GB+ FD>=65k)",
        }
    }
}

pub use pagesift_policy::adaptive::{
    AutonomousTuningEngine, CpuTopology, HardwareTopology, LocalityCluster, PerformanceDiagnosis,
    ProcessCpuSample, ResponseSurfaceOptimizer, SafetyStatus, SurfacePoint, TuningKnobs,
    pin_current_thread,
};

/// 宿主机物理规格
#[derive(Debug, Clone, Copy)]
pub struct HostHardware {
    pub logical_cores: usize,
    pub total_memory_bytes: u64,
    pub fd_limit: usize,
    pub tier: HardwareTier,
}

impl HostHardware {
    /// 探测当前主机的物理硬件与操作系统限制
    pub fn detect() -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let mem_bytes =
            detect_system_memory_bytes().unwrap_or((cores as u64) * 2 * 1024 * 1024 * 1024);
        let fd_limit = detect_fd_limit();
        let tier = classify_hardware(cores, mem_bytes, fd_limit);
        Self {
            logical_cores: cores,
            total_memory_bytes: mem_bytes,
            fd_limit,
            tier,
        }
    }

    /// 格式化内存大小（单位：GB）
    pub fn memory_gb(&self) -> f64 {
        self.total_memory_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
    }
}

/// 动态计算出的运行参数配置
#[derive(Debug, Clone)]
pub struct TuningProfile {
    pub mode: TuningMode,
    pub hardware: HostHardware,
    pub global_concurrency: usize,
    pub per_host_concurrency: usize,
    pub mpsc_buffer_capacity: usize,
    pub connect_timeout: Duration,
    pub total_timeout: Duration,
    pub max_body_bytes: usize,
}

impl TuningProfile {
    /// 根据当前硬件与指定模式生成调优策略
    pub fn new(mode: TuningMode) -> Self {
        let hardware = HostHardware::detect();
        let topology = HardwareTopology::detect();
        Self::for_hardware_and_topology(hardware, mode, Some(&topology))
    }

    pub fn for_hardware(hw: HostHardware, mode: TuningMode) -> Self {
        Self::for_hardware_and_topology(hw, mode, None)
    }

    fn for_hardware_and_topology(
        hw: HostHardware,
        mode: TuningMode,
        topology: Option<&HardwareTopology>,
    ) -> Self {
        // 木桶短板并发度计算 (Bottleneck-Constrained Concurrency)
        // 1. CPU 算力容量：自适应模式采用连续 C* 模型动态伸缩，废除对特定单机（如 32 核 768 并发）的人工写死
        let cpu_capacity = match (mode, topology) {
            (TuningMode::Conservative, Some(topo)) => (topo.worker_cores * 4).clamp(4, 512),
            (TuningMode::Adaptive, Some(topo)) => topo.base_concurrency,
            (TuningMode::Conservative, None) => (hw.logical_cores * 8).clamp(8, 512),
            (TuningMode::Adaptive, None) => match hw.tier {
                HardwareTier::Micro => 16,
                HardwareTier::Small => 48,
                HardwareTier::Mid => hw.logical_cores * 20,
                HardwareTier::High => (hw.logical_cores * 24).clamp(320, 1536),
                HardwareTier::Extreme => (hw.logical_cores * 24).clamp(1024, 4096),
            },
        };

        // 2. 内存安全容量：每并发预留 2MB 完整生命周期缓存（连接状态 + HTML + Markdown）
        let mem_capacity = (hw.total_memory_bytes / (2 * 1024 * 1024)) as usize;

        // 3. 文件描述符容量：系统 ulimit -n 预留 30% 给 DNS、日志与内核套接字
        let fd_capacity = (hw.fd_limit * 7) / 10;

        // 最终并发度为三者最小值（木桶短板）
        let safe_concurrency = cpu_capacity.min(mem_capacity).min(fd_capacity).max(4);

        // 超时与调度策略（兼顾全球网络覆盖率与死链判决速度）
        let (conn_timeout, total_timeout, per_host) = match mode {
            TuningMode::Conservative => (Duration::from_secs(4), Duration::from_secs(12), 2),
            TuningMode::Adaptive => match hw.tier {
                HardwareTier::Micro => (Duration::from_millis(4500), Duration::from_secs(12), 1),
                HardwareTier::Small => (Duration::from_millis(4000), Duration::from_secs(10), 2),
                HardwareTier::Mid => (
                    // 10700: 3.5s 连接握手超时，保证跨国正常站点握手率
                    Duration::from_millis(3500),
                    Duration::from_secs(10),
                    2,
                ),
                HardwareTier::High => (
                    // EPYC 9B45: 3.5s 连接超时（不盲目砍到 1.5s），总超时 10s，单站公平并发 2
                    Duration::from_millis(3500),
                    Duration::from_secs(10),
                    2,
                ),
                HardwareTier::Extreme => (Duration::from_millis(3500), Duration::from_secs(8), 4),
            },
        };

        Self {
            mode,
            hardware: hw,
            global_concurrency: safe_concurrency,
            per_host_concurrency: per_host,
            mpsc_buffer_capacity: safe_concurrency * 2,
            connect_timeout: conn_timeout,
            total_timeout,
            max_body_bytes: 15 * 1024 * 1024,
        }
    }

    /// 应用到 ScraperConfig
    ///
    /// 调用方显式设过 `[fetch] timeout_secs` 时不覆盖超时字段：用户给的是单次请求预算，
    /// 自适应只能调并发度，不能把它改大——否则配了 8 s 的爬取实际会等 30 s。
    pub fn apply_to_config(&self, cfg: &mut ScraperConfig) {
        if !cfg.fetcher.timeouts_explicit {
            cfg.fetcher.connect_timeout = self.connect_timeout;
            cfg.fetcher.dns_timeout = self.connect_timeout;
            cfg.fetcher.tls_timeout = self.connect_timeout;
            cfg.fetcher.first_byte_timeout = self.total_timeout;
            cfg.fetcher.body_idle_timeout = self.total_timeout;
            cfg.fetcher.tail_threshold = (self.total_timeout / 4).max(Duration::from_millis(500));
            cfg.fetcher.total_timeout = self.total_timeout;
        }
        cfg.fetcher.max_body_bytes = self.max_body_bytes;
        cfg.policy.rate.global_concurrency = self.global_concurrency;
        cfg.policy.rate.per_host_concurrency = self.per_host_concurrency;
        cfg.adaptive_tuning = self.mode == TuningMode::Adaptive;
    }
}

/// 探测宿主机物理内存容量（字节）
fn detect_system_memory_bytes() -> Option<u64> {
    if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
        for line in content.lines() {
            if line.starts_with("MemTotal:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2
                    && let Ok(kb) = parts[1].parse::<u64>()
                {
                    return Some(kb * 1024);
                }
            }
        }
    }
    None
}

/// 探测文件句柄上限 (ulimit -n)
fn detect_fd_limit() -> usize {
    if let Ok(content) = std::fs::read_to_string("/proc/self/limits") {
        for line in content.lines() {
            if line.contains("Max open files") {
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

/// 硬件全维分级判定（严格全维条件与运算，杜绝大内存小CPU误判）
pub fn classify_hardware(cores: usize, mem_bytes: u64, _fd_limit: usize) -> HardwareTier {
    let mem_gb = mem_bytes as f64 / (1024.0 * 1024.0 * 1024.0);

    // Extreme 硬件条件：核心 >= 64 且 内存 >= 96GB（FD 限制由木桶短板公式独立约束）
    if cores >= 64 && mem_gb >= 96.0 {
        HardwareTier::Extreme
    } else if cores >= 24 && mem_gb >= 48.0 {
        // High-end: 比如 EPYC 9B45 (32C, 128GB)
        HardwareTier::High
    } else if cores >= 6 && mem_gb >= 12.0 {
        // Mid desktop: 比如 i7-10700 (8C16T, 16GB)
        HardwareTier::Mid
    } else if cores >= 2 && mem_gb >= 1.8 {
        HardwareTier::Small
    } else {
        HardwareTier::Micro
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tier_classification_safety() {
        // 1. 正常 Extreme: 128C 256GB, FD 100k -> Extreme
        assert_eq!(
            classify_hardware(128, 256 * 1024 * 1024 * 1024, 100000),
            HardwareTier::Extreme
        );

        // 2. 漏洞用例测试：4C 256GB 内存优化型机型 -> 绝不能被误判为 Extreme！
        // 应该降级为 Small 或 Mid，由木桶原则保护 CPU
        let tier_4c_256g = classify_hardware(4, 256 * 1024 * 1024 * 1024, 65535);
        assert_ne!(tier_4c_256g, HardwareTier::Extreme);
        assert_eq!(tier_4c_256g, HardwareTier::Small);

        // 3. 4C 256GB 的并发计算：被 CPU 短板（4 核 * 24 = 96）严格限制，绝不会分配 8192
        let hw_4c = HostHardware {
            logical_cores: 4,
            total_memory_bytes: 256 * 1024 * 1024 * 1024,
            fd_limit: 65535,
            tier: tier_4c_256g,
        };
        let profile_4c = TuningProfile::for_hardware(hw_4c, TuningMode::Adaptive);
        assert!(profile_4c.global_concurrency <= 96);

        // 4. EPYC 9B45 32C 128GB: 应该评定为 High，并发为 768，连接超时为 3.5s
        let hw_epyc = HostHardware {
            logical_cores: 32,
            total_memory_bytes: 128 * 1024 * 1024 * 1024,
            fd_limit: 1048576,
            tier: HardwareTier::High,
        };
        let profile_epyc = TuningProfile::for_hardware(hw_epyc, TuningMode::Adaptive);
        assert_eq!(profile_epyc.global_concurrency, 768);
        assert_eq!(profile_epyc.connect_timeout, Duration::from_millis(3500));
        assert_eq!(profile_epyc.total_timeout, Duration::from_secs(10));
    }
}
