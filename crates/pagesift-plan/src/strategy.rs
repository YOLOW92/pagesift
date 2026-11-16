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
    /// 当前周期剩余总请求预算比例（`0.0..=1.0`）。
    pub remaining_budget_fraction: f64,
    /// 当前环境与调用参数是否允许使用浏览器。
    pub allow_browser: bool,
    /// 当前周期剩余浏览器名额比例（`0.0..=1.0`；默认 `1.0`）。
    pub browser_budget_fraction: f64,
    /// 当前实时浏览器标签占池上限利用率（`0.0..=1.0`；默认 `0.0`）。
    pub browser_tab_utilization: f64,
    /// 调度器允许在长期 `DirectBrowser` 后触发一次低成本 HTTP 回探验证 SSR 回迁。
    pub probe_recovery_eligible: bool,
}

impl Default for StrategyInput {
    fn default() -> Self {
        Self {
            url: String::new(),
            has_validators: false,
            has_embedded_json_hint: false,
            is_hub_or_seed: false,
            importance_weight: 1.0,
            staleness_ratio: 1.0,
            remaining_budget_fraction: 1.0,
            allow_browser: false,
            browser_budget_fraction: 1.0,
            browser_tab_utilization: 0.0,
            probe_recovery_eligible: true,
        }
    }
}

/// 可解释的自适应采集决策。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptDecision {
    pub family: String,
    pub strategy: AcquisitionStrategy,
    pub priority_score: f64,
    pub expected_cost_ms: f64,
    #[serde(default)]
    pub expected_value: f64,
    #[serde(default)]
    pub shadow_price: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reuse_source: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exploration_probe: bool,
    /// 执行层约束：若 HTTP 采集不充分，本策略决策是否允许升级浏览器；因高影子价格卸载、禁止或耗尽时为 `false`。
    #[serde(default)]
    pub browser_escalation_permitted: bool,
    pub reasons: Vec<String>,
}

const DIRECT_BROWSER_RECOVERY_PERIOD: u32 = 6;

fn browser_shadow_price(input: &StrategyInput) -> f64 {
    let budget_tight = (1.0 - input.browser_budget_fraction.clamp(0.0, 1.0)).powi(2);
    let tab_pressure = input.browser_tab_utilization.clamp(0.0, 1.0).powi(2);
    1.0 + 3.0 * budget_tight + 2.5 * tab_pressure
}

struct DecisionCtx<'a> {
    family: String,
    input: &'a StrategyInput,
    stats: Option<&'a FamilyStats>,
}

fn finish_decision(
    ctx: DecisionCtx<'_>,
    strategy: AcquisitionStrategy,
    priority_score: f64,
    exploration_probe: bool,
    browser_escalation_permitted: bool,
    reasons: Vec<String>,
) -> AdaptDecision {
    let fam = ctx.family;
    let input = ctx.input;
    let stats = ctx.stats;
    let shadow_price = browser_shadow_price(input);
    let expected_cost_ms = match strategy {
        AcquisitionStrategy::DeferOrSkip => 0.0,
        other => stats.map_or(
            match other {
                AcquisitionStrategy::ConditionalHttp => 25.0,
                AcquisitionStrategy::LightweightHttp => 45.0,
                AcquisitionStrategy::HttpWithEmbeddedJson => 45.0,
                AcquisitionStrategy::HttpVerifyBrowser => 140.0,
                AcquisitionStrategy::DirectBrowser => 550.0,
                AcquisitionStrategy::DeferOrSkip => 0.0,
            },
            |s| s.expected_cost_for(other),
        ),
    };
    let yield_prob = stats.map_or(0.65, |s| s.claim_yield.mean().clamp(0.2, 1.0));
    let pass_prob = stats.map_or(0.85, |s| (1.0 - s.barrier_rate.mean()).clamp(0.1, 1.0));
    let raw_gain = input.importance_weight.max(0.1)
        * input.staleness_ratio.clamp(0.25, 4.0)
        * (if input.is_hub_or_seed { 1.35 } else { 1.0 })
        * yield_prob
        * pass_prob;
    let effective_cost_penalty = if strategy.allows_browser() {
        (expected_cost_ms / 500.0) * shadow_price
    } else {
        expected_cost_ms / 500.0
    };
    let expected_value = match strategy {
        AcquisitionStrategy::DeferOrSkip => 0.0,
        _ => (raw_gain - 0.12 * effective_cost_penalty).max(-1.0),
    };
    let reuse_source = stats.and_then(|s| {
        if !s.transfer.rejected {
            s.transfer.donor_description.clone()
        } else {
            None
        }
    });
    AdaptDecision {
        family: fam,
        strategy,
        priority_score,
        expected_cost_ms,
        expected_value,
        shadow_price,
        reuse_source,
        exploration_probe,
        browser_escalation_permitted,
        reasons,
    }
}

