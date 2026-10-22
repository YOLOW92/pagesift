//! 长周期运行内存监控与渐进式泄漏探测器。
//! 实时跟踪内存增长斜率，确保 50,000 页压测中内存曲线平稳无泄漏。

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct MemorySample {
    pub timestamp: Instant,
    pub pages_completed: usize,
    pub rss_bytes: usize,
}

/// 渐进式泄漏探测器
pub struct LeakDetector {
    samples: Mutex<VecDeque<MemorySample>>,
    max_samples: usize,
    /// 允许的每页最大平均常驻内存增量（字节/页，默认 64 KiB，超过则判定为泄漏风险）
    max_growth_per_page: f64,
}

impl Default for LeakDetector {
    fn default() -> Self {
        Self::new(100, 64.0 * 1024.0)
    }
}

impl LeakDetector {
    pub fn new(max_samples: usize, max_growth_per_page: f64) -> Self {
        Self {
            samples: Mutex::new(VecDeque::with_capacity(max_samples)),
            max_samples: max_samples.max(10),
            max_growth_per_page,
        }
    }

    /// 记录一次内存与完成页数快照。
    pub fn record_sample(&self, pages_completed: usize, rss_bytes: usize) {
        let mut q = self.samples.lock().expect("lock poisoned");
        if q.len() >= self.max_samples {
            q.pop_front();
        }
        q.push_back(MemorySample {
            timestamp: Instant::now(),
            pages_completed,
            rss_bytes,
        });
    }

    /// 计算近期样本的平均内存增长斜率（字节/页）。
    pub fn memory_growth_slope(&self) -> f64 {
        let q = self.samples.lock().expect("lock poisoned");
        if q.len() < 2 {
            return 0.0;
        }
        let first = q.front().unwrap();
        let last = q.back().unwrap();

        let delta_pages = last.pages_completed.saturating_sub(first.pages_completed);
        if delta_pages == 0 {
            return 0.0;
        }
        let delta_bytes = (last.rss_bytes as f64) - (first.rss_bytes as f64);
        delta_bytes / (delta_pages as f64)
    }

    /// 检查是否存在潜在的渐进式内存泄漏。
    pub fn is_leaking(&self) -> bool {
        let q = self.samples.lock().expect("lock poisoned");
        if q.len() < 5 {
            return false;
        }
        drop(q);
        let slope = self.memory_growth_slope();
        slope > self.max_growth_per_page
    }

    /// 当前已采集的样本数量。
    pub fn sample_count(&self) -> usize {
        self.samples.lock().expect("lock poisoned").len()
    }
}
