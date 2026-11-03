//! 层级收缩：全局 → 站点 → 路径族，以及只在显著时更新的稳定性规则。
//!
//! 经验贝叶斯（H. Robbins, 1956；B. Efron, C. Morris, JASA 70(350), 1975）：把各组的比例看成来自
//! 共同的 `Beta(μm, (1−μ)m)`，用矩估计求先验：组间方差 `V` 减去平均抽样方差得 `τ²`，
//! 再由 Beta 分布的方差 `μ(1−μ)/(m+1) = τ²` 得 `m = μ(1−μ)/τ² − 1`。

use crate::Delta;
use crate::beta::{BetaCounts, Prior};

/// 由下层各组的计数估计上层先验。
///
/// - 先验均值 `μ` 取各组比例的**不加权**平均，单个组（站点）对它的影响是 `1/组数`，
///   一个站点刷大量样本也不能把全局拉过去（防投毒）。
/// - `τ² ≤ 0`（组间差异没有超出抽样噪声）时完全合并：强度取全部观测数之和，
///   先验不会比实际看到的数据更强。
/// - 少于两组、或没有任何观测时返回 `None`，调用方退回再上一层或无信息先验。
pub fn beta_prior(groups: &[BetaCounts]) -> Option<Prior> {
    let rates: Vec<(f64, f64)> = groups
        .iter()
        .filter(|g| g.trials() > 0.0)
        .map(|g| (g.rate(), g.trials()))
        .collect();
    if rates.len() < 2 {
        return None;
    }
    let k = rates.len() as f64;
    let mu = rates.iter().map(|(p, _)| p).sum::<f64>() / k;
    let total: f64 = rates.iter().map(|(_, n)| n).sum();
    if mu <= 0.0 || mu >= 1.0 {
        // 所有组都是 0 或都是 1：方差为 0，按完全合并处理
        return Some(Prior::from_mean(
            mu.clamp(f64::EPSILON, 1.0 - f64::EPSILON),
            total,
        ));
    }
    let between = rates.iter().map(|(p, _)| (p - mu).powi(2)).sum::<f64>() / (k - 1.0);
    let sampling = rates.iter().map(|(_, n)| mu * (1.0 - mu) / n).sum::<f64>() / k;
    let tau2 = between - sampling;
    let strength = if tau2 > 0.0 {
        (mu * (1.0 - mu) / tau2 - 1.0).clamp(0.0, total)
    } else {
        total
    };
    // 强度为 0 时先验退化，给它最小的有效形状：与无信息先验同强度
    Some(Prior::from_mean(
        mu,
        strength.max(Prior::UNIFORM.alpha + Prior::UNIFORM.beta),
    ))
}

/// 只在显著时更新：新估计与当前值之差超过 `z(1−δ)·SE` 才算变化。
pub fn significant(current: f64, estimate: f64, std_err: f64, delta: Delta) -> bool {
    (estimate - current).abs() > delta.z() * std_err
}

/// 精度加权步长（R. E. Kalman, J. Basic Engineering 82, 1960）：当前值方差 `p`、新观测方差 `r`，
/// 增益 `K = P/(P+R)`，更新 `x ← x + K(z − x)`、`P ← (1−K)P`。返回 `(x, P)`。
pub fn kalman_step(x: f64, p: f64, z: f64, r: f64) -> (f64, f64) {
    let k = p / (p + r);
    (x + k * (z - x), (1.0 - k) * p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    #[test]
    fn moment_prior_recovers_the_population_and_resists_one_big_group() {
        let (mu, m) = (0.3, 20.0);
        let mut rng = Rng::seed(4);
        let mut groups: Vec<BetaCounts> = (0..400)
            .map(|_| {
                let p = rng.beta(mu * m, (1.0 - mu) * m);
                let mut c = BetaCounts::default();
                (0..200).for_each(|_| c.observe(rng.uniform() < p));
                c
            })
            .collect();
        let prior = beta_prior(&groups).unwrap();
        let est_mu = prior.alpha / (prior.alpha + prior.beta);
        assert!((est_mu - mu).abs() < 0.01, "{prior:?}");
        assert!((prior.alpha + prior.beta - m).abs() < 4.0, "{prior:?}");
        // 一个组刷了一百万次成功，全局均值只移动约 (1 − μ)/组数
        let mut spam = BetaCounts::default();
        spam.observe_weighted(1e6, 0.0);
        groups.push(spam);
        let p2 = beta_prior(&groups).unwrap();
        let moved = p2.alpha / (p2.alpha + p2.beta) - est_mu;
        assert!(moved < 1.0 / 400.0 + 1e-3, "{moved}");
        // 组间无差异：完全合并
        let same: Vec<BetaCounts> = (0..10)
            .map(|_| BetaCounts {
                success: 50.0,
                failure: 50.0,
            })
            .collect();
        assert_eq!(
            beta_prior(&same).unwrap().alpha + beta_prior(&same).unwrap().beta,
            1000.0
        );
        assert!(!significant(1.0, 1.1, 0.1, Delta::DEFAULT));
        assert!(significant(1.0, 1.2, 0.1, Delta::DEFAULT));
        assert_eq!(kalman_step(0.0, 1.0, 2.0, 1.0), (1.0, 0.5));
    }
}
