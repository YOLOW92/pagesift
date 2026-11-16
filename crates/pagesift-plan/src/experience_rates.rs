//! Snapshot distributions for per-family capture, regularity, and change-rate learning.

use pagesift_stat::beta::{BetaCounts, Prior};
use pagesift_stat::poisson::ChangeRate;
use pagesift_stat::regularity::{
    PeriodEstimate, estimate_period, next_periodic_delay_secs, non_aliased_change_interval_secs,
};
use serde::{Deserialize, Serialize};

/// 单次采集对路径族经验的观测输入（G1 Observe：区分有效、删失、失败与模态成本）。
#[derive(Debug, Clone, Default)]
pub struct FamilyObservation {
    pub observed_at_ms: u64,
    /// 仅当实际发起轻量 HTTP 采集且无传输层/解析不可靠警告时有值；
    /// 直接走 Browser 或遇不可靠观测时应传 `None`（按删失处理）。
    pub http_sufficient: Option<bool>,
    /// 仅当同一资源实际触发浏览器升级对比且浏览器采集本身有效时有值。
    pub browser_gain: Option<bool>,
    /// 仅当携带 `ETag`/`Last-Modified` 发起条件请求且确认命中真实 304 时有值。
    pub conditional_hit: Option<bool>,
    /// 仅当实际检查 HTTP HTML 文档时反馈是否发现并有效使用了 `__NEXT_DATA__`/`__NUXT_DATA__`/JSON-LD。
    pub embedded_json_observed: Option<bool>,
    /// 距上次有效基线的间隔（秒）及是否发生实体/实质内容变化；
    /// 304（`false`）直接参与速率更新，不可靠编码或传输失败按 `None`（删失）处理。
    pub substantive_change: Option<(f64, bool)>,
    /// 本次有效抽取是否产出了实体声明或新链接。
    pub yielded_claims_or_links: Option<bool>,
    /// 本次是否命中拦截壁垒或传输失败（`Some(true)`）。
    pub encountered_barrier: Option<bool>,
    /// 本次观测综合耗时（ms）。
    pub cost_ms: f64,
    /// 本次观测是否带有 `decode:*` 不确定解码 / `body_truncated` / 无效 304 警告。
    pub unreliable_observation: bool,
    /// 本次采集是否实际动用了浏览器渲染执行路径。
    pub used_browser: bool,
    /// 本次 HTTP 请求是否为省流量的 304 Not Modified 命中。
    pub is_304: bool,
    /// 分离模态耗时：纯 HTTP 耗时（ms）。
    pub http_cost_ms: Option<f64>,
    /// 分离模态耗时：纯 Browser 渲染耗时（ms）。
    pub browser_cost_ms: Option<f64>,
    /// 浏览器执行是否发生超时/屏障/无增益降级失败（与策略选择脱敏）。
    pub browser_failed: Option<bool>,
    /// 服务端报告的精确修改时间戳（ms），可用于辅助非混叠周期判定。
    pub last_modified_at_ms: Option<u64>,
}

impl FamilyObservation {
    /// Returns whether this observation carries at least one non-censored signal.
    pub fn has_signal(&self) -> bool {
        self.http_sufficient.is_some()
            || self.browser_gain.is_some()
            || self.conditional_hit.is_some()
            || self.embedded_json_observed.is_some()
            || self.substantive_change.is_some()
            || self.yielded_claims_or_links.is_some()
            || self.encountered_barrier.is_some()
            || self.browser_failed.is_some()
    }
}

/// 可序列化的 Beta 计数快照。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BetaSnapshot {
    pub success: f64,
    pub failure: f64,
}

impl From<BetaCounts> for BetaSnapshot {
    fn from(c: BetaCounts) -> Self {
        Self {
            success: c.success,
            failure: c.failure,
        }
    }
}

impl From<BetaSnapshot> for BetaCounts {
    fn from(s: BetaSnapshot) -> Self {
        Self {
            success: s.success.max(0.0),
            failure: s.failure.max(0.0),
        }
    }
}

impl BetaSnapshot {
    pub fn observe_decayed(&mut self, ok: bool, keep: f64) {
        self.success *= keep;
        self.failure *= keep;
        if ok {
            self.success += 1.0;
        } else {
            self.failure += 1.0;
        }
    }

    /// 带贝叶斯预测惊异的遗忘更新：当先验已高度确信且连续与现实矛盾时加速退火，
    /// 使 SSR<->SPA 体制切换在 2~4 轮内翻越决策阈值，返回本次是否触发惊异遗忘。
    pub fn observe_with_surprise(&mut self, ok: bool, keep: f64, surprise_keep: f64) -> bool {
        let trials = self.trials();
        let mean = self.mean();
        let surprised = trials >= 4.0 && ((ok && mean < 0.28) || (!ok && mean > 0.72));
        let eff_keep = if surprised {
            surprise_keep.min(keep)
        } else {
            keep
        };
        self.observe_decayed(ok, eff_keep);
        surprised
    }

