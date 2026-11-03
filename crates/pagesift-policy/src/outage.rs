//! 故障持续时长分布与下次尝试的时刻。
//!
//! 一次故障从第一次失败算起，到下一次成功为止（Kaplan–Meier 的事件）；故障还没恢复条目就被
//! 清掉的记为右截尾。已经持续了 `e`，下次尝试放在**有把握**恢复概率达到 `1−δ` 的时刻：
//! 条件完成概率 `1 − S(t)/S(e)` 的 Wilson 下界 ≥ `1−δ` 的最小 `t`
//! （[`Survival::confident_time_to`]）。样本不够时返回 `None`，调用方用先验（0.81 的指数退避）。
//!
//! 只在故障开始和结束时更新，不在请求热路径上。

use std::sync::Mutex;
use std::time::Duration;

use pagesift_stat::Delta;
use pagesift_stat::survival::Survival;

#[derive(Debug, Default)]
pub(crate) struct Outages {
    km: Mutex<Survival>,
    delta: Delta,
}

impl Outages {
    /// 故障持续 `lasted` 后恢复。
    pub(crate) fn recovered(&self, lasted: Duration) {
        if let Ok(mut km) = self.km.lock() {
            km.event(lasted.as_secs_f64());
        }
    }

    /// 故障持续了 `lasted`，还没恢复就不再跟踪（条目被清理）。
    pub(crate) fn abandoned(&self, lasted: Duration) {
        if let Ok(mut km) = self.km.lock() {
            km.censor(lasted.as_secs_f64());
        }
    }

    /// 已经持续 `lasted` 的故障，过多久再试。没有足够把握时 `None`。
    pub(crate) fn retry_in(&self, lasted: Duration) -> Option<Duration> {
        let km = self.km.lock().ok()?;
        let e = lasted.as_secs_f64();
        let t = km.confident_time_to(1.0 - self.delta.get(), e, self.delta)?;
        Some(Duration::from_secs_f64((t - e).max(0.0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_stat::rng::Rng;

    #[test]
    fn cold_start_defers_to_the_prior() {
        let o = Outages::default();
        assert_eq!(o.retry_in(Duration::ZERO), None);
        for _ in 0..10 {
            o.recovered(Duration::from_secs(5));
        }
        assert_eq!(o.retry_in(Duration::ZERO), None, "10 次恢复还不够有把握");
    }

    /// 故障时长 ~ Exp(均值 20 s)：无记忆，已持续多久都应在约 20·ln 20 ≈ 60 s 后再试；
    /// 截尾（被清理的故障）不改变这一点（KM 的截尾正确性）。
    #[test]
    fn exponential_outages_give_a_memoryless_retry_time() {
        let o = Outages::default();
        let mut rng = Rng::seed(11);
        for i in 0..20_000 {
            let t = -20.0 * rng.uniform().max(1e-12).ln();
            if i % 4 == 0 && t > 10.0 {
                o.abandoned(Duration::from_secs_f64(10.0));
            } else {
                o.recovered(Duration::from_secs_f64(t));
            }
        }
        for lasted in [0.0, 30.0] {
            let r = o.retry_in(Duration::from_secs_f64(lasted)).unwrap();
            assert!((r.as_secs_f64() - 60.0).abs() < 8.0, "{lasted}: {r:?}");
        }
    }
}
