//! 受前提门控的知识覆盖度估计：Good–Turing / Chao–Jost 未见质量与 Chapman 捕获—再捕获。
//!
//! I. J. Good, Biometrika 40, 1953；A. Chao & L. Jost, Ecology 93(12), 2012；
//! D. G. Chapman, Univ. Calif. Publ. Stat. 1, 1951；P. Spoor et al., BMJ 313, 1996。
//!
//! 网页检索不天然满足独立同分布或等捕获概率假设；本模块在计算前显式检验闭合总体、最小样本量、
//! 单类退化、头部曝光偏置与双路相关性，条件不成立时返回 [`CoverageEstimate::NotEstimable`]，不输出伪精确百分比。

use crate::Delta;
use crate::beta::wilson;

/// 门控覆盖估计结果。
#[derive(Debug, Clone, PartialEq)]
pub enum CoverageEstimate {
    Estimable {
        point: f64,
        ci_low: f64,
        ci_high: f64,
    },
    NotEstimable {
        reason: &'static str,
    },
}

impl CoverageEstimate {
    pub fn point(&self) -> Option<f64> {
        match self {
            Self::Estimable { point, .. } => Some(*point),
            Self::NotEstimable { .. } => None,
        }
    }
}

/// Good–Turing / Chao–Jost 未见类别发现率估计（针对闭合受控总体内的类别频次计数 `counts`）。
///
/// - `closed_scope`: 是否来自边界清晰的受管数据源或固定评测集（开放全网必须传 `false`）。
/// - `counts`: 各已观测独立类别的出现频次（非零项有效）。
/// - `ranked_order_skew`: 可选的前后半程新类比值偏置指标（`> 0.88` 视为强排名曝光偏置）。
pub fn good_turing_unseen(
    closed_scope: bool,
    counts: &[u64],
    ranked_order_skew: Option<f64>,
    delta: Delta,
) -> CoverageEstimate {
    // [推导] Good–Turing 小样本相对方差约为 f1 / N^2，N >= 12 时单次类比例区间宽度才低于 0.5
    const MIN_TOTAL_SAMPLES: u128 = 12;
    // [策略] 单一头部类垄断超过 85% 观测且类别数 >= 3 时，严重违背近似齐次曝光假设
    const MAX_HEAD_DOMINANCE: f64 = 0.85;
    // [策略] 排名诱导的前半程新类占比超过 88% 时，顺序相关性破坏独立抽样假设
    const MAX_RANK_SKEW: f64 = 0.88;
    // [推导] 跨类覆盖估计至少需要观测到 2 个互异类别，单一类别垄断 100% 样本时缺乏跨类变异性
    const MIN_DISTINCT_CATEGORIES: usize = 2;
    // [协议] 多类偏置检验至少需要 3 个互异已观测类别才有头部垄断含义
    const MIN_DISTINCT_FOR_SKEW: usize = 3;

    if !closed_scope {
        return CoverageEstimate::NotEstimable {
            reason: "open_world_scope",
        };
    }
    let mut n_total = 0u128;
    let mut s_obs = 0usize;
    let mut f1 = 0u64;
    let mut f2 = 0u64;
    let mut max_c = 0u64;
    for &c in counts {
        if c == 0 {
            continue;
        }
        n_total += u128::from(c);
        s_obs += 1;
        if c == 1 {
            f1 += 1;
        } else if c == 2 {
            f2 += 1;
        }
        if c > max_c {
            max_c = c;
        }
    }
    if n_total < MIN_TOTAL_SAMPLES {
        return CoverageEstimate::NotEstimable {
            reason: "insufficient_samples",
        };
    }
    if s_obs < MIN_DISTINCT_CATEGORIES {
        return CoverageEstimate::NotEstimable {
            reason: "single_category_degenerate",
        };
    }
    let n_f = n_total as f64;
    if !n_f.is_finite() {
        return CoverageEstimate::NotEstimable {
            reason: "numerical_overflow",
        };
    }
    if s_obs >= MIN_DISTINCT_FOR_SKEW && (max_c as f64 / n_f) > MAX_HEAD_DOMINANCE {
        return CoverageEstimate::NotEstimable {
            reason: "severe_exposure_skew",
        };
    }
    if let Some(skew) = ranked_order_skew {
        if !skew.is_finite() || skew < 0.0 {
            return CoverageEstimate::NotEstimable {
                reason: "invalid_rank_skew",
            };
        }
        if skew > MAX_RANK_SKEW {
            return CoverageEstimate::NotEstimable {
                reason: "ranked_exposure_bias",
            };
        }
    }

    // Chao & Jost (2012) Eq. 4a: 样本覆盖赤字估计（同除 (N-1)*f1 防止大整数乘积浮点上溢）
    let f1_f = f1 as f64;
    let f2_f = f2 as f64;
    let f1_ratio = f1_f / n_f;
    let point = if f1 == 0 {
        0.0
    } else if f2 > 0 {
        let denom_adj = (2.0 * f2_f) / ((n_f - 1.0) * f1_f);
        f1_ratio / (1.0 + denom_adj)
    } else {
        f1_ratio * ((n_f - 1.0) / n_f)
    };
    if !point.is_finite() {
        return CoverageEstimate::NotEstimable {
            reason: "numerical_overflow",
        };
    }
    let (low, high) = wilson(f1_f, n_f, delta);
    CoverageEstimate::Estimable {
        point: point.clamp(0.0, 1.0),
        ci_low: low.min(point).clamp(0.0, 1.0),
        ci_high: high.max(point).clamp(0.0, 1.0),
    }
}

