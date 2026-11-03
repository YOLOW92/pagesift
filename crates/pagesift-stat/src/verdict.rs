//! 验收判定：Brier 分数、可靠性分箱、配对 bootstrap、Benjamini–Hochberg。

use crate::Delta;
use crate::beta::wilson;
use crate::rng::Rng;

/// Brier 分数（G. W. Brier, Monthly Weather Review 78(1), 1950）：`mean (p − y)²`，越低越好。
pub fn brier(probs: &[f64], ys: &[bool]) -> f64 {
    let n = probs.len() as f64;
    probs
        .iter()
        .zip(ys)
        .map(|(p, &y)| (p - f64::from(u8::from(y))).powi(2))
        .sum::<f64>()
        / n
}

/// Benjamini–Hochberg（JRSS B 57(1), 1995）：在错误发现率 `δ` 下哪些假设被拒绝。
pub fn benjamini_hochberg(p_values: &[f64], delta: Delta) -> Vec<bool> {
    let m = p_values.len();
    let mut idx: Vec<usize> = (0..m).collect();
    idx.sort_by(|&a, &b| p_values[a].total_cmp(&p_values[b]));
    let cutoff = idx
        .iter()
        .enumerate()
        .filter(|&(rank, &i)| p_values[i] <= (rank + 1) as f64 / m as f64 * delta.get())
        .map(|(rank, _)| rank + 1)
        .max()
        .unwrap_or(0);
    let mut out = vec![false; m];
    for &i in &idx[..cutoff] {
        out[i] = true;
    }
    out
}

/// 可靠性检验的一箱。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bin {
    pub mean_pred: f64,
    pub observed: f64,
    pub n: usize,
    /// 观测频率的 Wilson 区间（两端各 `δ`）。
    pub lo: f64,
    pub hi: f64,
}

/// 按预测概率的分位数分箱：箱数从 2 起加倍，直到每箱观测频率的 Wilson 区间宽度
/// 超过相邻箱平均预测之差（再分细就分辨不出来了），取上一个箱数。
pub fn reliability(probs: &[f64], ys: &[bool], delta: Delta) -> Vec<Bin> {
    let mut idx: Vec<usize> = (0..probs.len()).collect();
    idx.sort_by(|&a, &b| probs[a].total_cmp(&probs[b]));
    let make = |k: usize| -> Vec<Bin> {
        (0..k)
            .map(|b| {
                let part = &idx[b * idx.len() / k..(b + 1) * idx.len() / k];
                let n = part.len();
                let pos = part.iter().filter(|&&i| ys[i]).count() as f64;
                let (lo, hi) = wilson(pos, n as f64, delta);
                let mean_pred = part.iter().map(|&i| probs[i]).sum::<f64>() / n as f64;
                Bin {
                    mean_pred,
                    observed: pos / n as f64,
                    n,
                    lo,
                    hi,
                }
            })
            .collect()
    };
    let mut best = make(1);
    let mut k = 2;
    while k <= idx.len() / 2 {
        let bins = make(k);
        let width = bins.iter().map(|b| b.hi - b.lo).sum::<f64>() / k as f64;
        let gap = bins
            .windows(2)
            .map(|w| w[1].mean_pred - w[0].mean_pred)
            .sum::<f64>()
            / (k - 1) as f64;
        if width > gap {
            break;
        }
        best = bins;
        k *= 2;
    }
    best
}

/// 校准判定：每箱平均预测落在观测频率的 Wilson 区间内（多箱用 BH 控制），
/// 并且 Brier 分数显著低于常数基准率预测器（配对 bootstrap 的单侧上界 < 0）。
pub fn calibrated(
    probs: &[f64],
    ys: &[bool],
    delta: Delta,
    rng: &mut Rng,
) -> (bool, Vec<Bin>, Interval) {
    let bins = reliability(probs, ys, delta);
    // 每箱的双侧 p 值（正态近似），交给 BH
    let p_values: Vec<f64> = bins
        .iter()
        .map(|b| {
            let se = (b.mean_pred * (1.0 - b.mean_pred) / b.n as f64)
                .sqrt()
                .max(f64::MIN_POSITIVE);
            2.0 * crate::special::normal_cdf(-((b.observed - b.mean_pred).abs() / se))
        })
        .collect();
    let reliable = !benjamini_hochberg(&p_values, delta).iter().any(|&r| r);
    let base = ys.iter().filter(|&&y| y).count() as f64 / ys.len() as f64;
    let per_item: Vec<f64> = probs
        .iter()
        .zip(ys)
        .map(|(p, &y)| {
            let y = f64::from(u8::from(y));
            (p - y).powi(2) - (base - y).powi(2)
        })
        .collect();
    let diff = paired_bootstrap(&per_item, delta, rng);
    (reliable && diff.hi < 0.0, bins, diff)
}

