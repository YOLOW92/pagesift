//! 第③步怎样决定：把结构指纹的相似度换算成证据，算每个候选是目标的后验概率。
//!
//! **每个特征的证据。** 对特征 f 和候选 c，相似度 s_f(c)。目标以外的元素构成经验零分布：
//! q_f(c) = #{x : s_f(x) ≥ s_f(c)} / N（文档里有多大比例的元素在这一项上至少同样像，含 c 自己，
//! 所以 q ≥ 1/N）。“观察到这么像”的似然比上界是 P(·|目标) / P(·|非目标) ≤ 1 / q_f(c)。
//! 改版毁掉的特征（类名全被哈希化）对目标的相似度降到 0，q = 1，似然比为 1——自动变成中性，
//! 不需要知道改版改了什么。
//!
//! **合并。** 各特征当作独立证据：ln LR(c) = Σ_f −ln q_f(c)。在“c 是随机的非目标元素”之下每个 q_f
//! 近似均匀，2·Σ(−ln q) 服从自由度 2k 的 χ²（Fisher 合并法，k 是有证据的特征数），偶数自由度的
//! 尾概率有闭式 P = e^{−x} Σ_{i<k} x^i / i!。
//!
//! **接受。** 目标恰好在页面上出现一次、各候选先验相同，则
//! P(最高者是目标 | 观测) = LR_best / (LR_best + Σ LR_other)，其余候选不算最高者的祖先、后代
//! （列表模式下也不算同组的兄弟）。后验 ≥ 1 − δ（唯一的元参数 δ = 0.05）才接受。
//! “目标已被删掉”不单独建模：那时最高者多半只是背景里的一个，和其他没有证据的元素（LR ≈ 1）
//! 分享后验，过不了 1 − δ。
//!
//! 另外报告“最高者只是背景里最像的一个”的 p 值（N 个候选取最大，Bonferroni 校正 N·P），只作诊断：
//! 0.93 的评测里把它也当作接受条件时，留出集的取对率从 53.3% 降到 37.6%、取错率只从 18.7% 降到 16.9%
//! （`benchmarks/history/0.93/`）——Fisher 合并在离散的经验 q 上过于保守。
//!
//! 独立性假设会高估证据（特征之间有相关），评测里的取错率就是对这一点的检验。

use super::fingerprint::FEATURES;

/// 全局显著性水平 δ。
pub const DELTA: f64 = 0.05; // [δ] 全局风险水平

/// 每个候选的各特征相似度（`None` = 没有证据）。
pub type Scores = [Option<f64>; FEATURES];

/// 每个候选的 ln LR 和有证据的特征数。`pool` 是用来估计零分布的候选（可以多于 `scores`：
/// 列表项里的字段在各项之间合并估计）。
pub fn log_lr(scores: &[Scores], pool: &[Scores]) -> Vec<(f64, usize)> {
    let n = pool.len().max(1) as f64;
    // 每个特征的零分布（降序排好，用二分数出 ≥ s 的个数）
    let mut sorted: Vec<Vec<f64>> = (0..FEATURES)
        .map(|f| pool.iter().map(|s| s[f].unwrap_or(0.0)).collect())
        .collect();
    for v in &mut sorted {
        v.sort_by(|a, b| b.total_cmp(a));
    }
    scores
        .iter()
        .map(|s| {
            let mut sum = 0.0;
            let mut k = 0;
            for f in 0..FEATURES {
                let Some(x) = s[f] else { continue };
                k += 1;
                let at_least = sorted[f].partition_point(|v| *v >= x).max(1) as f64;
                sum += (n / at_least).ln();
            }
            (sum, k)
        })
        .collect()
}

/// 自由度 2k 的 χ² 在 2x 处的上尾概率：e^{−x} Σ_{i<k} x^i / i!。
pub fn chi2_even_tail(x: f64, k: usize) -> f64 {
    if x <= 0.0 {
        return 1.0;
    }
    let mut term = 1.0;
    let mut sum = 1.0;
    for i in 1..k {
        term *= x / i as f64;
        sum += term;
    }
    ((-x).exp() * sum).min(1.0)
}

/// 判决：`best` 的 ln LR、有证据的特征数，竞争者的 ln LR，候选总数 `n`（诊断用的 Bonferroni）。
/// 返回 `(接受, 最高者的后验, 背景检验的 p 值)`；只有后验决定接受与否。
pub fn decide(best: (f64, usize), others: &[f64], n: usize) -> (bool, f64, f64) {
    let p_background = (n.max(1) as f64 * chi2_even_tail(best.0, best.1)).min(1.0);
    // 后验：用 ln-sum-exp 防溢出
    let m = others.iter().copied().fold(best.0, f64::max);
    let z: f64 = (best.0 - m).exp() + others.iter().map(|o| (o - m).exp()).sum::<f64>();
    let posterior = (best.0 - m).exp() / z;
    (posterior >= 1.0 - DELTA, posterior, p_background)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chi2_tail_matches_known_values() {
        // k = 1：e^{−x}
        assert!((chi2_even_tail(1.0, 1) - (-1.0f64).exp()).abs() < 1e-12);
        // k = 2, x = 1：e^{−1}·2
        assert!((chi2_even_tail(1.0, 2) - 2.0 * (-1.0f64).exp()).abs() < 1e-12);
        assert_eq!(chi2_even_tail(0.0, 3), 1.0);
    }

    #[test]
    fn rare_matches_win_and_ties_do_not() {
        let bg: Scores = [Some(0.0), None, None, Some(0.0), Some(0.2), Some(0.1), None];
        let target: Scores = [Some(1.0), None, None, Some(1.0), Some(0.9), Some(0.8), None];
        let mut pool = vec![bg; 200];
        pool.push(target);
        let lr = log_lr(&pool, &pool);
        let best = lr[200];
        let others: Vec<f64> = lr[..200].iter().map(|x| x.0).collect();
        assert!(decide(best, &others, pool.len()).0);
        // 两个一模一样的最佳候选：后验各一半，拒绝
        pool.push(target);
        let lr = log_lr(&pool, &pool);
        let others: Vec<f64> = lr[..201].iter().map(|x| x.0).collect();
        assert!(!decide(lr[201], &others, pool.len()).0);
    }
}
