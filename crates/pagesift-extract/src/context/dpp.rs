//! Deterministic greedy Cholesky MAP inference for bounded L-ensemble Determinantal Point Processes.
//!
//! References:
//! - A. Kulesza & B. Taskar, "Learning Determinantal Point Processes", UAI 2011.
//! - L. Chen, G. Zhang & E. Zhou, "Fast Greedy MAP Inference for Determinantal Point Process
//!   to Improve Recommendation Diversity", NeurIPS 2018.
//!
//! Strictly bounded to `<= MAX_DPP_CANDIDATES = 32` candidates with guaranteed symmetric positive-definite
//! Gram construction, zero intermediate heap allocations, and deterministic tie-breaking for replayability.

// [策略] DPP 候选集硬上限 32，使 O(M^3) Cholesky 增量分解完全驻留栈内存（< 20 微秒，零堆碎片）
pub const MAX_DPP_CANDIDATES: usize = 32;
// [推导] 对角正则项 1e-6 确保任意近似共线或重复特征 Gram 矩阵严格正定（防止半正定奇异下溢）
const PSD_JITTER: f64 = 1e-6;
// [策略] 同一独立源根（origin_root）候选间的核相关系数下界权重 0.65，抑制同源转载重复入选
const SAME_ORIGIN_CORRELATION: f64 = 0.65;
// [推导] 质量上界 1e6 防止异常输入平方后在核矩阵中溢出为 Inf
const MAX_QUALITY_BOUND: f64 = 1e6;
// [推导] 质量正下界 1e-3 避免零行/零列退化
const MIN_QUALITY_BOUND: f64 = 1e-3;
// [推导] 机器精度级确定性平局决胜容差 1e-12，确保跨平台重放完全一致（2.0 Runtime 要求）
const TIE_TOL: f64 = 1e-12;

/// Candidate descriptor passed to the bounded L-ensemble DPP selector.
pub struct DppCandidate<'a> {
    pub quality: f64,
    pub cost_tokens: usize,
    pub sketch: &'a [f32],
    pub origin_root: &'a str,
}

