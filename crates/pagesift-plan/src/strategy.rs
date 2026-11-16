//! 自适应采集策略（`AcquisitionStrategy`）、证据驱动充足性判定（`CaptureAssessment`）
//! 与范围覆盖状态评估（`CoverageStatus`）。
//!
//! 1.5 Loop 的 Adapt 层不再把所有请求简化为“HTTP 还是开浏览器”的二选一：
//! - 决策前：结合资源缓存校验子（ETag / Last-Modified）、路径族历史经验（[`FamilyStats`]）、
//!   资源角色与剩余预算，选择最省成本且能满足证据目标的 [`AcquisitionStrategy`]，并输出可解释的 [`AdaptDecision`]；
//! - 采集后：用 [`evaluate_capture_sufficiency`] 检查字段完备性、证据对齐度、目录出链数与正文充实度，
//!   决定是否需要升级到浏览器重采；
//! - 周期结束时：用 [`evaluate_coverage_status`] 明确区分“范围内已完整覆盖”与“预算截断/访问受限”。

use serde::{Deserialize, Serialize};

use crate::experience::FamilyStats;
use crate::render::family;

/// 自适应采集策略动作空间。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionStrategy {
    /// 携带 `If-None-Match` / `If-Modified-Since` 发起轻量条件请求。
    ConditionalHttp,
    /// 普通轻量 HTTP 抓取与正文提取。
    LightweightHttp,
    /// 轻量 HTTP 抓取并优先提取内嵌结构化数据（JSON-LD / Hydration JSON）。
    HttpWithEmbeddedJson,
    /// 先执行轻量 HTTP，仅当充足性评估未达标时升级到浏览器渲染。
    HttpVerifyBrowser,
    /// 跳过无效 HTTP 试探，直接启动浏览器渲染（仅当路径族历史证明 HTTP 几乎总是不够用且浏览器有显著增益）。
    DirectBrowser,
    /// 本周期推迟或跳过（例如剩余预算不足且优先级低，或路径族正处于高拦截率状态）。
    DeferOrSkip,
}

impl AcquisitionStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConditionalHttp => "conditional_http",
            Self::LightweightHttp => "lightweight_http",
            Self::HttpWithEmbeddedJson => "http_with_embedded_json",
            Self::HttpVerifyBrowser => "http_verify_browser",
            Self::DirectBrowser => "direct_browser",
            Self::DeferOrSkip => "defer_or_skip",
        }
    }

    pub fn allows_browser(self) -> bool {
        matches!(self, Self::HttpVerifyBrowser | Self::DirectBrowser)
    }
}

/// 策略选择输入信号。
#[derive(Debug, Clone)]
pub struct StrategyInput {
    pub url: String,
    /// 是否持有上次成功抓取的 `ETag` 或 `Last-Modified`。
    pub has_validators: bool,
    /// 是否已知该资源或路径族包含内嵌 JSON。
    pub has_embedded_json_hint: bool,
    /// 是否为种子页、目录页（Hub）或分页入口。
    pub is_hub_or_seed: bool,
    /// 资源重要性权重（默认 `1.0`，近期产出过新声明或新链接的资源更高）。
    pub importance_weight: f64,
    /// 过期比：`距上次检查时间 / 目标检查间隔`（`>= 1.0` 表示已到期）。
    pub staleness_ratio: f64,
    /// 当前周期剩余请求预算比例（`0.0..=1.0`）。
    pub remaining_budget_fraction: f64,
    /// 当前环境与调用参数是否允许使用浏览器。
    pub allow_browser: bool,
}

/// 可解释的自适应采集决策。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptDecision {
    pub family: String,
    pub strategy: AcquisitionStrategy,
    pub priority_score: f64,
    pub expected_cost_ms: f64,
    pub reasons: Vec<String>,
}

