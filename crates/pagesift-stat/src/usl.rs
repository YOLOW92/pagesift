//! 通用可扩展性定律（USL）的在线拟合：由（在途数，吞吐）观测估计参数和峰值并发。

use crate::regress::RegressionSums;
use crate::special::t_quantile;
use crate::{DecodeError, Delta, Reader, Stat, Writer};

/// 通用可扩展性定律（N. J. Gunther, Guerrilla Capacity Planning, Springer 2007, 第 4 章）：
/// `X(N) = λN / (1 + σ(N−1) + κN(N−1))`。
///
/// 线性化 `N/X = a + bN + cN²`，`a = (1−σ)/λ`、`b = (σ−κ)/λ`、`c = κ/λ`，
/// 于是 `λ = 1/(a+b+c)`、`σ = 1 − aλ`、`κ = cλ`，峰值在 `N* = √((1−σ)/κ) = √(a/c)`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usl {
    sums: RegressionSums,
}

/// USL 的参数估计。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UslFit {
    pub lambda: f64,
    pub sigma: f64,
    pub kappa: f64,
    /// 峰值并发 `N*`；`κ ≤ 0`（数据里还看不到回落）时为无穷大。
    pub peak: f64,
    /// `N*` 的单侧 `1−δ` 置信下界（delta 方法）。
    pub peak_lower: f64,
}

impl Usl {
    /// 记录一个工作点：在途数 `n`、吞吐 `x`。
    pub fn push(&mut self, n: f64, x: f64) {
        if self.sums.dims() == 0 {
            // [推导] 线性化 N/X = a + bN + cN² 的三个系数
            self.sums = RegressionSums::new(3);
        }
        if n > 0.0 && x > 0.0 {
            self.sums.push(&[1.0, n, n * n], n / x);
        }
    }

    pub fn fit(&self, delta: Delta) -> Option<UslFit> {
        let f = self.sums.fit()?;
        let (a, b, c) = (f.beta[0], f.beta[1], f.beta[2]);
        let lambda = 1.0 / (a + b + c);
        if !(lambda > 0.0 && a > 0.0) {
            return None;
        }
        let sigma = 1.0 - a * lambda;
        let kappa = c * lambda;
        if c <= 0.0 {
            return Some(UslFit {
                lambda,
                sigma,
                kappa,
                peak: f64::INFINITY,
                peak_lower: f64::INFINITY,
            });
        }
        let peak = (a / c).sqrt();
        // g = √(a/c)：∂g/∂a = 1/(2√(ac))，∂g/∂c = −√a/(2c^{3/2})；Var g = Jᵀ Σ J（只涉及 a、c）
        let ga = 0.5 / (a * c).sqrt();
        let gc = -0.5 * a.sqrt() / c.powf(1.5); // [推导] ∂√(a/c)/∂c
        let var = ga * ga * f.cov(0, 0) + 2.0 * ga * gc * f.cov(0, 2) + gc * gc * f.cov(2, 2);
        let t = t_quantile(1.0 - delta.get(), f.dof);
        let peak_lower = (peak - t * var.max(0.0).sqrt()).max(1.0);
        Some(UslFit {
            lambda,
            sigma,
            kappa,
            peak,
            peak_lower,
        })
    }
}

impl Stat for Usl {
    fn merge(&mut self, o: &Self) {
        self.sums.merge(&o.sums);
    }

    fn encode(&self, w: &mut Writer) {
        self.sums.encode(w);
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            sums: RegressionSums::decode(r)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    /// 合成数据恢复 USL 参数。
    #[test]
    fn usl_recovers_parameters() {
        let (lambda, sigma, kappa) = (100.0, 0.05, 0.001);
        let mut rng = Rng::seed(5);
        let mut usl = Usl::default();
        for n in 1..=80 {
            let n = n as f64;
            let x = lambda * n / (1.0 + sigma * (n - 1.0) + kappa * n * (n - 1.0));
            usl.push(n, x * (1.0 + 0.002 * rng.normal()));
        }
        let f = usl.fit(Delta::DEFAULT).unwrap();
        let exact_peak = ((1.0 - sigma) / kappa).sqrt();
        assert!((f.lambda - lambda).abs() < 1.0, "{f:?}");
        assert!(
            (f.sigma - sigma).abs() < 0.005 && (f.kappa - kappa).abs() < 1e-4,
            "{f:?}"
        );
        assert!(
            (f.peak - exact_peak).abs() < 1.5 && f.peak_lower <= f.peak,
            "{f:?}"
        );
    }
}
