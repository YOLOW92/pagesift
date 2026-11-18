//! Bounded log-domain Entropic Unbalanced Optimal Transport (UOT) on sparse candidate subgraphs.
//!
//! Reference: L. Chizat, G. Peyré, B. Schmitzer & F.-X. Vialard,
//! "Scaling Algorithms for Unbalanced Optimal Transport Problems", Math. Comp. 87, 2018.
//!
//! Solves:
//!   min_{T >= 0} <C, T> + eps * KL(T || 1 1^T) + tau_1 * KL(T 1 || a) + tau_2 * KL(T^T 1 || b)
//! strictly on pruned connected subgraphs (`n, m <= MAX_UOT_NODES = 64`), avoiding full O(N*M)
//! dense matrices and operating in log-domain to prevent floating-point overflow/underflow.

// [策略] 局部模糊匹配子图单侧最大节点数 64，超出时上游按章节/连通分量分治，保证峰值缓冲 < 32 KiB
pub const MAX_UOT_NODES: usize = 64;
// [推导] 熵正则温度 eps = 0.08：使代价差 >= 0.25 的非最优路径权重衰减 exp(-3.1) < 0.05，同时保持强凸性
const DEFAULT_EPS: f64 = 0.08;
// [推导] 边际 KL 松弛惩罚 tau = 0.55：当代价 C_ij > 2 * tau = 1.10 时宁可创建/销毁质量也不强行错配
const DEFAULT_TAU: f64 = 0.55;
// [策略] 对数域 Sinkhorn 非平衡缩放迭代上限 32 步（实测在 eps=0.08 下 10~18 步内达到 1e-4 收敛）
const MAX_SINKHORN_ITERS: usize = 32;
// [推导] 对偶潜变量无穷范数收敛容差 1e-4
const DUAL_TOL: f64 = 1e-4;
// [推导] 无边不可达项等效无穷大代价哨兵
const INF_COST: f64 = 1e9;
// [推导] 对数耦合上界 30.0（防止异常大权重在对数域恢复 exp 时溢出为 +inf）
const MAX_LOG_MASS: f64 = 30.0;
// [推导] exp(-30) ≈ 9.3e-14 远低于最小保留质量，跳过超越函数调用
const MIN_LOG_MASS: f64 = -30.0;
// [策略] 过滤 < 1e-3 的可忽略数值尾项，保持输出传输图稀疏清晰
const MIN_PRIMAL_MASS: f64 = 1e-3;

/// Sparse finite-cost edge `(old_local_idx, new_local_idx, cost)` in `[0.0, 2.0]`.
#[derive(Debug, Clone, Copy)]
pub struct SparseCostEdge {
    pub old_idx: usize,
    pub new_idx: usize,
    pub cost: f64,
}

/// Non-zero soft transport entry `(old_local_idx, new_local_idx, mass)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransportLink {
    pub old_idx: usize,
    pub new_idx: usize,
    pub mass: f64,
}

