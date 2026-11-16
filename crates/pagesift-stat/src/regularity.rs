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
/// - `last_modified_at_ms` 若经调用方校验可信且位于 `(prev_change_ms, observed_at_ms]`，
///   仅能将“最后一次修改时间”精确到 `lm`（剥离 `[lm, observed_at_ms]` 的轮询延迟并窄化盲区），
///   但**不能**单独证明 `prev_change_ms..lm` 之间未发生多次中间变更。
/// - 冷启动（尚无既有稳态周期估计）时，必须存在位于 `prev_change_ms` 之后、
///   `effective_change_ts` 之前的“未变更确认”检查点且盲区比例受控，防止把粗轮询误判为真实周期。
/// - 已有稳定周期估计时，匹配既有周期窗口的命中可直接延续跟踪。
pub fn non_aliased_change_interval_secs(
    prev_change_ms: u64,
    last_confirmed_unchanged_ms: Option<u64>,
    observed_at_ms: u64,
    last_modified_at_ms: Option<u64>,
    existing_estimate: Option<PeriodEstimate>,
) -> Option<f64> {
    let effective_ts = last_modified_at_ms
        .filter(|&m| m > prev_change_ms && m <= observed_at_ms.saturating_add(5_000))
        .map(|m| m.min(observed_at_ms))
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

/// 统一推进单时间序列的周期窗口状态，并在出现早变更、明显延后变更、漏检多周期或长期失联时及时失效旧周期，
/// 确保清理后可重新学习新周期而不被历史计数值永久锁死。
#[allow(clippy::too_many_arguments)]
pub fn update_period_observations(
    intervals_secs: &mut Vec<f64>,
    probe_misses: &mut u8,
    early_changes: &mut u8,
    last_confirmed_unchanged_at_ms: &mut Option<u64>,
    prev_change_at_ms: Option<u64>,
    observed_at_ms: u64,
    changed: bool,
    last_modified_at_ms: Option<u64>,
    max_cv: f64,
    max_samples: usize,
) -> Option<u64> {
    if observed_at_ms == 0 {
        return None;
    }
    let existing = if *probe_misses >= 2 || *early_changes >= 2 {
        None
    } else {
        estimate_period(intervals_secs, max_cv)
    };
    let invalidate = |ivs: &mut Vec<f64>, pm: &mut u8, ec: &mut u8| {
        ivs.clear();
        *pm = 0;
        *ec = 0;
    };

    if changed {
        let effective_ts = last_modified_at_ms
            .filter(|&m| {
                prev_change_at_ms.is_none_or(|p| m > p) && m <= observed_at_ms.saturating_add(5_000)
            })
            .map(|m| m.min(observed_at_ms))
            .unwrap_or(observed_at_ms);

        if let Some(prev) = prev_change_at_ms
            && effective_ts > prev
        {
            let raw_dt_secs = (effective_ts - prev) as f64 / 1000.0;
            let wall_dt_secs = observed_at_ms.saturating_sub(prev) as f64 / 1000.0;
            if let Some(est) = existing {
                let guard = (2.0 * est.std_secs).clamp(5.0, (est.mean_secs * 0.15).max(5.0));
                let upper_tol = (est.mean_secs + guard * 2.0).max(est.mean_secs * 1.35);
                if raw_dt_secs < est.mean_secs * 0.55 {
                    *early_changes = early_changes.saturating_add(1);
                    if *early_changes >= 2 || raw_dt_secs < est.mean_secs * 0.35 {
                        invalidate(intervals_secs, probe_misses, early_changes);
                    }
                    return Some(effective_ts);
                }
                if raw_dt_secs > upper_tol || wall_dt_secs > est.mean_secs * 2.2 {
                    if raw_dt_secs >= est.mean_secs * 1.75 || wall_dt_secs >= est.mean_secs * 2.2 {
                        invalidate(intervals_secs, probe_misses, early_changes);
                    } else {
                        *probe_misses = probe_misses.saturating_add(1);
                        if *probe_misses >= 2 {
                            invalidate(intervals_secs, probe_misses, early_changes);
                        }
                    }
                    return Some(effective_ts);
                }
            }

            if let Some(valid_dt) = non_aliased_change_interval_secs(
                prev,
                *last_confirmed_unchanged_at_ms,
                observed_at_ms,
                last_modified_at_ms,
                existing,
            ) {
                intervals_secs.push(valid_dt);
                let cap = max_samples.max(3);
                if intervals_secs.len() > cap {
                    let drain = intervals_secs.len() - cap;
                    intervals_secs.drain(0..drain);
                }
                *probe_misses = 0;
                *early_changes = 0;
            }
        }
        Some(effective_ts)
    } else {
        *last_confirmed_unchanged_at_ms = Some(observed_at_ms);
        if let (Some(est), Some(prev)) = (existing, prev_change_at_ms) {
            let elapsed_secs = observed_at_ms.saturating_sub(prev) as f64 / 1000.0;
            let guard = (2.0 * est.std_secs).clamp(5.0, (est.mean_secs * 0.15).max(5.0));
            if elapsed_secs >= est.mean_secs * 2.0 + guard {
                invalidate(intervals_secs, probe_misses, early_changes);
            } else if elapsed_secs >= est.mean_secs + guard {
                *probe_misses = probe_misses.saturating_add(1);
                if *probe_misses >= 2 {
                    invalidate(intervals_secs, probe_misses, early_changes);
                }
            }
        }
        None
    }
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

/// 若长期缺失观测导致相位已跨越多个预测周期，拒绝继续沿用过期周期相位。
pub fn next_periodic_delay_guarded(
    est: &PeriodEstimate,
    elapsed_since_change_secs: f64,
    min_secs: f64,
    max_secs: f64,
) -> Option<f64> {
    if !elapsed_since_change_secs.is_finite() || elapsed_since_change_secs > est.mean_secs * 2.5 {
        return None;
    }
    Some(next_periodic_delay_secs(
        est,
        elapsed_since_change_secs,
        min_secs,
        max_secs,
    ))
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

    #[test]
    fn late_change_and_prolonged_absence_invalidate_old_period_and_allow_relearning() {
        let mut intervals = Vec::new();
        let mut misses = 0u8;
        let mut early = 0u8;
        let mut unchanged = None;
        let mut prev_chg = Some(1_000u64);

        // Learn clean 300s period
        for k in 1..=3 {
            let base = 1_000 + (k - 1) * 300_000;
            update_period_observations(
                &mut intervals,
                &mut misses,
                &mut early,
                &mut unchanged,
                prev_chg,
                base + 240_000,
                false,
                None,
                0.22,
                8,
            );
            prev_chg = update_period_observations(
                &mut intervals,
                &mut misses,
                &mut early,
                &mut unchanged,
                prev_chg,
                base + 300_000,
                true,
                None,
                0.22,
                8,
            );
        }
        let est = estimate_period(&intervals, 0.22).expect("locked 300s period");
        assert!((est.mean_secs - 300.0).abs() < 1.0);

        // Prolonged absence > 2.5 * P must refuse stale phase reuse
        assert!(next_periodic_delay_guarded(&est, 800.0, 30.0, 3600.0).is_none());

        // Late change after missed cycles (600s instead of 300s) invalidates old 300s lock cleanly
        prev_chg = update_period_observations(
            &mut intervals,
            &mut misses,
            &mut early,
            &mut unchanged,
            prev_chg,
            prev_chg.unwrap() + 600_000,
            true,
            None,
            0.22,
            8,
        );
        assert!(estimate_period(&intervals, 0.22).is_none());
        assert_eq!(misses, 0);

        // Can now re-learn a new 600s period cleanly without permanent lock-out
        for _ in 0..3 {
            let p = prev_chg.unwrap();
            update_period_observations(
                &mut intervals,
                &mut misses,
                &mut early,
                &mut unchanged,
                prev_chg,
                p + 500_000,
                false,
                None,
                0.22,
                8,
            );
            prev_chg = update_period_observations(
                &mut intervals,
                &mut misses,
                &mut early,
                &mut unchanged,
                prev_chg,
                p + 600_000,
                true,
                None,
                0.22,
                8,
            );
        }
        let new_est = estimate_period(&intervals, 0.22).expect("re-learned 600s period");
        assert!((new_est.mean_secs - 600.0).abs() < 1.0);
    }

    #[test]
    fn last_modified_narrows_poll_latency_without_bypassing_aliasing_guard() {
        // Without unchanged confirmation on cold start, Last-Modified alone still rejects blind interval
        assert!(
            non_aliased_change_interval_secs(1_000, None, 601_000, Some(600_000), None).is_none()
        );
        // With unchanged check at 560s and server Last-Modified at 600s even though poll arrived at 900s:
        let dt = non_aliased_change_interval_secs(0, Some(560_000), 900_000, Some(600_000), None)
            .expect("lm removes post-change polling lag");
        assert!((dt - 600.0).abs() < 0.01);
    }
}
