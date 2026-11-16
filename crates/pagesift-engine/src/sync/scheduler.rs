//! 自适应新鲜度调度器（Lagrange 价值加权泊松更新模型）与无序偏置单周期候选集选择。
//!
//! 理论依据：Cho & Garcia-Molina, "Effective Page Refresh Policies for Web Crawlers", ACM TODS 28(4), 2003。

use std::collections::BTreeMap;

use pagesift_plan::experience::{FamilyExperienceTable, FamilyStats};
use pagesift_plan::render::family;
use pagesift_plan::strategy::{
    AcquisitionStrategy, AdaptDecision, StrategyInput, select_acquisition_strategy,
};
use pagesift_stat::Delta;
use pagesift_stat::poisson::ChangeRate;
use pagesift_stat::regularity::{
    WaterfillItem, hierarchical_change_rate, lagrange_waterfill_intervals,
};
use pagesift_store::{
    DataSourceSpec, MAX_REQUESTS_PER_TICK_CAP, ResourceLifecycleState, ResourceRecord,
};

fn sanitize_weight(w: f64) -> f64 {
    if w.is_finite() {
        w.clamp(0.2, 5.0)
    } else {
        1.0
    }
}

fn secs_to_millis_saturated(secs: f64) -> u64 {
    if !secs.is_finite() || secs <= 0.0 {
        return 0;
    }
    let millis = secs * 1000.0;
    if !millis.is_finite() || millis >= u64::MAX as f64 {
        u64::MAX
    } else {
        millis as u64
    }
}

/// 计算资源下一次应被检查的时间戳（毫秒）；Tombstone 资源在冷静期届满后安排低频复活探针（C20）。
pub fn compute_next_due_at_ms(
    spec: &DataSourceSpec,
    rec: &ResourceRecord,
    fam_stats: Option<&FamilyStats>,
    now_ms: u64,
    substantive_changed: bool,
) -> u64 {
    let min_secs = spec.freshness.min_interval_secs.max(1) as f64;
    let max_secs = spec
        .freshness
        .max_interval_secs
        .max(spec.freshness.min_interval_secs.max(1)) as f64;
    let default_secs = (spec.freshness.default_interval_secs as f64).clamp(min_secs, max_secs);

    if matches!(
        rec.state,
        ResourceLifecycleState::ConfirmedGone | ResourceLifecycleState::Migrated
    ) {
        // 用户显式配置的 min_interval_secs 优先级高于 7 天上限，防止 min > 7 天时 clamp panic
        let tombstone_upper = (7.0_f64 * 86_400.0).max(min_secs);
        let tombstone_secs = (max_secs * 7.0).clamp(min_secs, tombstone_upper);
        return now_ms.saturating_add(secs_to_millis_saturated(tombstone_secs));
    }

    // 临时故障或疑似失联：按指数退避尽快复核
    if matches!(
        rec.state,
        ResourceLifecycleState::TransientFailure | ResourceLifecycleState::SuspectedGone
    ) {
        let exp = rec
            .consecutive_failures
            .max(rec.consecutive_404s)
            .clamp(1, 6);
        let backoff_secs = (min_secs * (1u64 << (exp - 1)) as f64).clamp(min_secs, max_secs);
        return now_ms.saturating_add(secs_to_millis_saturated(backoff_secs));
    }

    // Priority 1: 单资源自身经非混叠验证的周期性规律（不混叠才触发，否则无缝回退层次泊松）
    if !substantive_changed
        && let Some(delay_secs) = rec.learn.next_periodic_delay_secs(
            rec.last_substantive_change_at_ms,
            now_ms,
            min_secs,
            max_secs,
        )
    {
        return now_ms.saturating_add(secs_to_millis_saturated(delay_secs));
    }

    // Priority 2: 族级共享周期规律（针对冷启动/稀疏同构路径）
    if !substantive_changed
        && rec.learn.change_count < 2
        && let Some(st) = fam_stats.filter(|s| s.total_observations >= 3)
        && let Some(fam_delay) = st.substantive_change.regularity.next_delay_secs(
            rec.last_substantive_change_at_ms.unwrap_or(now_ms),
            min_secs,
            max_secs,
        )
    {
        return now_ms.saturating_add(secs_to_millis_saturated(fam_delay));
    }

    // Priority 3: Gamma-Poisson 层次收缩，融合族先验率与单资源实际变/未变暴露，区分长稳与突发个体
    let raw_base = match fam_stats.filter(|s| s.total_observations >= 2) {
        Some(st) => {
            if rec.learn.exposure_secs >= min_secs || rec.learn.change_count > 0 {
                let fam_rate = st.substantive_change_rate_per_sec();
                let prior_w = 7200.0;
                let blended_rate = hierarchical_change_rate(
                    fam_rate,
                    prior_w,
                    rec.learn.change_count,
                    rec.learn.exposure_secs,
                );
                let eff_exp = (prior_w + rec.learn.exposure_secs).max(1.0);
                let eff_k = blended_rate * eff_exp;
                let mut intervals = Vec::new();
                if eff_k >= 0.5 && blended_rate > 0.0 {
                    intervals.push(1.0 / blended_rate);
                }
                ChangeRate {
                    shape: 1.0 + eff_k,
                    exposure: eff_exp,
                    changed: intervals,
                }
                .next_check(Delta::DEFAULT.get())
            } else {
                st.next_check_secs(Delta::DEFAULT.get())
            }
        }
        None => {
            if substantive_changed {
                (default_secs * 0.5).max(min_secs)
            } else if rec.learn.unchanged_count >= 2 && rec.learn.change_count == 0 {
                (default_secs * (1.0 + 0.35 * (rec.learn.unchanged_count.min(4) as f64)))
                    .min(max_secs)
            } else {
                default_secs
            }
        }
    };
    let base_secs = if raw_base.is_finite() {
        raw_base
    } else {
        default_secs
    };

    let weight = sanitize_weight(rec.importance_weight);
    let hub_factor = if rec.role.is_hub_like() { 0.75 } else { 1.0 };
    let scaled_secs = (base_secs * hub_factor / weight.sqrt()).clamp(min_secs, max_secs);
    now_ms.saturating_add(secs_to_millis_saturated(scaled_secs))
}

