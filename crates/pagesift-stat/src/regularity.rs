//! 非混叠周期性检测与预算受限拉格朗日注水调度计算。
//!
//! 保持零第三方依赖，供 `pagesift-plan`、`pagesift-store` 与 `pagesift-engine` 共享唯一数学真值。

/// 已验证的规律更新周期估计。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeriodEstimate {
    pub mean_secs: f64,
    pub std_secs: f64,
    pub cv: f64,
    pub sample_count: usize,
}

/// 从已确认非混叠的变化间隔样本中估计规律周期。
///
/// 要求至少 3 个正间隔样本，且变异系数 `cv = std / mean <= max_cv`。
pub fn estimate_period(intervals_secs: &[f64], max_cv: f64) -> Option<PeriodEstimate> {
    let valid: Vec<f64> = intervals_secs
        .iter()
        .copied()
        .filter(|v| v.is_finite() && *v >= 1.0)
        .collect();
    let n = valid.len();
    if n < 3 {
        return None;
    }
    let mean = valid.iter().sum::<f64>() / n as f64;
    if !(mean.is_finite() && mean >= 1.0) {
        return None;
    }
    let var = valid
        .iter()
        .map(|v| {
            let d = v - mean;
            d * d
        })
        .sum::<f64>()
        / (n - 1) as f64;
    let std = var.max(0.0).sqrt();
    let cv = std / mean;
    if cv <= max_cv && cv.is_finite() {
        Some(PeriodEstimate {
            mean_secs: mean,
            std_secs: std,
            cv,
            sample_count: n,
        })
    } else {
        None
    }
}

/// 判定相邻两次已观察变化之间的时间差是否满足非混叠条件。
///
/// - 冷启动（尚无既有稳态周期估计）时，必须存在位于 `prev_change_ms` 之后、
///   `effective_change_ts` 之前的“未变更确认”检查点，或由服务器精确更新时间窄化盲区，
///   确保不会把粗轮询间隔误判成站点真实周期（抗混叠约束）。
/// - 已有稳定周期估计时，匹配既有周期窗口的命中可直接延续跟踪；若出现明显早于预测的变更，
///   则调用方需触发早变更/混叠退化检测。
pub fn non_aliased_change_interval_secs(
    prev_change_ms: u64,
    last_confirmed_unchanged_ms: Option<u64>,
    observed_at_ms: u64,
    last_modified_at_ms: Option<u64>,
    existing_estimate: Option<PeriodEstimate>,
) -> Option<f64> {
    let effective_ts = last_modified_at_ms
        .filter(|&m| m > prev_change_ms && m <= observed_at_ms.saturating_add(60_000))
        .unwrap_or(observed_at_ms);
    if effective_ts <= prev_change_ms {
        return None;
    }
    let dt_secs = (effective_ts - prev_change_ms) as f64 / 1000.0;
    if !dt_secs.is_finite() || dt_secs < 1.0 {
        return None;
    }

    // 检查盲区宽度：仅当在本次变更前存在最近一次已确认未变更点，且盲区占比不大时才可确信仅发生过一次变更。
    if let Some(unchanged_ms) = last_confirmed_unchanged_ms.filter(|&u| u > prev_change_ms)
        && unchanged_ms < effective_ts
    {
        let blind_secs = (effective_ts - unchanged_ms) as f64 / 1000.0;
        if blind_secs <= dt_secs * 0.42 + 15.0 {
            return Some(dt_secs);
        }
    }

    // 已有已验证周期时，仅当命中在周期预测邻域内且盲区未远超周期时，允许继续延续周期样本。
    if let Some(est) = existing_estimate {
        let tol = (3.5 * est.std_secs).max(est.mean_secs * 0.25);
        if (dt_secs - est.mean_secs).abs() <= tol {
            return Some(dt_secs);
        }
    }

    None
}

/// 根据周期估计计算下一次建议检查距“现在”的安全等待秒数。
///
/// 目标落在预计下一次变更时刻 `last_change + mean + guard` 之后，
/// 夹紧于 `[min_secs, max_secs]`。
pub fn next_periodic_delay_secs(
    est: &PeriodEstimate,
    elapsed_since_change_secs: f64,
    min_secs: f64,
    max_secs: f64,
) -> f64 {
    let guard = (2.0 * est.std_secs).clamp(5.0, (est.mean_secs * 0.15).max(5.0));
    let elapsed = elapsed_since_change_secs.max(0.0);
    let target_from_change = est.mean_secs + guard;
    let remaining = if elapsed < target_from_change {
        target_from_change - elapsed
    } else {
        // 已越过预测变更窗但尚未捕捉到变化，缩短步长按正常间隔或半周期补采。
        min_secs.max((est.mean_secs * 0.35).min(max_secs))
    };
    remaining.clamp(min_secs, max_secs.max(min_secs))
}