    pub fn merge(&mut self, other: &Self) {
        self.success += other.success;
        self.failure += other.failure;
    }

    pub fn mean(self) -> f64 {
        BetaCounts::from(self).posterior(Prior::UNIFORM).mean()
    }

    pub fn trials(self) -> f64 {
        self.success + self.failure
    }
}

/// 非混叠周期更新模型持久化状态（委托 [`pagesift_stat::regularity`] 共享无依赖数学）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegularitySnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_change_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_confirmed_unchanged_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intervals_secs: Vec<f64>,
    #[serde(default)]
    pub probe_misses: u8,
    #[serde(default)]
    pub early_changes: u8,
}

impl RegularitySnapshot {
    pub const MAX_CV: f64 = 0.22;

    pub fn detected_period(&self) -> Option<PeriodEstimate> {
        if self.probe_misses >= 2 || self.early_changes >= 2 {
            return None;
        }
        estimate_period(&self.intervals_secs, Self::MAX_CV)
    }

    pub fn observe(
        &mut self,
        observed_at_ms: u64,
        changed: bool,
        last_modified_at_ms: Option<u64>,
    ) {
        if observed_at_ms == 0 {
            return;
        }
        let existing = self.detected_period();
        if changed {
            let effective_ts = last_modified_at_ms
                .filter(|&m| m <= observed_at_ms.saturating_add(60_000))
                .unwrap_or(observed_at_ms);
            if let Some(prev) = self.last_change_at_ms
                && effective_ts > prev
            {
                let raw_dt_secs = (effective_ts - prev) as f64 / 1000.0;
                if let Some(est) = existing
                    && raw_dt_secs < est.mean_secs * 0.55
                {
                    self.early_changes = self.early_changes.saturating_add(1);
                    if self.early_changes >= 2 {
                        self.intervals_secs.clear();
                    }
                } else if let Some(valid_dt) = non_aliased_change_interval_secs(
                    prev,
                    self.last_confirmed_unchanged_at_ms,
                    observed_at_ms,
                    last_modified_at_ms,
                    existing,
                ) {
                    self.intervals_secs.push(valid_dt);
                    if self.intervals_secs.len() > 8 {
                        let drain = self.intervals_secs.len() - 8;
                        self.intervals_secs.drain(0..drain);
                    }
                    self.probe_misses = 0;
                    self.early_changes = 0;
                }
            }
            self.last_change_at_ms = Some(effective_ts);
        } else {
            self.last_confirmed_unchanged_at_ms = Some(observed_at_ms);
            if let (Some(est), Some(prev)) = (existing, self.last_change_at_ms) {
                let elapsed_secs = observed_at_ms.saturating_sub(prev) as f64 / 1000.0;
                let guard = (2.0 * est.std_secs).clamp(5.0, (est.mean_secs * 0.15).max(5.0));
                if elapsed_secs >= est.mean_secs + guard {
                    self.probe_misses = self.probe_misses.saturating_add(1);
                    if self.probe_misses >= 2 {
                        self.intervals_secs.clear();
                    }
                }
            }
        }
    }

    pub fn next_delay_secs(&self, now_ms: u64, min_secs: f64, max_secs: f64) -> Option<f64> {
        let est = self.detected_period()?;
        let last_chg = self.last_change_at_ms?;
        let elapsed_secs = now_ms.saturating_sub(last_chg) as f64 / 1000.0;
        Some(next_periodic_delay_secs(
            &est,
            elapsed_secs,
            min_secs,
            max_secs,
        ))
    }

    pub fn merge(&mut self, other: &Self) {
        if other.last_change_at_ms.unwrap_or(0) > self.last_change_at_ms.unwrap_or(0) {
            self.last_change_at_ms = other.last_change_at_ms;
        }
        if other.last_confirmed_unchanged_at_ms.unwrap_or(0)
            > self.last_confirmed_unchanged_at_ms.unwrap_or(0)
        {
            self.last_confirmed_unchanged_at_ms = other.last_confirmed_unchanged_at_ms;
        }
        if self.intervals_secs.is_empty() && !other.intervals_secs.is_empty() {
            self.intervals_secs = other.intervals_secs.clone();
        }
    }
}

/// 可序列化的泊松变化率快照（支持双向惊异遗忘与非混叠规律周期模型）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRateSnapshot {
    pub shape: f64,
    pub exposure_secs: f64,
    pub changed_intervals_secs: Vec<f64>,
    #[serde(default)]
    pub unchanged_streak: u32,
    #[serde(default)]
    pub quiet_streak_secs: f64,
    #[serde(default)]
    pub drift_events: u32,
    #[serde(default)]
    pub regularity: RegularitySnapshot,
}

