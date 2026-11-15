//! 自适应新鲜度调度器（Lagrange 价值加权泊松更新模型）与无序偏置单周期候选集选择。
//!
//! 理论依据：Cho & Garcia-Molina, "Effective Page Refresh Policies for Web Crawlers", ACM TODS 28(4), 2003。

use pagesift_plan::experience::{FamilyExperienceTable, FamilyStats};
use pagesift_plan::render::family;
use pagesift_plan::strategy::{
    AcquisitionStrategy, AdaptDecision, StrategyInput, select_acquisition_strategy,
};
use pagesift_stat::Delta;
use pagesift_store::{
    DataSourceSpec, MAX_REQUESTS_PER_TICK_CAP, ResourceLifecycleState, ResourceRecord,
};

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
        let tombstone_secs = (max_secs * 7.0).clamp(min_secs, 7.0 * 86_400.0);
        return now_ms.saturating_add((tombstone_secs * 1000.0) as u64);
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
        return now_ms.saturating_add((backoff_secs * 1000.0) as u64);
    }

    let base_secs = match fam_stats.filter(|s| s.total_observations >= 2) {
        Some(st) => st.next_check_secs(Delta::DEFAULT.get()),
        None => {
            if substantive_changed {
                (default_secs * 0.5).max(min_secs)
            } else {
                default_secs
            }
        }
    };

    let weight = rec.importance_weight.clamp(0.2, 5.0);
    let hub_factor = if rec.role.is_hub_like() { 0.75 } else { 1.0 };
    let scaled_secs = (base_secs * hub_factor / weight.sqrt()).clamp(min_secs, max_secs);
    now_ms.saturating_add((scaled_secs * 1000.0) as u64)
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
    let max_reqs = spec
        .budget
        .max_requests_per_tick
        .clamp(1, MAX_REQUESTS_PER_TICK_CAP);
    let default_interval_ms = (spec
        .freshness
        .default_interval_secs
        .max(spec.freshness.min_interval_secs)
        .max(1) as f64)
        * 1000.0;
    let contract_id = spec.contract_id();

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
        let staleness_ratio = (elapsed_ms / default_interval_ms).max(0.1);
        let has_matching_validators = (rec.etag.is_some() || rec.last_modified.is_some())
            && rec.last_validator_contract_id.as_deref() == Some(contract_id.as_str())
            && rec.baseline_sufficient;

        let input = StrategyInput {
            url: rec.url.clone(),
            has_validators: has_matching_validators,
            has_embedded_json_hint: rec.has_embedded_json,
            is_hub_or_seed: rec.role.is_hub_like(),
            importance_weight: rec.importance_weight,
            staleness_ratio,
            remaining_budget_fraction: order_independent_budget_ratio,
            allow_browser: spec.allow_browser,
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
