//! 按精度截窗的均值：不用固定的 EWMA 系数或固定窗长。
//!
//! 样本累进当前窗口；窗口均值的相对标准误 `SE/|均值| < δ` 时，这个窗口的估计已经够准，
//! 发布它并开新窗口。数据稳定时窗口自然变长，变化后最多一个窗口就跟上；只用最近一个
//! 完整窗口，不被很久以前的样本拖住（累计均值的毛病）。
//!
//! 单写者：每个线程或工人各持一个，发布出来的值由调用方放进原子量共享。

use crate::Delta;

/// 普通均值的窗口：`SE = s/√n`。
#[derive(Debug, Clone, Default)]
pub struct WindowMean {
    n: f64,
    sum: f64,
    sumsq: f64,
}

impl WindowMean {
    /// 加一个样本；窗口满足精度时返回窗口均值并清空。
    pub fn push(&mut self, x: f64, delta: Delta) -> Option<f64> {
        self.n += 1.0;
        self.sum += x;
        self.sumsq += x * x;
        if self.n < 2.0 {
            return None;
        }
        let mean = self.sum / self.n;
        let var = ((self.sumsq - self.n * mean * mean) / (self.n - 1.0)).max(0.0);
        if mean != 0.0 && (var / self.n).sqrt() < delta.get() * mean.abs() {
            *self = Self::default();
            return Some(mean);
        }
        None
    }
}

/// 比值估计 `R = Σy / Σx` 的窗口（例如每字节耗时 = 总耗时 / 总字节）。
///
/// 比值估计量的方差（Cochran, Sampling Techniques, 3rd ed., 1977, §6.3）：
/// `Var(R) ≈ Σ(yᵢ − R·xᵢ)² / ((n−1)·n·x̄²)`，其中 `Σ(y − Rx)² = Σy² − 2RΣxy + R²Σx²`。
#[derive(Debug, Clone, Default)]
pub struct WindowRatio {
    n: f64,
    sx: f64,
    sy: f64,
    sxx: f64,
    syy: f64,
    sxy: f64,
}

impl WindowRatio {
    /// 加一对 `(x, y)`；窗口满足精度时返回 `R` 并清空。`x ≤ 0` 的样本不计。
    pub fn push(&mut self, x: f64, y: f64, delta: Delta) -> Option<f64> {
        if x <= 0.0 {
            return None;
        }
        self.n += 1.0;
        self.sx += x;
        self.sy += y;
        self.sxx += x * x;
        self.syy += y * y;
        self.sxy += x * y;
        if self.n < 2.0 {
            return None;
        }
        let r = self.sy / self.sx;
        let resid = (self.syy - 2.0 * r * self.sxy + r * r * self.sxx).max(0.0);
        let xbar = self.sx / self.n;
        let var = resid / ((self.n - 1.0) * self.n * xbar * xbar);
        if r != 0.0 && var.sqrt() < delta.get() * r.abs() {
            *self = Self::default();
            return Some(r);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    #[test]
    fn publishes_once_precise_and_tracks_a_level_shift() {
        let d = Delta::default();
        let mut w = WindowMean::default();
        let mut rng = Rng::seed(1);
        let mut last = None;
        for _ in 0..10_000 {
            if let Some(m) = w.push(100.0 + 10.0 * rng.normal(), d) {
                last = Some(m);
            }
        }
        let m = last.unwrap();
        assert!((m - 100.0).abs() < 5.0, "{m}");
        // 水平变了以后，下一个发布值就是新水平，不被旧样本拖住
        let mut after = None;
        for _ in 0..10_000 {
            if let Some(m) = w.push(300.0 + 30.0 * rng.normal(), d) {
                after = Some(m);
            }
        }
        assert!((after.unwrap() - 300.0).abs() < 15.0);
    }

    #[test]
    fn ratio_estimates_cost_per_unit() {
        let d = Delta::default();
        let mut w = WindowRatio::default();
        let mut rng = Rng::seed(2);
        let mut last = None;
        for _ in 0..10_000 {
            let x = 1000.0 + 900.0 * rng.uniform();
            if let Some(r) = w.push(x, 3.0 * x * (1.0 + 0.2 * rng.normal()), d) {
                last = Some(r);
            }
        }
        assert!((last.unwrap() - 3.0).abs() < 0.15);
        assert_eq!(w.push(0.0, 1.0, d), None);
    }
}
