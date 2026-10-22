//! 严格内存天花板与主动背压防线。

pub mod backpressure;
pub mod leak_detector;
pub mod mem_guard;

pub use backpressure::{BackpressureController, PressureLevel};
pub use leak_detector::{LeakDetector, MemorySample};
pub use mem_guard::{BudgetStatus, DEFAULT_MAX_BYTES, DEFAULT_WARN_BYTES, MemGuard};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_mem_guard_rss_sampling() {
        let guard = MemGuard::default();
        let rss = guard.current_rss_bytes();
        assert!(rss > 0, "Current RSS must be greater than 0");
        assert_eq!(guard.check(), BudgetStatus::Ok);
    }

    #[test]
    fn test_backpressure_thresholds() {
        // 设置极低的阈值模拟告警与熔断
        let current = MemGuard::default().current_rss_bytes();
        let guard = Arc::new(MemGuard::new(current / 2, current + 1024 * 1024 * 1024));
        let ctrl = BackpressureController::new(guard);

        let level = ctrl.evaluate();
        assert_eq!(level, PressureLevel::Throttling);
        assert!(!ctrl.should_pause());
        assert!(ctrl.throttle_delay().as_millis() > 0);

        // 模拟熔断
        let breaker_guard = Arc::new(MemGuard::new(current / 4, current / 2));
        let breaker_ctrl = BackpressureController::new(breaker_guard);
        assert_eq!(breaker_ctrl.evaluate(), PressureLevel::Halted);
        assert!(breaker_ctrl.should_pause());
    }

    #[test]
    fn test_leak_detector_slopes() {
        let detector = LeakDetector::new(50, 1024.0);
        assert!(!detector.is_leaking());

        // 模拟平稳无泄漏
        for i in 0..10 {
            detector.record_sample(i * 100, 100 * 1024 * 1024);
        }
        assert!(!detector.is_leaking());
        assert_eq!(detector.memory_growth_slope(), 0.0);

        // 模拟异常急剧暴涨泄漏（每页暴增 100KB）
        for i in 11..20 {
            detector.record_sample(i * 100, 100 * 1024 * 1024 + (i - 10) * 100 * 100 * 1024);
        }
        assert!(detector.is_leaking());
    }
}
