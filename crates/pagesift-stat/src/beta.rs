//! 比例的贝塔后验：成功率、需要渲染率、被拒率。

use crate::rng::Rng;
use crate::special::{beta_quantile, normal_quantile};
use crate::{DecodeError, Delta, Reader, Stat, Writer};

/// 观测到的成功和失败次数（可以是带权的分数次数）。
///
/// 后验 `Beta(α₀ + s, β₀ + f)`，先验 `(α₀, β₀)` 由调用方给（上层后验收缩而来，见 [`crate::shrink`]，
/// 或无信息先验 `Beta(1,1)`）。合并只加计数，先验不参与合并，所以与顺序无关。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BetaCounts {
    pub success: f64,
    pub failure: f64,
}

/// 先验 `Beta(α, β)`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prior {
    pub alpha: f64,
    pub beta: f64,
}

impl Prior {
    /// [推导] 无信息先验 Beta(1,1)（Bayes–Laplace 均匀先验）
    pub const UNIFORM: Prior = Prior {
        alpha: 1.0,
        beta: 1.0,
    };

    /// 由均值 `μ` 和强度 `m`（等效样本数）构造：`α = μm`、`β = (1−μ)m`。
    pub fn from_mean(mu: f64, strength: f64) -> Self {
        Self {
            alpha: mu * strength,
            beta: (1.0 - mu) * strength,
        }
    }
}

impl BetaCounts {
    pub fn observe(&mut self, success: bool) {
        if success {
            self.success += 1.0;
        } else {
            self.failure += 1.0;
        }
    }

    pub fn observe_weighted(&mut self, success: f64, failure: f64) {
        self.success += success;
        self.failure += failure;
    }

    pub fn trials(&self) -> f64 {
        self.success + self.failure
    }

    /// 样本比例；没有观测时为 NaN。
    pub fn rate(&self) -> f64 {
        self.success / self.trials()
    }

    pub fn posterior(&self, prior: Prior) -> Posterior {
        Posterior {
            alpha: prior.alpha + self.success,
            beta: prior.beta + self.failure,
        }
    }
}

impl Stat for BetaCounts {
    fn merge(&mut self, o: &Self) {
        self.success += o.success;
        self.failure += o.failure;
    }

    fn encode(&self, w: &mut Writer) {
        w.f64(self.success);
        w.f64(self.failure);
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let s = Self {
            success: r.f64()?,
            failure: r.f64()?,
        };
        if !(s.success >= 0.0 && s.failure >= 0.0) {
            return Err(DecodeError::Invalid("beta counts"));
        }
        Ok(s)
    }
}

/// 贝塔后验。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Posterior {
    pub alpha: f64,
    pub beta: f64,
}

impl Posterior {
    pub fn mean(&self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }

    pub fn variance(&self) -> f64 {
        let s = self.alpha + self.beta;
        self.alpha * self.beta / (s * s * (s + 1.0))
    }

    /// 等尾可信区间 `[q(δ), q(1−δ)]`，两端各 `δ`。
    pub fn interval(&self, delta: Delta) -> (f64, f64) {
        let d = delta.get();
        (
            beta_quantile(self.alpha, self.beta, d),
            beta_quantile(self.alpha, self.beta, 1.0 - d),
        )
    }

    /// Thompson 采样：从后验抽一个值（Thompson, Biometrika 25, 1933）。
    pub fn sample(&self, rng: &mut Rng) -> f64 {
        rng.beta(self.alpha, self.beta)
    }
}

/// Wilson 得分区间（Wilson, JASA 22, 1927）：`k` 次成功、`n` 次试验，双侧水平 `1−2δ`
/// 时的 `(下界, 上界)`。
pub fn wilson(k: f64, n: f64, delta: Delta) -> (f64, f64) {
    if n <= 0.0 {
        return (0.0, 1.0);
    }
    let z = normal_quantile(1.0 - delta.get());
    let p = k / n;
    let z2n = z * z / n;
    // [推导] Wilson 区间：(p + z²/2n ± z√(p(1−p)/n + z²/4n²)) / (1 + z²/n)
    let centre = p + 0.5 * z2n;
    let half = z * (p * (1.0 - p) / n + 0.25 * z2n / n).sqrt(); // [推导] z²/4n² 项
    let den = 1.0 + z2n;
    (
        ((centre - half) / den).max(0.0),
        ((centre + half) / den).min(1.0),
    )
}

/// 同 [`wilson`]，但分位数 `z` 由调用方预先算好：热路径上反复比较区间时不必每次求 `Φ⁻¹`。
/// `z` 取 `Delta::z()`（单侧 `z(1−δ)`）或双侧的 `Φ⁻¹(1−δ/2)`。
pub fn wilson_z(k: f64, n: f64, z: f64) -> (f64, f64) {
    if n <= 0.0 {
        return (0.0, 1.0);
    }
    let p = k / n;
    let z2n = z * z / n;
    // [推导] Wilson 区间：(p + z²/2n ± z√(p(1−p)/n + z²/4n²)) / (1 + z²/n)
    let centre = p + 0.5 * z2n;
    let half = z * (p * (1.0 - p) / n + 0.25 * z2n / n).sqrt(); // [推导] z²/4n² 项
    let den = 1.0 + z2n;
    (
        ((centre - half) / den).max(0.0),
        ((centre + half) / den).min(1.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posterior_interval_and_wilson_cover_the_rate() {
        let mut c = BetaCounts::default();
        (0..300).for_each(|i| c.observe(i % 4 == 0));
        let post = c.posterior(Prior::UNIFORM);
        assert!((post.mean() - 76.0 / 302.0).abs() < 1e-12);
        let (lo, hi) = post.interval(Delta::DEFAULT);
        assert!(lo < 0.25 && 0.25 < hi);
        let (wl, wh) = wilson(75.0, 300.0, Delta::DEFAULT);
        assert!(wl < 0.25 && 0.25 < wh);
        // 大样本时两种区间接近
        assert!((wl - lo).abs() < 0.01 && (wh - hi).abs() < 0.01);
        assert_eq!(wilson(0.0, 0.0, Delta::DEFAULT), (0.0, 1.0));
        assert_eq!(
            wilson_z(75.0, 300.0, Delta::DEFAULT.z()),
            wilson(75.0, 300.0, Delta::DEFAULT)
        );
    }
}
