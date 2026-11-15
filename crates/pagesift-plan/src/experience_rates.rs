//! Snapshot distributions for per-family capture and change-rate learning.

use pagesift_stat::beta::{BetaCounts, Prior};
use pagesift_stat::poisson::ChangeRate;
use serde::{Deserialize, Serialize};

/// 单次采集对路径族经验的观测输入。
#[derive(Debug, Clone)]
pub struct FamilyObservation {
    pub observed_at_ms: u64,
    /// 仅当实际发起轻量 HTTP 采集（含 Conditional 200 / EmbeddedJson）且响应可用时，
    /// 记录 HTTP 是否满足任务评估；未执行 HTTP（如 `DirectBrowser`、`304` 或错误）时为 `None`。
    pub http_sufficient: Option<bool>,
    /// 若实际尝试了浏览器渲染（含失败/超时），浏览器是否带来正文或结构化声明增益。
    pub browser_gain: Option<bool>,
    /// 若发送了条件请求（`If-None-Match` / `If-Modified-Since`）且未发生传输错误，是否命中 304。
    pub conditional_hit: Option<bool>,
    /// 若执行了 HTTP 内容提取，是否观测到内嵌结构化 JSON（JSON-LD / Hydration JSON）。
    pub embedded_json_observed: Option<bool>,
    /// 距上次权威观测的时间间隔（秒）及是否发生**实质内容变化**（非权威错误/阻塞传 `None` 做删失处理）。
    pub substantive_change: Option<(f64, bool)>,
    /// 本次权威采集是否产出至少 1 条结构化声明或新发现链接（非 body 响应如 304/错误为 `None`）。
    pub yielded_claims_or_links: Option<bool>,
    /// 本次采集是否遭遇拦截（403 / 429 / 验证码挑战）。
    pub encountered_barrier: bool,
    /// 本次采集实际花费的总耗时（毫秒）。
    pub cost_ms: f64,
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

/// 可序列化的泊松变化率快照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRateSnapshot {
    pub shape: f64,
    pub exposure_secs: f64,
    pub changed_intervals_secs: Vec<f64>,
    #[serde(default)]
    pub unchanged_streak: u32,
}

impl Default for ChangeRateSnapshot {
    fn default() -> Self {
        let cr = ChangeRate::default();
        Self {
            shape: cr.shape,
            exposure_secs: cr.exposure,
            changed_intervals_secs: cr.changed,
            unchanged_streak: 0,
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
        if !(dt_secs > 0.0 && dt_secs.is_finite()) {
            return;
        }
        const MAX_EXPOSURE_SECS: f64 = 365.0 * 86_400.0;
        let default_exp = ChangeRate::default().exposure;
        if substantive_changed {
            self.unchanged_streak = 0;
            // 站点由长期静态切换为频繁变化时，衰减旧静默曝光，使后验速率在数次变化内完成重学习（R09）
            let decay = if !self.changed_intervals_secs.is_empty() {
                0.65
            } else {
                0.85
            };
            let extra = (self.exposure_secs - default_exp).max(0.0) * decay;
            self.exposure_secs = (default_exp + extra).clamp(1.0, MAX_EXPOSURE_SECS);
            self.changed_intervals_secs.push(dt_secs);
            // 保留最近 32 个变化区间，防止长周期运行无限增长同时保持近期变化灵敏度
            if self.changed_intervals_secs.len() > 32 {
                let drain = self.changed_intervals_secs.len() - 32;
                self.changed_intervals_secs.drain(0..drain);
            }
        } else {
            self.unchanged_streak = self.unchanged_streak.saturating_add(1);
            self.exposure_secs = (self.exposure_secs + dt_secs).min(MAX_EXPOSURE_SECS);
            // 若连续多次无变化且仍有历史遗留变化区间，逐步退役最早的变化区间，避免永久拉高 λ（R09）
            if self.unchanged_streak >= 3
                && self.unchanged_streak % 2 == 1
                && !self.changed_intervals_secs.is_empty()
            {
                self.changed_intervals_secs.remove(0);
            }
        }
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
    }
}