impl Default for ChangeRateSnapshot {
    fn default() -> Self {
        let cr = ChangeRate::default();
        Self {
            shape: cr.shape,
            exposure_secs: cr.exposure,
            changed_intervals_secs: cr.changed,
            unchanged_streak: 0,
            quiet_streak_secs: 0.0,
            drift_events: 0,
            regularity: RegularitySnapshot::default(),
        }
    }
}

impl ChangeRateSnapshot {
    pub fn to_change_rate(&self) -> ChangeRate {
        ChangeRate {
            shape: self.shape.max(1e-6),
            exposure: self.exposure_secs.max(1.0),
            changed: self.changed_intervals_secs.clone(),
        }
    }

    pub fn observe(&mut self, dt_secs: f64, substantive_changed: bool) {
        let _ = self.observe_at(dt_secs, substantive_changed, 0, None);
    }

    /// 记录带观测时间戳的变更样本，返回本次是否因预测惊异触发了变点遗忘。
    pub fn observe_at(
        &mut self,
        dt_secs: f64,
        substantive_changed: bool,
        observed_at_ms: u64,
        last_modified_at_ms: Option<u64>,
    ) -> bool {
        if !(dt_secs > 0.0 && dt_secs.is_finite()) {
            return false;
        }
        const MAX_EXPOSURE_SECS: f64 = 365.0 * 86_400.0;
        let default_exp = ChangeRate::default().exposure;
        let mut drifted = false;

        if substantive_changed {
            let expected_in_dt = self.to_change_rate().mean() * dt_secs;
            // 静态期突发高惊异变更（预期 < 0.15 次）：更激进地遗忘历史静默曝光，2~3 轮内抬高 λ
            let surprise_wake = self.unchanged_streak >= 3 && expected_in_dt < 0.15;
            if surprise_wake {
                drifted = true;
                self.drift_events = self.drift_events.saturating_add(1);
            }
            let decay = if surprise_wake {
                0.20
            } else if !self.changed_intervals_secs.is_empty() {
                0.50
            } else {
                0.85
            };
            self.unchanged_streak = 0;
            self.quiet_streak_secs = 0.0;
            let extra = (self.exposure_secs - default_exp).max(0.0) * decay;
            self.exposure_secs = (default_exp + extra).clamp(1.0, MAX_EXPOSURE_SECS);
            self.changed_intervals_secs.push(dt_secs);
            if self.changed_intervals_secs.len() > 32 {
                let drain = self.changed_intervals_secs.len() - 32;
                self.changed_intervals_secs.drain(0..drain);
            }
        } else {
            self.unchanged_streak = self.unchanged_streak.saturating_add(1);
            self.quiet_streak_secs = (self.quiet_streak_secs + dt_secs).min(MAX_EXPOSURE_SECS);
            self.exposure_secs = (self.exposure_secs + dt_secs).min(MAX_EXPOSURE_SECS);

            if !self.changed_intervals_secs.is_empty() {
                // 预测证伪概率 p_quiet = (hist_exp / (hist_exp + quiet_streak))^k_hist
                let hist_exp = (self.exposure_secs - self.quiet_streak_secs).max(1.0);
                let hist_k = (self.changed_intervals_secs.len() as f64).min(12.0);
                let p_quiet = (hist_exp / (hist_exp + self.quiet_streak_secs)).powf(hist_k);
                if p_quiet < 0.02 && self.unchanged_streak >= 2 {
                    // 强预测证伪：立刻淘汰半数以上过期加速区间，无需拖延数十轮
                    let drop_n = self
                        .changed_intervals_secs
                        .len()
                        .div_ceil(2)
                        .max(1)
                        .min(self.changed_intervals_secs.len());
                    self.changed_intervals_secs.drain(0..drop_n);
                    self.drift_events = self.drift_events.saturating_add(1);
                    drifted = true;
                } else if p_quiet < 0.10 && self.unchanged_streak >= 2 {
                    self.changed_intervals_secs.remove(0);
                    self.drift_events = self.drift_events.saturating_add(1);
                    drifted = true;
                } else if self.unchanged_streak >= 3 && self.unchanged_streak % 2 == 1 {
                    self.changed_intervals_secs.remove(0);
                }
            }
        }

        if observed_at_ms > 0 {
            self.regularity
                .observe(observed_at_ms, substantive_changed, last_modified_at_ms);
        }
        drifted
    }

    pub fn merge(&mut self, other: &Self) {
        let default_exp = ChangeRate::default().exposure;
        let added_exp = (other.exposure_secs - default_exp).max(0.0);
        self.exposure_secs = (self.exposure_secs + added_exp).min(365.0 * 86_400.0);
        for &dt in &other.changed_intervals_secs {
            if self.changed_intervals_secs.len() < 32 {
                self.changed_intervals_secs.push(dt);
            }
        }
        self.unchanged_streak = self.unchanged_streak.min(other.unchanged_streak);
        self.quiet_streak_secs = self.quiet_streak_secs.min(other.quiet_streak_secs);
        self.drift_events = self.drift_events.max(other.drift_events);
        self.regularity.merge(&other.regularity);
    }
}