/// 根据资源信号、跨周期经验与资源影子价格选择采集策略与调度优先级。
pub fn select_acquisition_strategy(
    input: &StrategyInput,
    stats: Option<&FamilyStats>,
) -> AdaptDecision {
    let fam = stats
        .map(|s| s.family.clone())
        .unwrap_or_else(|| family(&input.url));
    let mut reasons = Vec::new();
    let shadow_price = browser_shadow_price(input);

    if let Some(st) = stats
        && !st.transfer.rejected
        && let Some(ref donor) = st.transfer.donor_description
    {
        reasons.push(format!("transfer_seed={donor}"));
    }

    let yield_factor = stats.map_or(1.0, |s| 0.5 + s.claim_yield.mean());
    let hub_bonus = if input.is_hub_or_seed { 1.35 } else { 1.0 };
    let priority_score = (input.importance_weight.max(0.1)
        * input.staleness_ratio.max(0.05)
        * yield_factor
        * hub_bonus)
        .clamp(0.01, 1000.0);

    if input.remaining_budget_fraction <= 0.0 {
        reasons.push("budget_exhausted".to_string());
        return finish_decision(
            DecisionCtx {
                family: fam,
                input,
                stats,
            },
            AcquisitionStrategy::DeferOrSkip,
            priority_score,
            false,
            false,
            reasons,
        );
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
            return finish_decision(
                DecisionCtx {
                    family: fam,
                    input,
                    stats,
                },
                AcquisitionStrategy::DeferOrSkip,
                priority_score * 0.25,
                false,
                false,
                reasons,
            );
        }
    }

    if let Some(st) = stats
        && st.barrier_rate.trials() >= 4.0
        && st.barrier_rate.mean() >= 0.80
        && input.staleness_ratio >= 3.0
    {
        reasons.push("high_barrier_staleness_exploration_probe".to_string());
    }

    let learned_embedded_json =
        stats.is_some_and(|st| st.embedded_json.trials() >= 2.0 && st.embedded_json.mean() >= 0.55);
    let prefer_embedded_json = input.has_embedded_json_hint || learned_embedded_json;
    let browser_allowed_under_budget = input.allow_browser && input.browser_budget_fraction > 0.0;
    let browser_fail_rate = stats
        .filter(|st| st.browser_failure_rate.trials() >= 3.0)
        .map_or(0.0, |st| st.browser_failure_rate.mean());

    // 先检查已确认的强 SPA 依赖族：避免空 HTML 壳恰好携带 ETag 时被 ConditionalHttp 304 永久锁定而不进入 DirectBrowser / 恢复探测
    if let Some(st) = stats
        && st.http_sufficient.trials() >= 2.5
    {
        let http_ok = st.http_sufficient.mean();
        let b_gain = st.browser_gain.mean();

        if browser_allowed_under_budget
            && http_ok < 0.25
            && st.browser_gain.trials() >= 2.0
            && b_gain > 0.60
        {
            if (input.probe_recovery_eligible
                && (st.consecutive_direct_browser >= DIRECT_BROWSER_RECOVERY_PERIOD
                    || st.probe_positive_streak > 0))
                || input.has_embedded_json_hint
                || browser_fail_rate >= 0.65
            {
                reasons.push("voi_probe_direct_browser_recovery".to_string());
                if st.probe_positive_streak > 0 {
                    reasons.push(format!(
                        "recovery_positive_streak={}",
                        st.probe_positive_streak
                    ));
                }
                if browser_fail_rate >= 0.65 {
                    reasons.push(format!("high_browser_failure_rate={browser_fail_rate:.2}"));
                }
                return finish_decision(
                    DecisionCtx {
                        family: fam,
                        input,
                        stats,
                    },
                    AcquisitionStrategy::HttpVerifyBrowser,
                    priority_score,
                    true,
                    true,
                    reasons,
                );
            }
            // 在浏览器配额/标签池极度紧绷（影子价格高）且具备可降级路径或边际压力时，真实禁止浏览器升阶
            if shadow_price >= 3.8 && prefer_embedded_json {
                reasons.push(format!("browser_shadow_price_offload={shadow_price:.2}"));
                return finish_decision(
                    DecisionCtx {
                        family: fam,
                        input,
                        stats,
                    },
                    AcquisitionStrategy::HttpWithEmbeddedJson,
                    priority_score,
                    false,
                    false,
                    reasons,
                );
            }
            reasons.push(format!("family_http_sufficient_low={http_ok:.2}"));
            reasons.push(format!("family_browser_gain_high={b_gain:.2}"));
            return finish_decision(
                DecisionCtx {
                    family: fam,
                    input,
                    stats,
                },
                AcquisitionStrategy::DirectBrowser,
                priority_score,
                false,
                false,
                reasons,
            );
        }
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
        let shadow_offloaded = shadow_price >= 3.5
            && stats
                .is_some_and(|st| st.browser_gain.trials() >= 2.0 && st.browser_gain.mean() < 0.55);
        if shadow_offloaded {
            reasons.push(format!(
                "shadow_price_suppresses_browser_escalation={shadow_price:.2}"
            ));
        }
        return finish_decision(
            DecisionCtx {
                family: fam,
                input,
                stats,
            },
            AcquisitionStrategy::ConditionalHttp,
            priority_score,
            false,
            browser_allowed_under_budget && browser_fail_rate < 0.75 && !shadow_offloaded,
            reasons,
        );
    }

    if let Some(st) = stats
        && st.http_sufficient.trials() >= 2.5
    {
        let http_ok = st.http_sufficient.mean();
        if http_ok >= 0.78 || (shadow_price >= 3.5 && http_ok >= 0.55) {
            reasons.push(format!("family_http_sufficient_high={http_ok:.2}"));
            let shadow_offloaded = shadow_price >= 3.5;
            if shadow_price >= 3.5 && http_ok < 0.78 {
                reasons.push(format!("high_shadow_price_avoid_browser={shadow_price:.2}"));
            }
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
            return finish_decision(
                DecisionCtx {
                    family: fam,
                    input,
                    stats,
                },
                strat,
                priority_score,
                false,
                browser_allowed_under_budget && browser_fail_rate < 0.75 && !shadow_offloaded,
                reasons,
            );
        }
    }

    if prefer_embedded_json {
        reasons.push(if input.has_embedded_json_hint {
            "embedded_json_hint".to_string()
        } else {
            "learned_embedded_json_family".to_string()
        });
        let shadow_offloaded = shadow_price >= 3.8;
        if shadow_offloaded {
            reasons.push(format!(
                "shadow_price_offload_embedded_json={shadow_price:.2}"
            ));
        }
        return finish_decision(
            DecisionCtx {
                family: fam,
                input,
                stats,
            },
            AcquisitionStrategy::HttpWithEmbeddedJson,
            priority_score,
            false,
            browser_allowed_under_budget && browser_fail_rate < 0.75 && !shadow_offloaded,
            reasons,
        );
    }

    let (strategy, allow_esc) = if browser_allowed_under_budget && browser_fail_rate < 0.75 {
        if shadow_price >= 4.2 && !input.is_hub_or_seed && input.staleness_ratio < 1.5 {
            reasons.push(format!("shadow_price_prefer_lightweight={shadow_price:.2}"));
            (AcquisitionStrategy::LightweightHttp, false)
        } else {
            reasons.push("cold_start_http_with_browser_verification".to_string());
            (AcquisitionStrategy::HttpVerifyBrowser, true)
        }
    } else {
        reasons.push(if !browser_allowed_under_budget {
            "http_only_or_browser_budget_exhausted".to_string()
        } else {
            "browser_failure_fallback_http".to_string()
        });
        (AcquisitionStrategy::LightweightHttp, false)
    };

    finish_decision(
        DecisionCtx {
            family: fam,
            input,
            stats,
        },
        strategy,
        priority_score,
        false,
        allow_esc,
        reasons,
    )
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
