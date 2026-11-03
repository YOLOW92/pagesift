//! 本地单调时钟。租约、心跳都只用本机时钟上的相对时长，不比较不同机器的时钟。
//!
//! 测试用 [`ManualClock`] 手动推进时间，不用真实等待。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub trait Clock: Send + Sync + 'static {
    /// 自某个固定起点以来的毫秒数，单调不减。
    fn now_ms(&self) -> u64;

    /// 墙钟（Unix 毫秒）：只用于按 UTC 日切分配额、记录请求时刻（跨机器回放要用同一把尺子）。
    fn unix_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, ms)
    }
}

/// `Instant` 起点的单调时钟。
#[derive(Debug)]
pub struct MonoClock {
    origin: Instant,
}

impl MonoClock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            origin: Instant::now(),
        })
    }
}

impl Clock for MonoClock {
    fn now_ms(&self) -> u64 {
        ms(self.origin.elapsed())
    }
}

/// 手动推进的时钟（故障注入测试用）。
#[derive(Debug, Default)]
pub struct ManualClock {
    now: AtomicU64,
}

impl ManualClock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn advance(&self, d: Duration) {
        self.now.fetch_add(ms(d), Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }

    /// 手动时钟的墙钟与单调时钟相同（从 Unix 纪元起算）。
    fn unix_ms(&self) -> u64 {
        self.now_ms()
    }
}

pub(crate) fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}
