//! 自适应新鲜度调度器（Lagrange 价值加权泊松更新模型）与无序偏置单周期候选集选择。
//!
//! 理论依据：Cho & Garcia-Molina, "Effective Page Refresh Policies for Web Crawlers", ACM TODS 28(4), 2003。

use std::collections::{BTreeMap, BTreeSet};

use pagesift_plan::experience::{FamilyExperienceTable, FamilyStats};
use pagesift_plan::render::family;
use pagesift_plan::strategy::{
    AcquisitionStrategy, AdaptDecision, StrategyInput, select_acquisition_strategy,
};
use pagesift_stat::Delta;
use pagesift_stat::poisson::ChangeRate;
use pagesift_stat::regularity::{
    WaterfillItem, hierarchical_change_rate, lagrange_waterfill_intervals,
    next_periodic_delay_guarded,
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

/// 计算单资源在无全局预算冲突下的本地建议等待秒数。
pub(crate) fn compute_local_interval_secs(
    spec: &DataSourceSpec,
    rec: &ResourceRecord,
    fam_stats: Option<&FamilyStats>,
    now_ms: u64,
    substantive_changed: bool,
) -> f64 {
    let min_secs = spec.freshness.min_interval_secs.max(1) as f64;
    let max_secs = spec
        .freshness
        .max_interval_secs
        .max(spec.freshness.min_interval_secs.max(1)) as f64;
    let default_secs = (spec.freshness.default_interval_secs as f64).clamp(min_secs, max_secs);

    // Priority 1: 单资源自身经非混叠验证的周期性规律（即便本轮恰好发生周期边沿变化，仍继续按相位锁定下一周期）
    if let Some(delay_secs) = rec.learn.next_periodic_delay_secs(
        rec.last_substantive_change_at_ms,
        now_ms,
        min_secs,
        max_secs,
    ) {
        return delay_secs;
    }

    // Priority 2: 族级共享周期长度先验 + 资源自身最后实质变更时间锚点 + 真实当前时刻 `now_ms`
    if rec.learn.change_count < 2
        && let Some(st) = fam_stats.filter(|s| s.total_observations >= 3)
        && let Some(est) = st.shared_period_estimate()
        && let Some(res_last_chg) = rec.last_substantive_change_at_ms
        && let Some(fam_delay) = next_periodic_delay_guarded(
            &est,
            now_ms.saturating_sub(res_last_chg) as f64 / 1000.0,
            min_secs,
            max_secs,
        )
    {
        return fam_delay;
    }

    // Priority 3: Gamma-Poisson 层次收缩，融合族先验率与单资源实际变/未变暴露
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
    (base_secs * hub_factor / weight.sqrt()).clamp(min_secs, max_secs)
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

    let scaled_secs =
        compute_local_interval_secs(spec, rec, fam_stats, now_ms, substantive_changed);
    now_ms.saturating_add(secs_to_millis_saturated(scaled_secs))
}

fn compute_waterfill_targets(
    spec: &DataSourceSpec,
    active_records: &[&ResourceRecord],
    experience: &FamilyExperienceTable,
) -> (Vec<WaterfillItem>, Vec<f64>) {
    if active_records.is_empty() {
        return (Vec::new(), Vec::new());
    }
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
            let cost_ms = fam_stats.as_ref().map_or(55.0, |s| {
                let is_spa_browser = spec.allow_browser
                    && s.http_sufficient.trials() >= 2.5
                    && s.http_sufficient.mean() < 0.25
                    && s.browser_gain.mean() > 0.60;
                if is_spa_browser {
                    s.expected_cost_for(AcquisitionStrategy::DirectBrowser)
                } else if rec.etag.is_some() || rec.last_modified.is_some() {
                    s.expected_cost_for(AcquisitionStrategy::ConditionalHttp)
                } else {
                    s.ewma_http_cost_ms.max(s.ewma_cost_ms).max(15.0)
                }
            });
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

    let cadence = default_secs.clamp(min_secs, max_secs);
    let mean_cost = wf_items.iter().map(|i| i.cost_ms).sum::<f64>() / wf_items.len() as f64;
    let wall_cap_per_sec = if spec.budget.max_wall_clock_ms > 0 {
        spec.budget.max_wall_clock_ms as f64 / cadence
    } else {
        f64::INFINITY
    };
    let budget_cost_per_sec = ((max_reqs as f64 * mean_cost) / cadence)
        .min(wall_cap_per_sec)
        .max(1e-3);
    let mut wf = lagrange_waterfill_intervals(&wf_items, budget_cost_per_sec);
    let reqs_per_cadence =
        |intervals: &[f64]| -> f64 { intervals.iter().map(|&t| cadence / t.max(1.0)).sum() };
    if reqs_per_cadence(&wf) > max_reqs as f64 {
        let min_burn: f64 = wf_items
            .iter()
            .map(|it| it.cost_ms.max(1.0) / it.max_interval_secs.max(min_secs))
            .sum();
        let mut lo = min_burn;
        let mut hi = budget_cost_per_sec;
        for _ in 0..24 {
            let mid = 0.5 * (lo + hi);
            let trial = lagrange_waterfill_intervals(&wf_items, mid);
            if reqs_per_cadence(&trial) > max_reqs as f64 {
                hi = mid;
            } else {
                lo = mid;
                wf = trial;
            }
        }
    }
    (wf_items, wf)
}

/// 在同步周期提交前，将全局拉格朗日注水目标间隔实际写入已访问活跃资源的 `next_due_at_ms`：
/// - 若全局请求与墙钟预算宽裕（本地基线排程未超配额），保留精准的本地周期/Gamma-Poisson 间隔；
/// - 若预算紧绷，则将本次已执行活跃资源的 `next_due_at_ms` 真实拉宽至注水最优解 `T_j*`，
///   既避免低权重/高耗时冷资源每轮刚到即逾期堆积 `candidates_due`，又保留高频关键资源的紧周期检查。
pub fn apply_waterfill_schedules_to_inventory(
    spec: &DataSourceSpec,
    inventory: &mut BTreeMap<String, ResourceRecord>,
    experience: &FamilyExperienceTable,
    now_ms: u64,
    visited_keys: &BTreeSet<String>,
) {
    if visited_keys.is_empty() {
        return;
    }
    let active_records: Vec<&ResourceRecord> = inventory
        .values()
        .filter(|r| matches!(r.state, ResourceLifecycleState::Active))
        .collect();
    if active_records.is_empty() {
        return;
    }

    let min_secs = spec.freshness.min_interval_secs.max(1) as f64;
    let max_secs = spec
        .freshness
        .max_interval_secs
        .max(spec.freshness.min_interval_secs.max(1)) as f64;
    let default_secs = (spec.freshness.default_interval_secs as f64).clamp(min_secs, max_secs);
    let cadence = default_secs.clamp(min_secs, max_secs);
    let max_reqs = spec
        .budget
        .max_requests_per_tick
        .clamp(1, MAX_REQUESTS_PER_TICK_CAP) as f64;

    let (wf_items, wf_intervals) = compute_waterfill_targets(spec, &active_records, experience);

    // 计算无注水压缩前的期望请求与成本消耗，判断全局预算是否真正构成紧约束
    let mut unconstrained_reqs = 0.0;
    let mut unconstrained_burn = 0.0;
    for (rec, wf_item) in active_records.iter().zip(&wf_items) {
        let fam_stats = experience.get_family(&family(&rec.url));
        let local_secs = compute_local_interval_secs(spec, rec, fam_stats.as_ref(), now_ms, false);
        unconstrained_reqs += cadence / local_secs.max(1.0);
        unconstrained_burn += wf_item.cost_ms / local_secs.max(1.0);
    }
    let wall_cap_per_sec = if spec.budget.max_wall_clock_ms > 0 {
        spec.budget.max_wall_clock_ms as f64 / cadence
    } else {
        f64::INFINITY
    };
    if unconstrained_reqs <= max_reqs && unconstrained_burn <= wall_cap_per_sec {
        return;
    }

    let wf_targets: Vec<(String, f64)> = active_records
        .iter()
        .zip(wf_intervals)
        .filter(|(rec, _)| visited_keys.contains(&rec.resource_key))
        .map(|(rec, t)| (rec.resource_key.clone(), t.clamp(min_secs, max_secs)))
        .collect();
    for (key, wf_secs) in wf_targets {
        if let Some(rec) = inventory.get_mut(&key) {
            let anchor = rec
                .valid_observation_anchor_ms()
                .unwrap_or(now_ms)
                .min(now_ms);
            rec.next_due_at_ms = anchor.saturating_add(secs_to_millis_saturated(wf_secs));
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScheduledCandidate {
    pub record: ResourceRecord,
    pub decision: AdaptDecision,
}

fn build_strategy_input(
    spec: &DataSourceSpec,
    rec: &ResourceRecord,
    contract_id: &str,
    staleness_ratio: f64,
    remaining_budget_fraction: f64,
    browser_budget_fraction: f64,
    browser_tab_utilization: f64,
) -> StrategyInput {
    let has_matching_validators = (rec.etag.is_some() || rec.last_modified.is_some())
        && rec.last_validator_contract_id.as_deref() == Some(contract_id)
        && rec.baseline_sufficient;
    StrategyInput {
        url: rec.url.clone(),
        has_validators: has_matching_validators,
        has_embedded_json_hint: rec.has_embedded_json,
        is_hub_or_seed: rec.role.is_hub_like(),
        importance_weight: sanitize_weight(rec.importance_weight),
        staleness_ratio,
        remaining_budget_fraction,
        allow_browser: spec.allow_browser,
        browser_budget_fraction,
        browser_tab_utilization,
        probe_recovery_eligible: true,
    }
}

/// 在单波次候选执行循环内按实时消耗的浏览器余量、并发标签占用与最新族经验刷新当前决策（含影子价格与升阶授权）。
pub(crate) fn refresh_candidate_decision(
    spec: &DataSourceSpec,
    rec: &ResourceRecord,
    initial_decision: &AdaptDecision,
    experience: &FamilyExperienceTable,
    remaining_budget_fraction: f64,
    browser_budget_fraction: f64,
    browser_tab_utilization: f64,
) -> AdaptDecision {
    let fam_stats = experience.get_family(&family(&rec.url));
    let contract_id = spec.contract_id();
    let input = build_strategy_input(
        spec,
        rec,
        &contract_id,
        (initial_decision.priority_score / sanitize_weight(rec.importance_weight)).clamp(0.1, 4.0),
        remaining_budget_fraction.clamp(0.05, 1.0),
        browser_budget_fraction,
        browser_tab_utilization,
    );
    let mut refreshed = select_acquisition_strategy(&input, fam_stats.as_ref());
    refreshed.priority_score = initial_decision.priority_score;
    refreshed
}

/// 判断单资源在给定时间点是否已到期需要调度检查（死链/已迁移不受 `force_all` 强刷）。
pub(crate) fn is_record_due(r: &ResourceRecord, now_ms: u64, force_all_active: bool) -> bool {
    match r.state {
        ResourceLifecycleState::ConfirmedGone | ResourceLifecycleState::Migrated => {
            r.next_due_at_ms <= now_ms
        }
        _ => {
            force_all_active
                || r.state == ResourceLifecycleState::Discovered
                || r.next_due_at_ms <= now_ms
        }
    }
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

    // 在活跃集合上计算受预算约束的拉格朗日最优间隔 T_j*（复用统一建模函数）
    let active_records: Vec<&ResourceRecord> = inventory
        .iter()
        .filter(|r| {
            !matches!(
                r.state,
                ResourceLifecycleState::ConfirmedGone | ResourceLifecycleState::Migrated
            )
        })
        .collect();
    let (_, wf_intervals) = compute_waterfill_targets(spec, &active_records, experience);
    let wf_by_key: BTreeMap<&str, f64> = active_records
        .iter()
        .zip(wf_intervals)
        .map(|(r, t)| (r.resource_key.as_str(), t))
        .collect();

    let due_records: Vec<&ResourceRecord> = inventory
        .iter()
        .filter(|r| is_record_due(r, now_ms, force_all_active))
        .collect();

    let total_due = due_records.len();
    let order_independent_budget_ratio =
        (max_reqs as f64 / total_due.max(1) as f64).clamp(0.05, 1.0);
    let mut scored = Vec::with_capacity(total_due);

    for rec in due_records {
        let fam_name = family(&rec.url);
        let fam_stats = experience.get_family(&fam_name);
        let elapsed_ms = rec
            .valid_observation_anchor_ms()
            .or(rec.last_attempted_at_ms)
            .or(rec.last_checked_at_ms)
            .map_or(default_interval_ms * 2.0, |last| {
                now_ms.saturating_sub(last) as f64
            });
        let target_ms = wf_by_key
            .get(rec.resource_key.as_str())
            .map(|&s| s * 1000.0)
            .unwrap_or(default_interval_ms);
        let staleness_ratio = (elapsed_ms / target_ms.max(1000.0)).max(0.1);
        let input = build_strategy_input(
            spec,
            rec,
            &contract_id,
            staleness_ratio,
            order_independent_budget_ratio,
            browser_budget_fraction,
            browser_tab_utilization,
        );
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
            last_attempted_at_ms: Some(1000),
            last_valid_observed_at_ms: None,
            last_substantive_change_at_ms: None,
            next_due_at_ms: 1000,
            etag: None,
            last_modified: None,
            last_modified_at_ms: None,
            last_modified_dynamic: false,
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

    #[test]
    fn waterfill_updates_real_next_due_at_ms_and_period_prior_uses_resource_phase() {
        let mut spec = test_spec(30, 120, 7200);
        spec.budget.max_requests_per_tick = 2;

        let exp = FamilyExperienceTable::new();
        for k in 0..6 {
            exp.record_observation(
                "https://ex.com/hot",
                &pagesift_plan::experience::FamilyObservation {
                    observed_at_ms: 1_000 + k * 100,
                    http_sufficient: Some(true),
                    substantive_change: Some((60.0, true)),
                    http_cost_ms: Some(30.0),
                    cost_ms: 30.0,
                    ..Default::default()
                },
            );
            exp.record_observation(
                "https://ex.com/cold",
                &pagesift_plan::experience::FamilyObservation {
                    observed_at_ms: 1_000 + k * 100,
                    http_sufficient: Some(false),
                    browser_gain: Some(true),
                    used_browser: true,
                    browser_cost_ms: Some(1_600.0),
                    substantive_change: Some((7_200.0, false)),
                    cost_ms: 1_600.0,
                    ..Default::default()
                },
            );
        }

        let now = 1_000_000u64;
        let mut hot = test_rec(ResourceLifecycleState::Active, 2.5);
        hot.resource_key = "rk-hot".into();
        hot.url = "https://ex.com/hot/1".into();
        hot.last_valid_observed_at_ms = Some(now);
        hot.next_due_at_ms = now + 60_000;
        hot.learn.change_count = 6;
        hot.learn.exposure_secs = 360.0;

        let mut cold = test_rec(ResourceLifecycleState::Active, 0.5);
        cold.resource_key = "rk-cold".into();
        cold.url = "https://ex.com/cold/1".into();
        cold.last_valid_observed_at_ms = Some(now);
        cold.next_due_at_ms = now + 60_000;
        cold.learn.unchanged_count = 6;
        cold.learn.exposure_secs = 36_000.0;

        let mut unvisited = test_rec(ResourceLifecycleState::Active, 1.0);
        unvisited.resource_key = "rk-unv".into();
        unvisited.url = "https://ex.com/hot/2".into();
        unvisited.next_due_at_ms = now - 5_000;

        let mut inv = BTreeMap::from([
            (hot.resource_key.clone(), hot),
            (cold.resource_key.clone(), cold),
            (unvisited.resource_key.clone(), unvisited),
        ]);
        let visited = BTreeSet::from(["rk-hot".to_string(), "rk-cold".to_string()]);
        apply_waterfill_schedules_to_inventory(&spec, &mut inv, &exp, now, &visited);

        let hot_due = inv.get("rk-hot").unwrap().next_due_at_ms - now;
        let cold_due = inv.get("rk-cold").unwrap().next_due_at_ms - now;
        assert!(
            hot_due < cold_due,
            "tight budget waterfill must schedule cold expensive items later than hot cheap items: hot={hot_due}, cold={cold_due}"
        );
        assert_eq!(
            inv.get("rk-unv").unwrap().next_due_at_ms,
            now - 5_000,
            "unvisited overdue item must remain due rather than pushed forward"
        );

        // 族级周期先验 + 个体相位验证：不同个体变更时间错开 100s，next_due 必须以 real now_ms - resource.last_change 计算剩余相位
        let mut fam = FamilyStats::new("https://ex.com/per");
        for i in 0..4 {
            fam.observe(&pagesift_plan::experience::FamilyObservation {
                observed_at_ms: 1000 + i * 100,
                http_sufficient: Some(true),
                resource_detected_period_secs: Some((600.0, 2.0)),
                cost_ms: 40.0,
                ..Default::default()
            });
        }
        let mut peer_a = test_rec(ResourceLifecycleState::Active, 1.0);
        peer_a.last_substantive_change_at_ms = Some(now - 500_000);
        let due_a = compute_next_due_at_ms(&spec, &peer_a, Some(&fam), now, false);
        // Period=600s + guard ~ 15s -> remaining ~ 115s from now (NOT 615s!)
        assert!(
            due_a - now < 200_000,
            "expected remaining phase ~115s, got {}ms",
            due_a - now
        );
    }
}