/// Chapman (1951) 捕获—再捕获总体规模估计（仅限闭合语料且两路检索器相关性受控时启用）。
pub fn chapman_capture_recapture(
    closed_scope: bool,
    n_a: u64,
    n_b: u64,
    overlap_m: u64,
    retriever_correlation: f64,
    delta: Delta,
) -> CoverageEstimate {
    // [策略] 两路检索特征重叠相关性 > 0.70 时正相关偏差显著低估真实总体规模（Spoor et al. 1996）
    const MAX_RETRIEVER_CORRELATION: f64 = 0.70;
    // [协议] 相关系数合法下界 -1.0（柯西—施瓦茨不等式边界）
    const MIN_RETRIEVER_CORRELATION: f64 = -1.0;

    if !closed_scope {
        return CoverageEstimate::NotEstimable {
            reason: "open_world_scope",
        };
    }
    if n_a == 0 || n_b == 0 {
        return CoverageEstimate::NotEstimable {
            reason: "empty_capture_arm",
        };
    }
    if overlap_m == 0 {
        return CoverageEstimate::NotEstimable {
            reason: "zero_overlap",
        };
    }
    if overlap_m > n_a.min(n_b) {
        return CoverageEstimate::NotEstimable {
            reason: "invalid_overlap_exceeds_arm",
        };
    }
    if !retriever_correlation.is_finite()
        || !(MIN_RETRIEVER_CORRELATION..=1.0).contains(&retriever_correlation)
    {
        return CoverageEstimate::NotEstimable {
            reason: "invalid_retriever_correlation",
        };
    }
    if retriever_correlation > MAX_RETRIEVER_CORRELATION {
        return CoverageEstimate::NotEstimable {
            reason: "correlated_retrievers",
        };
    }

    // 使用 u128 提升消除 n_a = u64::MAX / overlap_m = u64::MAX - 1 / n_a + n_b 的 u64 加法溢出，
    // 并将乘积拆为先除后乘形式避免中间四次方浮点上溢。
    let a1 = (u128::from(n_a) + 1) as f64;
    let b1 = (u128::from(n_b) + 1) as f64;
    let m1 = (u128::from(overlap_m) + 1) as f64;
    let m2 = (u128::from(overlap_m) + 2) as f64;
    let rem_a = (n_a - overlap_m) as f64;
    let rem_b = (n_b - overlap_m) as f64;
    let observed_union = (u128::from(n_a - overlap_m) + u128::from(n_b)) as f64;
    let n_hat = ((a1 / m1) * b1 - 1.0).max(observed_union);

    // Seber (1982) / Wittes (1972) Chapman 方差近似（配对相除防中间上溢）
    let var = (a1 / m1) * (b1 / m1) * (rem_a / m2) * rem_b;
    if !n_hat.is_finite() || !var.is_finite() {
        return CoverageEstimate::NotEstimable {
            reason: "numerical_overflow",
        };
    }
    let se = var.max(0.0).sqrt();
    let z = delta.z();
    let ci_high = n_hat + z * se;
    if !ci_high.is_finite() {
        return CoverageEstimate::NotEstimable {
            reason: "numerical_overflow",
        };
    }
    CoverageEstimate::Estimable {
        point: n_hat,
        ci_low: (n_hat - z * se).max(observed_union),
        ci_high,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn good_turing_gates_open_world_small_samples_single_class_and_nan_skew() {
        assert_eq!(
            good_turing_unseen(false, &[4, 3, 2, 1, 1, 1], None, Delta::DEFAULT),
            CoverageEstimate::NotEstimable {
                reason: "open_world_scope"
            }
        );
        assert_eq!(
            good_turing_unseen(true, &[], None, Delta::DEFAULT),
            CoverageEstimate::NotEstimable {
                reason: "insufficient_samples"
            }
        );
        assert_eq!(
            good_turing_unseen(true, &[2, 1, 1], None, Delta::DEFAULT),
            CoverageEstimate::NotEstimable {
                reason: "insufficient_samples"
            }
        );
        // Single category monopolizing all 20 hits must NOT claim 0% unseen!
        assert_eq!(
            good_turing_unseen(true, &[20], None, Delta::DEFAULT),
            CoverageEstimate::NotEstimable {
                reason: "single_category_degenerate"
            }
        );
        // Severe head dominance (20 out of 22 on one item)
        assert_eq!(
            good_turing_unseen(true, &[20, 1, 1], None, Delta::DEFAULT),
            CoverageEstimate::NotEstimable {
                reason: "severe_exposure_skew"
            }
        );
        // NaN skew must not bypass ranked_order_skew gate
        assert_eq!(
            good_turing_unseen(true, &[4, 4, 4, 1, 1], Some(f64::NAN), Delta::DEFAULT),
            CoverageEstimate::NotEstimable {
                reason: "invalid_rank_skew"
            }
        );
    }

    #[test]
    fn good_turing_and_chapman_estimate_valid_closed_corpus_and_guard_u64_max() {
        let gt = good_turing_unseen(true, &[3, 3, 2, 2, 1, 1, 1, 1], None, Delta::DEFAULT);
        match gt {
            CoverageEstimate::Estimable {
                point,
                ci_low,
                ci_high,
            } => {
                assert!(point > 0.10 && point < 0.40, "point={point}");
                assert!(ci_low <= point && point <= ci_high);
            }
            other => panic!("expected estimable, got {other:?}"),
        }

        // True N = 100, arm A captures 40, arm B captures 50, expected independent overlap = 20
        let cr = chapman_capture_recapture(true, 40, 50, 20, 0.25, Delta::DEFAULT);
        match cr {
            CoverageEstimate::Estimable {
                point,
                ci_low,
                ci_high,
            } => {
                assert!((point - 98.6).abs() < 2.0, "point={point}");
                assert!(ci_low <= 100.0 && 100.0 <= ci_high, "{ci_low}..{ci_high}");
            }
            other => panic!("expected estimable, got {other:?}"),
        }

        assert_eq!(
            chapman_capture_recapture(true, 40, 50, 20, 0.85, Delta::DEFAULT),
            CoverageEstimate::NotEstimable {
                reason: "correlated_retrievers"
            }
        );
        // NaN correlation must be rejected, not silently accepted!
        assert_eq!(
            chapman_capture_recapture(true, 40, 50, 20, f64::NAN, Delta::DEFAULT),
            CoverageEstimate::NotEstimable {
                reason: "invalid_retriever_correlation"
            }
        );
        assert_eq!(
            chapman_capture_recapture(true, 0, 50, 0, 0.1, Delta::DEFAULT),
            CoverageEstimate::NotEstimable {
                reason: "empty_capture_arm"
            }
        );
        assert_eq!(
            chapman_capture_recapture(true, 40, 50, 0, 0.1, Delta::DEFAULT),
            CoverageEstimate::NotEstimable {
                reason: "zero_overlap"
            }
        );

        // Large u64::MAX / near-MAX inputs must never panic or wrap around!
        let huge = chapman_capture_recapture(
            true,
            u64::MAX,
            u64::MAX - 1,
            u64::MAX - 2,
            0.10,
            Delta::DEFAULT,
        );
        match huge {
            CoverageEstimate::Estimable {
                point,
                ci_low,
                ci_high,
            } => {
                assert!(point.is_finite() && point >= (u64::MAX - 1) as f64);
                assert!(ci_low <= point && point <= ci_high);
            }
            other => panic!("expected finite estimable on u64::MAX, got {other:?}"),
        }
    }
}
