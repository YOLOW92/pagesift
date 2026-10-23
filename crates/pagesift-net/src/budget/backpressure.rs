//! 主动内存背压控制器。
//! 当内存上升至高位时，主动对调度器施加延迟惩罚并强制落盘，防止无界膨胀。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::mem_guard::{BudgetStatus, MemGuard};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressureLevel {
    Normal,
    Throttling,
    Halted,
}

/// 主动背压协调器
pub struct BackpressureController {
    guard: Arc<MemGuard>,
    is_paused: AtomicBool,
}

impl BackpressureController {
    pub fn new(guard: Arc<MemGuard>) -> Self {
        Self {
            guard,
            is_paused: AtomicBool::new(false),
        }
    }

    /// 评估当前的背压等级。
    pub fn evaluate(&self) -> PressureLevel {
        match self.guard.check() {
            BudgetStatus::Ok => {
                self.is_paused.store(false, Ordering::Release);
                PressureLevel::Normal
            }
            BudgetStatus::Warning => {
                self.is_paused.store(false, Ordering::Release);
                PressureLevel::Throttling
            }
            BudgetStatus::CircuitBreaker => {
                self.is_paused.store(true, Ordering::Release);
                PressureLevel::Halted
            }
        }
    }

    /// 检查调度器是否需要暂停派发新任务。
    #[inline]
    pub fn should_pause(&self) -> bool {
        self.is_paused.load(Ordering::Acquire)
    }

    /// 根据背压等级计算每个工作协程推荐的主动休眠惩罚（毫秒）。
    pub fn throttle_delay(&self) -> Duration {
        match self.evaluate() {
            PressureLevel::Normal => Duration::ZERO,
            PressureLevel::Throttling => Duration::from_millis(15),
            PressureLevel::Halted => Duration::from_millis(100),
        }
    }

    pub fn guard(&self) -> &MemGuard {
        &self.guard
    }
}
