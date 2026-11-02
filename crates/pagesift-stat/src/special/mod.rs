//! 特殊函数与分布函数：对数伽马、不完全伽马与贝塔、正态和 t 分布的分布函数与分位数。
//!
//! 公式出处是 NIST DLMF（Digital Library of Mathematical Functions）的章节号，写在各函数上。
//! 精度目标是 f64 的相对误差 1e-13 量级；这些函数用在决策的门槛上，不在每页的热路径里。

mod beta_t;
mod gamma;
mod normal;

pub use beta_t::{beta_inc, beta_quantile, t_cdf, t_quantile};
pub use gamma::{erfc, gamma_p, gamma_q, ln_beta, ln_gamma};
pub use normal::{normal_cdf, normal_pdf, normal_quantile};

/// 连分式和级数的收敛判据：相邻两项的相对变化小于机器精度。
pub(crate) const EPS: f64 = f64::EPSILON;

/// 修正 Lentz 算法里替代 0 的极小数（DLMF 3.10.1 后的说明）。
pub(crate) const TINY: f64 = f64::MIN_POSITIVE;

/// [推导] 级数和连分式的迭代上限：参数在本 crate 的用法范围（a、b ≤ 1e7）内几百步收敛，
/// 上限只防止 NaN 输入死循环。
pub(crate) const MAX_ITER: usize = 10_000;

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn close(a: f64, b: f64, rel: f64) -> bool {
        (a - b).abs() <= rel * b.abs().max(1e-300)
    }

    #[test]
    fn reference_values() {
        // Γ(½) = √π，Γ(10) = 9!
        assert!(close(ln_gamma(0.5), 0.5 * PI.ln(), 1e-14));
        assert!(close(ln_gamma(10.0), 362_880f64.ln(), 1e-14));
        assert!(close(ln_gamma(1e-3), 6.907_178_885_383_854, 1e-12));
        // 正态
        assert!(close(normal_quantile(0.975), 1.959_963_984_540_054, 1e-13));
        assert!(close(normal_quantile(1e-10), -6.361_340_902_404_056, 1e-12));
        assert!(close(normal_cdf(-3.0), 1.349_898_031_630_094_6e-3, 1e-12));
        for p in [1e-6, 0.01, 0.3, 0.5, 0.77, 0.999] {
            assert!(close(normal_cdf(normal_quantile(p)), p, 1e-12), "{p}");
        }
        // I_0.5(2,3) = 11/16；Beta(2,3) 中位数满足 I = ½
        assert!(close(beta_inc(2.0, 3.0, 0.5), 0.6875, 1e-13));
        let m = beta_quantile(2.0, 3.0, 0.5);
        assert!(close(beta_inc(2.0, 3.0, m), 0.5, 1e-12));
        // t(10) 的 0.975 分位数；ν 很大时趋于正态
        assert!(close(
            t_quantile(0.975, 10.0),
            2.228_138_851_964_938_5,
            1e-9
        ));
        assert!(close(t_quantile(0.975, 1e7), 1.959_963_984_540_054, 1e-6));
        assert!(close(gamma_p(3.0, 2.0), 0.323_323_583_816_936_5, 1e-12));
    }
}
