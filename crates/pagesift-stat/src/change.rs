//! 均值变化检测：双侧 CUSUM（Page–Hinkley 形式）。
//!
//! E. S. Page, "Continuous Inspection Schemes", Biometrika 41, 1954；
//! D. V. Hinkley, Biometrika 58(3), 1971。正态均值从 `μ₀` 移到 `μ₀ ± μ_Δ` 时，
//! 单步对数似然比是 `(μ_Δ/σ²)(x − μ₀ ∓ μ_Δ/2)`；把累计和超过 `ln(1/δ)` 作为报警
//! （Wald SPRT 的门槛，Ann. Math. Statist. 16, 1945），换算到观测尺度就是
//! `h = (σ²/μ_Δ)·ln(1/δ)`。Lorden（Ann. Math. Statist. 42, 1971）证明此门槛下
//! 平均误报间隔不少于 `1/δ`，即每个观测的误报率不超过 `δ`。
//!
//! 与原文的差别：`μ₀`、`σ²` 用当前平稳段的样本估计（[`Moments`]），不是已知值。

use crate::moments::Moments;
use crate::{Delta, Stat};

/// 变化方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shift {
    Up,
    Down,
}

/// 检测到变化时交出旧段，调用方把它降为新段的先验。
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub shift: Shift,
    pub old: Moments,
}

#[derive(Debug, Clone)]
pub struct ChangeDetector {
    /// 决策需要分辨的均值差 `μ_Δ`（观测单位）。
    resolution: f64,
    delta: Delta,
    segment: Moments,
    up: f64,
    down: f64,
}

impl ChangeDetector {
    pub fn new(resolution: f64, delta: Delta) -> Self {
        assert!(resolution > 0.0, "change resolution must be positive");
        Self {
            resolution,
            delta,
            segment: Moments::default(),
            up: 0.0,
            down: 0.0,
        }
    }

    /// 当前平稳段的累计统计。
    pub fn segment(&self) -> &Moments {
        &self.segment
    }

    /// 报警门槛 `h = (σ²/μ_Δ)·ln(1/δ)`。
    pub fn threshold(&self) -> f64 {
        self.segment.variance() / self.resolution * (1.0 / self.delta.get()).ln()
    }

    /// 加一个观测；检测到变化时返回旧段并从这个观测开始新段。
    pub fn push(&mut self, x: f64) -> Option<Change> {
        let var = self.segment.variance();
        if var.is_finite() && var > 0.0 {
            let mu = self.segment.mean();
            let k = 0.5 * self.resolution;
            self.up = (self.up + x - mu - k).max(0.0);
            self.down = (self.down - (x - mu) - k).max(0.0);
            let h = self.threshold();
            let shift = if self.up > h {
                Some(Shift::Up)
            } else if self.down > h {
                Some(Shift::Down)
            } else {
                None
            };
            if let Some(shift) = shift {
                let old = std::mem::take(&mut self.segment);
                self.up = 0.0;
                self.down = 0.0;
                self.segment.push(x);
                return Some(Change { shift, old });
            }
        }
        self.segment.push(x);
        None
    }

    /// 合并另一个检测器的当前段（例如别的工人上的同一指标）。累计量不可合并，清零重新累计。
    pub fn absorb(&mut self, other: &Self) {
        self.segment.merge(&other.segment);
        self.up = 0.0;
        self.down = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    #[test]
    fn false_alarm_rate_is_below_delta_and_steps_are_found() {
        let delta = Delta::DEFAULT;
        let mut rng = Rng::seed(21);
        let mut det = ChangeDetector::new(1.0, delta);
        let n = 200_000;
        let alarms = (0..n)
            .filter(|_| det.push(10.0 + rng.normal()).is_some())
            .count();
        assert!((alarms as f64) / (n as f64) < delta.get(), "{alarms}");

        // 阶跃 +1σ：报警方向正确，延迟在几十个观测以内
        let mut det = ChangeDetector::new(1.0, delta);
        (0..2000).for_each(|_| {
            det.push(rng.normal());
        });
        let mut found = None;
        for i in 0..200 {
            if let Some(c) = det.push(1.0 + rng.normal()) {
                found = Some((i, c.shift));
                break;
            }
        }
        let (lag, shift) = found.expect("step not detected");
        assert_eq!(shift, Shift::Up);
        assert!(lag < 60, "{lag}");
    }
}
