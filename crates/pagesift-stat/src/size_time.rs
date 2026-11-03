//! 按"输入字节数 → 处理耗时"给时限：`ln t = β₀ + β₁·ln b + r`，普通最小二乘（一元回归的闭式解，
//! 充分统计量 Σ1、Σx、Σx²、Σy、Σxy、Σy² 可以直接相加合并）。
//!
//! 时限取一次新观测的预测上界 `exp(β̂₀ + β̂₁·ln b + s·z(1 − δ/n))`：残差按正态近似，对样本里的 `n`
//! 份输入做 Bonferroni（整个样本上错杀一份本来能处理完的输入的概率不超过 δ）。比见过的最大输入还大时
//! 不按斜率（通常 < 1）外推，按线性外推（耗时至少和字节数同比增长），免得大文件被过早截断。
//!
//! 样本少于 `⌈ln δ / ln(1−δ)⌉`（δ = 0.05 时 59）个时不给时限（[`SizeTime::timeout`] 返回 `None`），
//! 调用方用先验：这么多样本里至少有一个落在 `q(1−δ)` 之上的概率才达到 `1−δ`，尾部才有证据。
//! 超时的那些是截尾样本，调用方不记（只会让时限偏宽）。

use std::time::Duration;

use crate::Delta;
use crate::special::normal_quantile;

/// 一元对数回归的充分统计量和见过的最大输入。
#[derive(Debug, Clone, Default)]
pub struct SizeTime {
    n: f64,
    sx: f64,
    sxx: f64,
    sy: f64,
    sxy: f64,
    syy: f64,
    x_max: f64,
}

impl SizeTime {
    /// 没有样本的模型。
    pub fn new() -> Self {
        Self::default()
    }

    /// 记一次处理成功的耗时。
    pub fn observe(&mut self, bytes: usize, elapsed: Duration) {
        let secs = elapsed.as_secs_f64();
        if bytes == 0 || secs <= 0.0 {
            return;
        }
        let (x, y) = ((bytes as f64).ln(), secs.ln());
        if self.n == 0.0 {
            self.x_max = x;
        }
        self.n += 1.0;
        self.sx += x;
        self.sxx += x * x;
        self.sy += y;
        self.sxy += x * y;
        self.syy += y * y;
        self.x_max = self.x_max.max(x);
    }

    pub fn count(&self) -> u64 {
        self.n as u64
    }

    /// `bytes` 字节的输入的时限；样本不够时 `None`。
    pub fn timeout(&self, bytes: usize, delta: Delta) -> Option<Duration> {
        let d = delta.get();
        let need = (d.ln() / (1.0 - d).ln()).ceil();
        if self.n < need {
            return None;
        }
        let det = self.n * self.sxx - self.sx * self.sx;
        // 所有输入一样大时斜率无定义，按 β₁ = 0（只用均值）
        let b1 = if det.abs() > f64::EPSILON * self.n * self.sxx {
            (self.n * self.sxy - self.sx * self.sy) / det
        } else {
            0.0
        };
        let b0 = (self.sy - b1 * self.sx) / self.n;
        // [推导] 残差方差 s² = (Σy² − β₀Σy − β₁Σxy)/(n − 2)，两个参数各占一个自由度
        let s2 = (self.syy - b0 * self.sy - b1 * self.sxy) / (self.n - 2.0);
        let s = s2.max(0.0).sqrt();
        let tail = s * normal_quantile(1.0 - d / self.n);
        let x = (bytes.max(1) as f64).ln();
        let line = |x: f64| b0 + b1 * x + tail;
        let ln_t = if x > self.x_max {
            line(self.x_max) + (x - self.x_max)
        } else {
            line(x)
        };
        Some(Duration::try_from_secs_f64(ln_t.exp()).unwrap_or(Duration::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    #[test]
    fn no_timeout_until_the_tail_has_evidence() {
        let mut m = SizeTime::new();
        for i in 0..58 {
            m.observe(1000 + i, Duration::from_millis(10));
        }
        assert_eq!(m.timeout(1000, Delta::DEFAULT), None);
        m.observe(2000, Duration::from_millis(10));
        assert!(m.timeout(1000, Delta::DEFAULT).is_some());
    }

    /// 合成：t = 1e-7 · b^0.9 · 对数正态噪声（σ = 0.3）。时限覆盖样本里每一个观测，且随大小增长。
    #[test]
    fn covers_the_sample_and_grows_with_size() {
        let mut rng = Rng::seed(11);
        let mut m = SizeTime::new();
        let mut obs = Vec::new();
        for _ in 0..2000 {
            let b = (1e3 * (rng.uniform() * 10.0).exp()) as usize;
            let t = 1e-7 * (b as f64).powf(0.9) * (0.3 * rng.normal()).exp();
            let t = Duration::from_secs_f64(t);
            m.observe(b, t);
            obs.push((b, t));
        }
        let d = Delta::DEFAULT;
        let missed = obs
            .iter()
            .filter(|(b, t)| m.timeout(*b, d).unwrap() < *t)
            .count();
        assert!(missed <= 1, "{missed}");
        let (small, big) = (
            m.timeout(10_000, d).unwrap(),
            m.timeout(10_000_000, d).unwrap(),
        );
        assert!(small < big);
        // 超出见过的范围按线性外推
        let far = m.timeout(1_000_000_000_000, d).unwrap();
        let edge = m.timeout((m.x_max.exp()) as usize, d).unwrap();
        assert!(far > edge * 10);
    }
}