/// 根据资源信号与路径族跨周期经验选择采集策略与调度优先级。
pub fn select_acquisition_strategy(
    input: &StrategyInput,
    stats: Option<&FamilyStats>,
) -> AdaptDecision {
    let fam = stats
        .map(|s| s.family.clone())
        .unwrap_or_else(|| family(&input.url));
    let mut reasons = Vec::new();

    let yield_factor = stats.map_or(1.0, |s| 0.5 + s.claim_yield.mean());
    let hub_bonus = if input.is_hub_or_seed { 1.35 } else { 1.0 };
    let priority_score = (input.importance_weight.max(0.1)
        * input.staleness_ratio.max(0.05)
        * yield_factor
        * hub_bonus)
        .clamp(0.01, 1000.0);

    if input.remaining_budget_fraction <= 0.0 {
        reasons.push("budget_exhausted".to_string());
        return AdaptDecision {
            family: fam,
            strategy: AcquisitionStrategy::DeferOrSkip,
            priority_score,
            expected_cost_ms: 0.0,
            reasons,
        };
    }

    if let Some(st) = stats
        && st.barrier_rate.trials() >= 4.0
        && st.barrier_rate.mean() >= 0.80
        && !input.is_hub_or_seed
        && input.staleness_ratio < 3.0
    {
        reasons.push(format!(
            "family_high_barrier_rate={:.2}",
            st.barrier_rate.mean()
        ));
        if input.remaining_budget_fraction < 0.35 {
            reasons.push("defer_due_to_barrier_and_tight_budget".to_string());
            return AdaptDecision {
                family: fam,
                strategy: AcquisitionStrategy::DeferOrSkip,
                priority_score: priority_score * 0.25,
                expected_cost_ms: st.ewma_cost_ms,
                reasons,
            };
        }
    }

    if let Some(st) = stats
        && st.barrier_rate.trials() >= 4.0
        && st.barrier_rate.mean() >= 0.80
        && input.staleness_ratio >= 3.0
    {
        reasons.push("high_barrier_staleness_exploration_probe".to_string());
    }

    if input.has_validators {
        reasons.push("cached_validators_available".to_string());
        if let Some(st) = stats
            && st.conditional_hit.trials() >= 2.0
        {
            reasons.push(format!(
                "family_304_hit_rate={:.2}",
                st.conditional_hit.mean()
            ));
        }
        return AdaptDecision {
            family: fam,
            strategy: AcquisitionStrategy::ConditionalHttp,
            priority_score,
            expected_cost_ms: stats.map_or(25.0, |s| (s.ewma_cost_ms * 0.5).max(10.0)),
            reasons,
        };
    }

    let learned_embedded_json =
        stats.is_some_and(|st| st.embedded_json.trials() >= 2.0 && st.embedded_json.mean() >= 0.60);
    let prefer_embedded_json = input.has_embedded_json_hint || learned_embedded_json;

    if let Some(st) = stats
        && st.http_sufficient.trials() >= 3.0
    {
        let http_ok = st.http_sufficient.mean();
        let b_gain = st.browser_gain.mean();
        if input.allow_browser && http_ok < 0.25 && st.browser_gain.trials() >= 2.0 && b_gain > 0.60
        {
            reasons.push(format!("family_http_sufficient_low={http_ok:.2}"));
            reasons.push(format!("family_browser_gain_high={b_gain:.2}"));
            return AdaptDecision {
                family: fam,
                strategy: AcquisitionStrategy::DirectBrowser,
                priority_score,
                expected_cost_ms: st.ewma_cost_ms.max(450.0),
                reasons,
            };
        }
        if http_ok >= 0.80 {
            reasons.push(format!("family_http_sufficient_high={http_ok:.2}"));
            let strat = if prefer_embedded_json {
                reasons.push(if input.has_embedded_json_hint {
                    "embedded_json_hint".to_string()
                } else {
                    "learned_embedded_json_family".to_string()
                });
                AcquisitionStrategy::HttpWithEmbeddedJson
            } else {
                AcquisitionStrategy::LightweightHttp
            };
            return AdaptDecision {
                family: fam,
                strategy: strat,
                priority_score,
                expected_cost_ms: st.ewma_cost_ms.max(35.0),
                reasons,
            };
        }
    }

    if prefer_embedded_json {
        reasons.push(if input.has_embedded_json_hint {
            "embedded_json_hint".to_string()
        } else {
            "learned_embedded_json_family".to_string()
        });
        return AdaptDecision {
            family: fam,
            strategy: AcquisitionStrategy::HttpWithEmbeddedJson,
            priority_score,
            expected_cost_ms: stats.map_or(45.0, |s| s.ewma_cost_ms.max(35.0)),
            reasons,
        };
    }

    let strategy = if input.allow_browser {
        reasons.push("cold_start_http_with_browser_verification".to_string());
        AcquisitionStrategy::HttpVerifyBrowser
    } else {
        reasons.push("http_only_constraint".to_string());
        AcquisitionStrategy::LightweightHttp
    };

    AdaptDecision {
        family: fam,
        strategy,
        priority_score,
        expected_cost_ms: stats.map_or(60.0, |s| s.ewma_cost_ms.max(40.0)),
        reasons,
    }
}