#[derive(Debug, Clone)]
pub struct ScheduledCandidate {
    pub record: ResourceRecord,
    pub decision: AdaptDecision,
}

/// 筛选到期资源、生成与输入遍历顺序无关的自适应策略（C28/C29）并按优先级排序。
pub fn select_tick_candidates(
    spec: &DataSourceSpec,
    inventory: &[ResourceRecord],
    experience: &FamilyExperienceTable,
    now_ms: u64,
    force_all_active: bool,
) -> (Vec<ScheduledCandidate>, usize, usize) {
    select_tick_candidates_with_context(
        spec,
        inventory,
        experience,
        now_ms,
        force_all_active,
        1.0,
        0.0,
    )
}

/// 联合考虑预算约束拉格朗日注水间隔、浏览器配额余量及实时标签池占用率的候选调度选择（G2/G3）。
pub fn select_tick_candidates_with_context(
    spec: &DataSourceSpec,
    inventory: &[ResourceRecord],
    experience: &FamilyExperienceTable,
    now_ms: u64,
    force_all_active: bool,
    browser_budget_fraction: f64,
    browser_tab_utilization: f64,
) -> (Vec<ScheduledCandidate>, usize, usize) {
    let max_reqs = spec
        .budget
        .max_requests_per_tick
        .clamp(1, MAX_REQUESTS_PER_TICK_CAP);
    let min_secs = spec.freshness.min_interval_secs.max(1) as f64;
    let max_secs = spec
        .freshness
        .max_interval_secs
        .max(spec.freshness.min_interval_secs.max(1)) as f64;
    let default_secs = (spec.freshness.default_interval_secs as f64).clamp(min_secs, max_secs);
    let default_interval_ms = default_secs * 1000.0;
    let contract_id = spec.contract_id();

    // 在活跃集合上计算受预算约束的拉格朗日最优间隔 T_j*
    let active_records: Vec<&ResourceRecord> = inventory
        .iter()
        .filter(|r| {
            !matches!(
                r.state,
                ResourceLifecycleState::ConfirmedGone | ResourceLifecycleState::Migrated
            )
        })
        .collect();
    let wf_items: Vec<WaterfillItem> = active_records
        .iter()
        .map(|rec| {
            let fam_name = family(&rec.url);
            let fam_stats = experience.get_family(&fam_name);
            let fam_rate = fam_stats.as_ref().map_or(
                1.0 / default_secs,
                FamilyStats::substantive_change_rate_per_sec,
            );
            let rate = hierarchical_change_rate(
                fam_rate,
                7200.0,
                rec.learn.change_count,
                rec.learn.exposure_secs,
            )
            .max(1.0 / max_secs.max(3600.0));
            let cost_ms = fam_stats.map_or(55.0, |s| s.ewma_cost_ms.max(15.0));
            let weight = sanitize_weight(rec.importance_weight)
                * if rec.role.is_hub_like() { 1.5 } else { 1.0 };
            WaterfillItem {
                weight,
                rate_per_sec: rate,
                cost_ms,
                min_interval_secs: min_secs,
                max_interval_secs: max_secs,
            }
        })
        .collect();
    let sustainable_budget_cost_per_sec = ((max_reqs as f64) * 140.0) / default_secs.max(1.0);
    let wf_intervals = lagrange_waterfill_intervals(&wf_items, sustainable_budget_cost_per_sec);
    let wf_by_key: BTreeMap<&str, f64> = active_records
        .iter()
        .zip(wf_intervals)
        .map(|(r, t)| (r.resource_key.as_str(), t))
        .collect();

    let due_records: Vec<&ResourceRecord> = inventory
        .iter()
        .filter(|r| match r.state {
            ResourceLifecycleState::ConfirmedGone | ResourceLifecycleState::Migrated => {
                // 冷静期届满时才发起一次低频恢复探针，force_all 不浪费预算反复敲打已确认死链（C20）
                r.next_due_at_ms <= now_ms
            }
            _ => {
                force_all_active
                    || r.state == ResourceLifecycleState::Discovered
                    || r.next_due_at_ms <= now_ms
            }
        })
        .collect();

    let total_due = due_records.len();
    let order_independent_budget_ratio =
        (max_reqs as f64 / total_due.max(1) as f64).clamp(0.05, 1.0);
    let mut scored = Vec::with_capacity(total_due);

    for rec in due_records {
        let fam_name = family(&rec.url);
        let fam_stats = experience.get_family(&fam_name);
        let elapsed_ms = rec
            .last_checked_at_ms
            .map_or(default_interval_ms * 2.0, |last| {
                now_ms.saturating_sub(last) as f64
            });
        let target_ms = wf_by_key
            .get(rec.resource_key.as_str())
            .map(|&s| s * 1000.0)
            .unwrap_or(default_interval_ms);
        let staleness_ratio = (elapsed_ms / target_ms.max(1000.0)).max(0.1);
        let has_matching_validators = (rec.etag.is_some() || rec.last_modified.is_some())
            && rec.last_validator_contract_id.as_deref() == Some(contract_id.as_str())
            && rec.baseline_sufficient;

        let input = StrategyInput {
            url: rec.url.clone(),
            has_validators: has_matching_validators,
            has_embedded_json_hint: rec.has_embedded_json,
            is_hub_or_seed: rec.role.is_hub_like(),
            importance_weight: sanitize_weight(rec.importance_weight),
            staleness_ratio,
            remaining_budget_fraction: order_independent_budget_ratio,
            allow_browser: spec.allow_browser,
            browser_budget_fraction,
            browser_tab_utilization,
            probe_recovery_eligible: true,
        };
        let decision = select_acquisition_strategy(&input, fam_stats.as_ref());
        scored.push(ScheduledCandidate {
            record: rec.clone(),
            decision,
        });
    }

    scored.sort_by(|a, b| {
        b.decision
            .priority_score
            .total_cmp(&a.decision.priority_score)
            .then_with(|| a.record.depth.cmp(&b.record.depth))
            .then_with(|| a.record.url.cmp(&b.record.url))
    });

    let mut selected = Vec::new();
    let mut deferred = 0usize;
    for cand in scored {
        if cand.decision.strategy == AcquisitionStrategy::DeferOrSkip {
            deferred += 1;
            continue;
        }
        if selected.len() < max_reqs {
            selected.push(cand);
        } else {
            deferred += 1;
        }
    }

    (selected, deferred, total_due)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_store::{FreshnessPolicy, ResourceRole, SyncBudget};

    fn test_spec(min_s: u64, def_s: u64, max_s: u64) -> DataSourceSpec {
        DataSourceSpec {
            id: "sched-test".to_string(),
            instance_id: String::new(),
            name: None,
            seeds: vec!["https://example.com/".to_string()],
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            max_depth: 2,
            allow_subdomains: false,
            freshness: FreshnessPolicy {
                min_interval_secs: min_s,
                default_interval_secs: def_s,
                max_interval_secs: max_s,
                confirm_gone_after_404s: 2,
            },
            budget: SyncBudget::default(),
            extract_schema: None,
            extract_prompt: None,
            required_fields: Vec::new(),
            allow_browser: true,
            revision: 1,
            created_at_ms: 1000,
            updated_at_ms: 1000,
        }
    }

    fn test_rec(state: ResourceLifecycleState, weight: f64) -> ResourceRecord {
        ResourceRecord {
            resource_key: "rk".to_string(),
            url: "https://example.com/p".to_string(),
            canonical_url: None,
            migrated_to: None,
            parent_url: None,
            depth: 1,
            role: ResourceRole::Detail,
            state,
            discovered_at_ms: 1000,
            last_checked_at_ms: Some(1000),
            last_substantive_change_at_ms: None,
            next_due_at_ms: 1000,
            etag: None,
            last_modified: None,
            content_hash: None,
            latest_doc_id: None,
            last_status_code: Some(404),
            consecutive_failures: 1,
            consecutive_404s: 2,
            importance_weight: weight,
            last_strategy: None,
            title: None,
            claim_count: 0,
            last_validator_contract_id: None,
            last_authoritative_status: None,
            baseline_sufficient: false,
            has_embedded_json: false,
            text_signature: None,
            confirmed_gone_at_ms: Some(1000),
            learn: pagesift_store::ResourceLearnStats::default(),
        }
    }

    #[test]
    fn p1_05_large_min_interval_tombstone_and_extreme_bounds_never_panic() {
        // Reproducer from P1-05: min=864000 (> 7 days), default=1296000, max=2592000.
        let spec = test_spec(864_000, 1_296_000, 2_592_000);
        for state in [
            ResourceLifecycleState::ConfirmedGone,
            ResourceLifecycleState::Migrated,
        ] {
            let rec = test_rec(state, 1.0);
            let due = compute_next_due_at_ms(&spec, &rec, None, 10_000, false);
            assert_eq!(due, 10_000 + 864_000 * 1000);
        }

        // Inverted persisted intervals, zero, NaN weight, and u64::MAX boundaries must saturate safely.
        let huge = test_spec(u64::MAX, 0, u64::MAX);
        let nan_rec = test_rec(ResourceLifecycleState::Active, f64::NAN);
        let due_huge = compute_next_due_at_ms(&huge, &nan_rec, None, u64::MAX - 50, false);
        assert_eq!(due_huge, u64::MAX);

        let zero_spec = test_spec(0, 0, 0);
        let due_zero = compute_next_due_at_ms(&zero_spec, &nan_rec, None, 5_000, false);
        assert!(due_zero >= 6_000);
    }
}