/// Gamma-Poisson 层次收缩速率：将族先验 `(family_rate_per_sec, prior_weight_secs)` 与单资源观测合并。
pub fn hierarchical_change_rate(
    family_rate_per_sec: f64,
    prior_weight_secs: f64,
    resource_changes: u32,
    resource_exposure_secs: f64,
) -> f64 {
    let fam_rate = if family_rate_per_sec.is_finite() && family_rate_per_sec >= 0.0 {
        family_rate_per_sec
    } else {
        0.0
    };
    let w0 = prior_weight_secs.max(0.0);
    let exp = resource_exposure_secs.max(0.0);
    let denom = (w0 + exp).max(1.0);
    ((w0 * fam_rate + resource_changes as f64) / denom).max(0.0)
}

/// 预算受限拉格朗日注水调度的单项输入。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaterfillItem {
    pub weight: f64,
    pub rate_per_sec: f64,
    pub cost_ms: f64,
    pub min_interval_secs: f64,
    pub max_interval_secs: f64,
}

impl WaterfillItem {
    fn interval_for_eta(self, eta: f64) -> f64 {
        let min_i = self.min_interval_secs.max(1.0);
        let max_i = self.max_interval_secs.max(min_i);
        let denom_inner =
            (self.weight.max(1e-6) * self.rate_per_sec.max(1e-12)) / self.cost_ms.max(1.0);
        let kappa = denom_inner.sqrt();
        let raw = if eta * kappa <= 1e-18 {
            max_i
        } else {
            1.0 / (eta * kappa)
        };
        raw.clamp(min_i, max_i)
    }
}

/// 在稳态单位时间成本预算 `budget_cost_ms_per_sec` 约束下，按拉格朗日最优公式
/// `T_j* = clamp(1 / (eta * sqrt(w_j * lambda_j / c_j)), min_j, max_j)` 分配目标检查间隔。
pub fn lagrange_waterfill_intervals(
    items: &[WaterfillItem],
    budget_cost_ms_per_sec: f64,
) -> Vec<f64> {
    if items.is_empty() {
        return Vec::new();
    }
    if !(budget_cost_ms_per_sec.is_finite() && budget_cost_ms_per_sec > 0.0) {
        return items
            .iter()
            .map(|it| it.max_interval_secs.max(it.min_interval_secs.max(1.0)))
            .collect();
    }

    let min_burn: f64 = items
        .iter()
        .map(|it| it.cost_ms.max(1.0) / it.max_interval_secs.max(it.min_interval_secs.max(1.0)))
        .sum();
    if budget_cost_ms_per_sec <= min_burn {
        return items
            .iter()
            .map(|it| it.max_interval_secs.max(it.min_interval_secs.max(1.0)))
            .collect();
    }

    let max_burn: f64 = items
        .iter()
        .map(|it| it.cost_ms.max(1.0) / it.min_interval_secs.max(1.0))
        .sum();
    if budget_cost_ms_per_sec >= max_burn {
        return items
            .iter()
            .map(|it| it.min_interval_secs.max(1.0))
            .collect();
    }

    let mut lo = 1e-9_f64;
    let mut hi = 1e9_f64;
    for _ in 0..56 {
        let mid = (lo * hi).sqrt();
        let burn: f64 = items
            .iter()
            .map(|it| it.cost_ms.max(1.0) / it.interval_for_eta(mid))
            .sum();
        if burn > budget_cost_ms_per_sec {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let eta = (lo * hi).sqrt();
    items.iter().map(|it| it.interval_for_eta(eta)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_aliased_rejects_blind_polling_without_unchanged_confirm() {
        assert!(non_aliased_change_interval_secs(0, None, 120_000, None, None).is_none());
        let dt = non_aliased_change_interval_secs(0, Some(95_000), 125_000, None, None);
        assert!(dt.is_some());
        assert!((dt.unwrap() - 125.0).abs() < 0.01);
    }

    #[test]
    fn estimate_period_accepts_consistent_cycles_and_rejects_bursts() {
        let periodic = estimate_period(&[3600.0, 3540.0, 3660.0], 0.22).expect("regular");
        assert!(periodic.cv < 0.05);
        assert!(estimate_period(&[60.0, 3600.0, 300.0], 0.22).is_none());
    }

    #[test]
    fn waterfill_allocates_shorter_interval_for_hotter_and_cheaper_items() {
        let items = vec![
            WaterfillItem {
                weight: 2.0,
                rate_per_sec: 1.0 / 600.0,
                cost_ms: 100.0,
                min_interval_secs: 60.0,
                max_interval_secs: 86_400.0,
            },
            WaterfillItem {
                weight: 1.0,
                rate_per_sec: 1.0 / 36_000.0,
                cost_ms: 1500.0,
                min_interval_secs: 60.0,
                max_interval_secs: 86_400.0,
            },
        ];
        let out = lagrange_waterfill_intervals(&items, 1.0);
        assert!(out[0] < out[1], "got {out:?}");
    }
}