/// 任务级采集充足性目标。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureGoal {
    pub min_markdown_chars: usize,
    pub required_fields: Vec<String>,
    pub require_evidence_grounded: bool,
    pub expect_links_for_hub: bool,
}

impl Default for CaptureGoal {
    fn default() -> Self {
        Self {
            min_markdown_chars: 64,
            required_fields: Vec::new(),
            require_evidence_grounded: false,
            expect_links_for_hub: false,
        }
    }
}

/// 单页抓取后的质量与证据信号。
#[derive(Debug, Clone, Default)]
pub struct CaptureSignals {
    pub status_code: u16,
    pub not_modified_304: bool,
    /// 仅在 `not_modified_304 == true` 时检查：已知权威 baseline 是否与当前任务契约一致且已满足质量门。
    pub baseline_matches_contract: bool,
    pub markdown_chars: usize,
    pub has_embedded_json: bool,
    pub has_client_shell_markers: bool,
    pub extracted_fields_present: Vec<String>,
    /// 经逐字段 `EvidenceSpan` 与字段值一致对齐的字段名集合。
    pub grounded_fields_present: Vec<String>,
    pub grounded_evidence_count: usize,
    pub unaligned_claim_count: usize,
    pub outgoing_links_count: usize,
    pub is_barrier: bool,
}

/// 证据驱动的采集充足性评估结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureAssessment {
    pub sufficient: bool,
    pub needs_browser_escalation: bool,
    pub confidence: f64,
    pub missing_fields: Vec<String>,
    #[serde(default)]
    pub ungrounded_fields: Vec<String>,
    pub reasons: Vec<String>,
}