/// Deterministically select candidate indices maximizing `log det(L_S)` subject to `sum cost <= budget`.
pub fn select_bounded_dpp(candidates: &[DppCandidate<'_>], budget_tokens: usize) -> Vec<usize> {
    let mut unit_state = ();
    select_bounded_dpp_with_costs(
        candidates,
        budget_tokens,
        &mut unit_state,
        |(), i| candidates[i].cost_tokens.max(1),
        |(), _| {},
    )
}

/// Internal generalization supporting dynamic union-marginal costs (e.g. shared table headers across bundles).
/// Returning `0` from `marginal_cost_of(state, i)` indicates all members of candidate `i` are already covered.
pub(crate) fn select_bounded_dpp_with_costs<S, C, A>(
    candidates: &[DppCandidate<'_>],
    budget_tokens: usize,
    state: &mut S,
    marginal_cost_of: C,
    on_accept: A,
) -> Vec<usize>
where
    C: Fn(&S, usize) -> usize,
    A: Fn(&mut S, usize),
{
    let m = candidates.len().min(MAX_DPP_CANDIDATES);
    if m == 0 || budget_tokens == 0 {
        return Vec::new();
    }

    // Precompute sanitized qualities and sub-unit L2 scaling factors so that for any signed sketch:
    //   tilde_s_i = clean(sketch_i) / max(1, ||clean(sketch_i)||_2)
    // has ||tilde_s_i||_2^2 <= 1, guaranteeing M = tilde_S tilde_S^T + diag(1 - ||tilde_s_i||_2^2) >= 0 (PSD)!
    let mut qualities = [MIN_QUALITY_BOUND; MAX_DPP_CANDIDATES];
    let mut scales = [1.0f64; MAX_DPP_CANDIDATES];
    for i in 0..m {
        let q = candidates[i].quality;
        qualities[i] = if q.is_finite() {
            q.clamp(MIN_QUALITY_BOUND, MAX_QUALITY_BOUND)
        } else {
            MIN_QUALITY_BOUND
        };
        let mut norm_sq = 0.0f64;
        for &v in candidates[i].sketch {
            if v.is_finite() {
                let vf = f64::from(v);
                norm_sq += vf * vf;
            }
        }
        if norm_sq > 1.0 && norm_sq.is_finite() {
            scales[i] = 1.0 / norm_sq.sqrt();
        } else if !norm_sq.is_finite() {
            scales[i] = 0.0;
        }
    }

    // Flat stack-allocated symmetric positive-definite kernel L (32 * 32 * 8B = 8 KiB)
    let mut kernel = [0.0f64; MAX_DPP_CANDIDATES * MAX_DPP_CANDIDATES];
    for i in 0..m {
        let qi = qualities[i];
        let si = scales[i];
        for j in i..m {
            let qj = qualities[j];
            let sim = if i == j {
                1.0
            } else {
                let sj = scales[j];
                let mut dot = 0.0f64;
                if si > 0.0 && sj > 0.0 {
                    let len = candidates[i].sketch.len().min(candidates[j].sketch.len());
                    for k in 0..len {
                        let a = candidates[i].sketch[k];
                        let b = candidates[j].sketch[k];
                        if a.is_finite() && b.is_finite() {
                            dot += (f64::from(a) * si) * (f64::from(b) * sj);
                        }
                    }
                }
                let same_root = if !candidates[i].origin_root.is_empty()
                    && candidates[i].origin_root == candidates[j].origin_root
                {
                    1.0
                } else {
                    0.0
                };
                (1.0 - SAME_ORIGIN_CORRELATION) * dot + SAME_ORIGIN_CORRELATION * same_root
            };
            let mut val = qi * sim * qj;
            if i == j {
                val += PSD_JITTER;
            }
            kernel[i * MAX_DPP_CANDIDATES + j] = val;
            kernel[j * MAX_DPP_CANDIDATES + i] = val;
        }
    }

    // Stack-allocated incremental Cholesky factors `c_mat[i * MAX_DPP_CANDIDATES + step]`
    let mut c_mat = [0.0f64; MAX_DPP_CANDIDATES * MAX_DPP_CANDIDATES];
    let mut di2 = [0.0f64; MAX_DPP_CANDIDATES];
    for i in 0..m {
        di2[i] = kernel[i * MAX_DPP_CANDIDATES + i];
    }
    let mut chosen = [false; MAX_DPP_CANDIDATES];
    let mut selected = Vec::with_capacity(m);
    let mut spent_tokens = 0usize;

    for step in 0..m {
        let mut best_idx: Option<usize> = None;
        let mut best_cost = usize::MAX;
        let mut best_score = f64::NEG_INFINITY;

        for i in 0..m {
            if chosen[i] {
                continue;
            }
            let cost = marginal_cost_of(state, i);
            if cost == 0 || spent_tokens.saturating_add(cost) > budget_tokens {
                continue;
            }
            let d2 = di2[i];
            if !(d2.is_finite() && d2 > PSD_JITTER * 0.5) {
                continue;
            }
            let gain = d2.ln_1p() / (cost as f64).sqrt();
            if !gain.is_finite() {
                continue;
            }
            let better = match best_idx {
                None => true,
                Some(prev) => {
                    if (gain - best_score).abs() > TIE_TOL {
                        gain > best_score
                    } else {
                        // Deterministic tie-breaker: higher quality -> lower marginal cost -> smaller index
                        (qualities[i], best_cost, prev) > (qualities[prev], cost, i)
                    }
                }
            };
            if better {
                best_score = gain;
                best_cost = cost;
                best_idx = Some(i);
            }
        }

        let Some(j_star) = best_idx else {
            break;
        };
        let d_star = di2[j_star].max(PSD_JITTER).sqrt();
        chosen[j_star] = true;
        spent_tokens += best_cost;
        on_accept(state, j_star);
        selected.push(j_star);

        let j_row = j_star * MAX_DPP_CANDIDATES;
        c_mat[j_row + step] = d_star;

        for i in 0..m {
            if chosen[i] {
                continue;
            }
            let i_row = i * MAX_DPP_CANDIDATES;
            let mut inner = 0.0f64;
            for t in 0..step {
                inner += c_mat[j_row + t] * c_mat[i_row + t];
            }
            let e_i = (kernel[j_row + i] - inner) / d_star;
            c_mat[i_row + step] = e_i;
            di2[i] = (di2[i] - e_i * e_i).max(0.0);
        }
    }

    selected
}
