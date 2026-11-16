//! Coverage completeness assessment for sync ticks and site crawls.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    /// 范围内所有已发现资源均已完成权威核验，无未检查资源、预算截断、访问受限或采集缺口。
    CompleteWithinScope,
    /// 已达到调用方设定的目标条目数或结构化声明目标。
    TargetGoalSatisfied,
    /// 因达到请求数、渲染数或时间预算上限而截断，仍有到期或未检查资源待处理。
    BudgetCapped,
    /// 因 robots.txt、403/429 或反爬挑战导致关键入口或全部已知资源受限。
    AccessRestricted,
    /// 队列已处理，但存在未检查新发现、临时故障、访问受限或采集充足性缺口。
    PartialWithGaps,
}

/// 范围覆盖评估输入统计。
#[derive(Debug, Clone, Default)]
pub struct CoverageInput {
    pub discovered_total: usize,
    /// 权威成功的资源数（200 满足质量门、304 匹配基线或自然 404/410 已确证）。
    pub authoritative_successes: usize,
    /// 本周期发起尝试的资源数（含失败/受限/拦截）。
    pub attempted_total: usize,
    /// 此前已检查且本周期尚未到期、依然新鲜的资源数。
    pub fresh_unexpired: usize,
    /// 已发现但从未检查过的活跃资源数（如抓取期间新发现的链接）。
    pub unchecked_total: usize,
    /// 因预算、退避或策略跳过而明确推迟的候选数。
    pub deferred_count: usize,
    pub transient_failures: usize,
    pub access_restricted_count: usize,
    /// 已拿到响应但未达到采集目标（如浏览器预算耗尽或缺少接地证据）的资源数。
    pub insufficient_captures: usize,
    /// 未解决的不可靠观测缺口数（如 `decode:*` 乱码或 `body_truncated` 截断）；存在未解决缺口时绝不能报告 `complete_within_scope`。
    pub uncertain_observations: usize,
    pub pending_confirmation: usize,
    pub target_goal_reached: bool,
    pub budget_exhausted: bool,
}

/// 范围覆盖评估报告。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageReport {
    pub status: CoverageStatus,
    pub discovered_total: usize,
    /// 权威核验通过数量（不把 403/429/5xx 尝试计入完成）。
    pub processed_total: usize,
    #[serde(default)]
    pub attempted_total: usize,
    #[serde(default)]
    pub fresh_unexpired: usize,
    #[serde(default)]
    pub unchecked_total: usize,
    pub deferred_count: usize,
    pub transient_failures: usize,
    pub access_restricted_count: usize,
    #[serde(default)]
    pub insufficient_captures: usize,
    #[serde(default)]
    pub uncertain_observations: usize,
    pub reasons: Vec<String>,
}

/// 根据周期执行统计与真实库存状态评估本次同步的覆盖完整性。
pub fn evaluate_coverage_status(input: &CoverageInput) -> CoverageReport {
    let mut reasons = Vec::new();
    if input.transient_failures > 0 {
        reasons.push(format!("transient_failures={}", input.transient_failures));
    }
    if input.access_restricted_count > 0 {
        reasons.push(format!(
            "access_restricted={}",
            input.access_restricted_count
        ));
    }
    if input.insufficient_captures > 0 {
        reasons.push(format!(
            "insufficient_captures={}",
            input.insufficient_captures
        ));
    }
    if input.uncertain_observations > 0 {
        reasons.push(format!(
            "uncertain_observations={}",
            input.uncertain_observations
        ));
    }
    if input.pending_confirmation > 0 {
        reasons.push(format!(
            "pending_confirmation={}",
            input.pending_confirmation
        ));
    }
    if input.unchecked_total > 0 {
        reasons.push(format!("unchecked_discovered={}", input.unchecked_total));
    }
    if input.deferred_count > 0 {
        reasons.push(format!("deferred={}", input.deferred_count));
    }
    if input.budget_exhausted {
        reasons.push("budget_exhausted".to_string());
    }

    let has_pending_work = input.deferred_count > 0
        || input.unchecked_total > 0
        || input.insufficient_captures > 0
        || input.uncertain_observations > 0;
    let status = if input.target_goal_reached {
        reasons.insert(0, "target_goal_satisfied".to_string());
        CoverageStatus::TargetGoalSatisfied
    } else if input.budget_exhausted && has_pending_work {
        let pending = input.deferred_count.max(
            input
                .unchecked_total
                .saturating_add(input.insufficient_captures)
                .saturating_add(input.uncertain_observations),
        );
        reasons.insert(0, format!("budget_capped_with_{pending}_pending"));
        CoverageStatus::BudgetCapped
    } else if input.access_restricted_count > 0 && input.authoritative_successes == 0 {
        reasons.insert(
            0,
            format!(
                "access_restricted_on_{}_resources",
                input.access_restricted_count
            ),
        );
        CoverageStatus::AccessRestricted
    } else if input.transient_failures > 0
        || input.access_restricted_count > 0
        || input.deferred_count > 0
        || input.unchecked_total > 0
        || input.insufficient_captures > 0
        || input.uncertain_observations > 0
        || input.pending_confirmation > 0
        || (input.discovered_total > 0
            && input
                .authoritative_successes
                .saturating_add(input.fresh_unexpired)
                == 0)
        || input.discovered_total == 0
    {
        if input.discovered_total == 0 {
            reasons.push("no_in_scope_resources".to_string());
        } else if input
            .authoritative_successes
            .saturating_add(input.fresh_unexpired)
            == 0
            && reasons.is_empty()
        {
            reasons.push("no_authoritative_verifications".to_string());
        }
        CoverageStatus::PartialWithGaps
    } else {
        reasons.push("all_in_scope_resources_verified".to_string());
        CoverageStatus::CompleteWithinScope
    };

    CoverageReport {
        status,
        discovered_total: input.discovered_total,
        processed_total: input.authoritative_successes,
        attempted_total: input.attempted_total,
        fresh_unexpired: input.fresh_unexpired,
        unchecked_total: input.unchecked_total,
        deferred_count: input.deferred_count,
        transient_failures: input.transient_failures,
        access_restricted_count: input.access_restricted_count,
        insufficient_captures: input.insufficient_captures,
        uncertain_observations: input.uncertain_observations,
        reasons,
    }
}