/// 评估一次轻量采集结果是否已经满足任务目标，以及是否需要升级到浏览器重采。
pub fn evaluate_capture_sufficiency(
    goal: &CaptureGoal,
    signals: &CaptureSignals,
) -> CaptureAssessment {
    if signals.not_modified_304 {
        if signals.baseline_matches_contract {
            return CaptureAssessment {
                sufficient: true,
                needs_browser_escalation: false,
                confidence: 1.0,
                missing_fields: Vec::new(),
                ungrounded_fields: Vec::new(),
                reasons: vec!["http_304_not_modified".to_string()],
            };
        }
        return CaptureAssessment {
            sufficient: false,
            needs_browser_escalation: false,
            confidence: 0.25,
            missing_fields: goal.required_fields.clone(),
            ungrounded_fields: Vec::new(),
            reasons: vec!["http_304_baseline_contract_mismatch".to_string()],
        };
    }

    if signals.is_barrier {
        return CaptureAssessment {
            sufficient: false,
            needs_browser_escalation: false,
            confidence: 0.0,
            missing_fields: goal.required_fields.clone(),
            ungrounded_fields: Vec::new(),
            reasons: vec!["barrier_detected".to_string()],
        };
    }

    if signals.status_code >= 400 || signals.status_code == 0 {
        return CaptureAssessment {
            sufficient: false,
            needs_browser_escalation: false,
            confidence: 0.0,
            missing_fields: goal.required_fields.clone(),
            ungrounded_fields: Vec::new(),
            reasons: vec![format!("http_error_status_{}", signals.status_code)],
        };
    }

    let mut reasons = Vec::new();
    let missing_fields: Vec<String> = goal
        .required_fields
        .iter()
        .filter(|req| !signals.extracted_fields_present.iter().any(|p| p == *req))
        .cloned()
        .collect();

    if !missing_fields.is_empty() {
        reasons.push(format!(
            "missing_required_fields={}",
            missing_fields.join(",")
        ));
    }

    let ungrounded_fields: Vec<String> = if goal.require_evidence_grounded {
        goal.required_fields
            .iter()
            .filter(|req| {
                signals.extracted_fields_present.iter().any(|p| p == *req)
                    && !signals.grounded_fields_present.iter().any(|g| g == *req)
            })
            .cloned()
            .collect()
    } else {
        Vec::new()
    };

    if goal.require_evidence_grounded {
        if !ungrounded_fields.is_empty() {
            reasons.push(format!(
                "ungrounded_required_fields={}",
                ungrounded_fields.join(",")
            ));
        }
        if signals.grounded_evidence_count == 0 {
            reasons.push("no_grounded_evidence_for_extracted_fields".to_string());
        }
        if signals.unaligned_claim_count > 0 {
            reasons.push(format!(
                "unaligned_claims={}",
                signals.unaligned_claim_count
            ));
        }
    }

    if goal.expect_links_for_hub && signals.outgoing_links_count == 0 {
        reasons.push("hub_missing_outgoing_links".to_string());
    }

    let empty_without_claims = signals.markdown_chars == 0
        && signals.grounded_evidence_count == 0
        && signals.extracted_fields_present.is_empty();
    let content_thin = (signals.markdown_chars < goal.min_markdown_chars
        && !signals.has_embedded_json)
        || empty_without_claims;
    if content_thin {
        reasons.push(format!(
            "thin_markdown_chars={}<{}",
            signals.markdown_chars, goal.min_markdown_chars
        ));
    }
    if signals.has_client_shell_markers {
        reasons.push("client_shell_markers_detected".to_string());
    }

    let grounding_ok = !goal.require_evidence_grounded
        || (ungrounded_fields.is_empty()
            && signals.unaligned_claim_count == 0
            && signals.grounded_evidence_count > 0);

    let sufficient = missing_fields.is_empty()
        && grounding_ok
        && (!goal.expect_links_for_hub || signals.outgoing_links_count > 0)
        && !content_thin
        && !signals.has_client_shell_markers;

    let needs_browser_escalation = !sufficient
        && (signals.has_client_shell_markers
            || content_thin
            || !missing_fields.is_empty()
            || !ungrounded_fields.is_empty()
            || (goal.expect_links_for_hub && signals.outgoing_links_count == 0)
            || !grounding_ok);

    let confidence = if sufficient {
        if signals.grounded_evidence_count > 0 || signals.has_embedded_json {
            0.95
        } else {
            0.85
        }
    } else {
        0.30
    };

    if sufficient && reasons.is_empty() {
        reasons.push("capture_goal_satisfied".to_string());
    }

    CaptureAssessment {
        sufficient,
        needs_browser_escalation,
        confidence,
        missing_fields,
        ungrounded_fields,
        reasons,
    }
}

/// 同步周期或发现任务的覆盖完整性状态。
#[path = "strategy_coverage.rs"]
mod strategy_coverage;
pub use strategy_coverage::{
    CoverageInput, CoverageReport, CoverageStatus, evaluate_coverage_status,
};

#[cfg(test)]
#[path = "strategy_tests.rs"]
mod tests;