/// 置信区间。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Interval {
    pub estimate: f64,
    pub lo: f64,
    pub hi: f64,
}

/// 配对差的均值的 bootstrap 百分位区间（Efron, Tibshirani, An Introduction to the Bootstrap,
/// 1993, 第 13 章），两端各 `δ`。重抽样次数从 `1/δ` 的量级起加倍，直到端点的蒙特卡洛标准误
/// 小于统计标准误的一个数量级。
pub fn paired_bootstrap(diffs: &[f64], delta: Delta, rng: &mut Rng) -> Interval {
    let n = diffs.len();
    let mean = diffs.iter().sum::<f64>() / n as f64;
    let se = (diffs.iter().map(|d| (d - mean).powi(2)).sum::<f64>()
        / (n as f64 * (n as f64 - 1.0)))
        .sqrt();
    let d = delta.get();
    let mut reps = ((1.0 / d).ceil() as usize).max(1) * 2;
    loop {
        let mut means: Vec<f64> = (0..reps)
            .map(|_| {
                (0..n)
                    .map(|_| diffs[(rng.next_u64() % n as u64) as usize])
                    .sum::<f64>()
                    / n as f64
            })
            .collect();
        means.sort_by(f64::total_cmp);
        let at = |q: f64| means[((q * (reps - 1) as f64).round() as usize).min(reps - 1)];
        // 分位点的蒙特卡洛标准误 ≈ √(q(1−q)/reps) / f(q)，正态近似下 f(q) = φ(z_q)/se
        let z = crate::special::normal_quantile(1.0 - d);
        let mc = (d * (1.0 - d) / reps as f64).sqrt() * se / crate::special::normal_pdf(z);
        // [推导] "小一个数量级"：蒙特卡洛误差不到统计误差的 1/10
        if mc < 0.1 * se || reps > n.saturating_mul(n).max(1 << 20) {
            return Interval {
                estimate: mean,
                lo: at(d),
                hi: at(1.0 - d),
            };
        }
        reps *= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calib::{Logistic, sigmoid};

    #[test]
    fn calibrated_model_passes_and_reversed_one_fails() {
        let mut rng = Rng::seed(17);
        // 真实概率 σ(2x − 1)：逻辑回归恢复系数，保序回归单调
        let xs: Vec<Vec<f64>> = (0..4000)
            .map(|_| vec![1.0, rng.uniform() * 4.0 - 2.0])
            .collect();
        let ys: Vec<bool> = xs
            .iter()
            .map(|x| rng.uniform() < sigmoid(2.0 * x[1] - 1.0))
            .collect();
        let lr = Logistic::fit(&xs, &ys).unwrap();
        assert!(
            (lr.w[0] + 1.0).abs() < 0.2 && (lr.w[1] - 2.0).abs() < 0.2,
            "{:?}",
            lr.w
        );
        let probs: Vec<f64> = xs.iter().map(|x| lr.predict(x)).collect();
        let (ok, bins, diff) = calibrated(&probs, &ys, Delta::DEFAULT, &mut rng);
        assert!(ok, "{bins:?} {diff:?}");
        assert!(bins.len() >= 4);
        // 打乱的预测不满足判定
        let shuffled: Vec<f64> = probs.iter().rev().copied().collect();
        assert!(!calibrated(&shuffled, &ys, Delta::DEFAULT, &mut rng).0);
        assert_eq!(
            benjamini_hochberg(&[0.001, 0.2, 0.01, 0.9], Delta::DEFAULT),
            vec![true, false, true, false]
        );
    }
}