/// Solve entropic UOT on a bounded sparse bipartite graph (`weights_a.len(), weights_b.len() <= 64`).
pub fn solve_bounded_uot(
    weights_a: &[f64],
    weights_b: &[f64],
    edges: &[SparseCostEdge],
) -> Vec<TransportLink> {
    let n = weights_a.len().min(MAX_UOT_NODES);
    let m = weights_b.len().min(MAX_UOT_NODES);
    if n == 0 || m == 0 || edges.is_empty() {
        return Vec::new();
    }

    let eps = DEFAULT_EPS;
    let tau = DEFAULT_TAU;
    let rho = tau / (tau + eps);

    let mut log_a = [f64::NEG_INFINITY; MAX_UOT_NODES];
    let mut valid_a = [false; MAX_UOT_NODES];
    for i in 0..n {
        let w = weights_a[i];
        if w.is_finite() && w > 0.0 {
            valid_a[i] = true;
            // [推导] 1e-9 正下界保护极小正权重取对数数值稳定
            log_a[i] = w.max(1e-9).ln();
        }
    }

    let mut log_b = [f64::NEG_INFINITY; MAX_UOT_NODES];
    let mut valid_b = [false; MAX_UOT_NODES];
    for j in 0..m {
        let w = weights_b[j];
        if w.is_finite() && w > 0.0 {
            valid_b[j] = true;
            // [推导] 1e-9 正下界保护极小正权重取对数数值稳定
            log_b[j] = w.max(1e-9).ln();
        }
    }

    // Build deduplicated adjacency lists over valid bounded indices and finite costs < INF_COST
    let mut row_adj: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    let mut col_adj: Vec<Vec<(usize, f64)>> = vec![Vec::new(); m];
    for e in edges {
        if e.old_idx < n
            && e.new_idx < m
            && valid_a[e.old_idx]
            && valid_b[e.new_idx]
            && e.cost.is_finite()
            && e.cost < INF_COST
        {
            let clean_cost = e.cost.max(0.0);
            if let Some(existing) = row_adj[e.old_idx]
                .iter_mut()
                .find(|(nj, _)| *nj == e.new_idx)
            {
                if clean_cost < existing.1 {
                    existing.1 = clean_cost;
                    if let Some(col_ex) = col_adj[e.new_idx]
                        .iter_mut()
                        .find(|(oi, _)| *oi == e.old_idx)
                    {
                        col_ex.1 = clean_cost;
                    }
                }
            } else {
                row_adj[e.old_idx].push((e.new_idx, clean_cost));
                col_adj[e.new_idx].push((e.old_idx, clean_cost));
            }
        }
    }

    if row_adj.iter().all(Vec::is_empty) {
        return Vec::new();
    }

    // Dual variables f_i / eps (`u`) and g_j / eps (`v`) in log-domain
    let mut u = [0.0f64; MAX_UOT_NODES];
    let mut v = [0.0f64; MAX_UOT_NODES];
    for i in 0..n {
        if row_adj[i].is_empty() {
            u[i] = -INF_COST;
        }
    }
    for j in 0..m {
        if col_adj[j].is_empty() {
            v[j] = -INF_COST;
        }
    }
    let mut scratch = [0.0f64; MAX_UOT_NODES];

    for _ in 0..MAX_SINKHORN_ITERS {
        let mut max_delta = 0.0f64;

        for i in 0..n {
            let deg = row_adj[i].len();
            if deg == 0 {
                continue;
            }
            for (k, &(j, c_ij)) in row_adj[i].iter().enumerate() {
                scratch[k] = v[j] - c_ij / eps;
            }
            let lse = log_sum_exp(&scratch[..deg]);
            if lse.is_finite() {
                let next_u = rho * (log_a[i] - lse);
                if next_u.is_finite() {
                    max_delta = max_delta.max((next_u - u[i]).abs());
                    u[i] = next_u;
                }
            }
        }

        for j in 0..m {
            let deg = col_adj[j].len();
            if deg == 0 {
                continue;
            }
            for (k, &(i, c_ij)) in col_adj[j].iter().enumerate() {
                scratch[k] = u[i] - c_ij / eps;
            }
            let lse = log_sum_exp(&scratch[..deg]);
            if lse.is_finite() {
                let next_v = rho * (log_b[j] - lse);
                if next_v.is_finite() {
                    max_delta = max_delta.max((next_v - v[j]).abs());
                    v[j] = next_v;
                }
            }
        }

        if max_delta < DUAL_TOL {
            break;
        }
    }

    // Recover sparse primal coupling T_ij = exp(u_i + v_j - C_ij / eps) over valid deduplicated edges
    let mut out = Vec::with_capacity(edges.len());
    for (i, adj) in row_adj.iter().enumerate() {
        for &(j, c_ij) in adj {
            let log_t = u[i] + v[j] - c_ij / eps;
            if log_t.is_finite() && log_t > MIN_LOG_MASS {
                let mass = log_t.min(MAX_LOG_MASS).exp();
                if mass.is_finite() && mass >= MIN_PRIMAL_MASS {
                    out.push(TransportLink {
                        old_idx: i,
                        new_idx: j,
                        mass,
                    });
                }
            }
        }
    }
    out
}

fn log_sum_exp(vals: &[f64]) -> f64 {
    let mut max_v = f64::NEG_INFINITY;
    for &x in vals {
        if x.is_finite() && x > max_v {
            max_v = x;
        }
    }
    if !max_v.is_finite() {
        return f64::NEG_INFINITY;
    }
    let mut sum = 0.0f64;
    for &x in vals {
        if x.is_finite() {
            sum += (x - max_v).exp();
        }
    }
    if sum > 0.0 && sum.is_finite() {
        max_v + sum.ln()
    } else {
        f64::NEG_INFINITY
    }
}
