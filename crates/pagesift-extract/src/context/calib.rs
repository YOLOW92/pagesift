//! Calibrated retrieval confidence and drift-guarded online adaptation (`Learn/Adapt`).
//!
//! Wraps `pagesift_stat::calib::Isotonic`, `pagesift_stat::change::ChangeDetector` (CUSUM),
//! and `pagesift_stat::bocpd::BernoulliBocpd`. Accepts ONLY grounded/verified supervision
//! signals (never weak user-silence or LLM self-grades) and supports deterministic freeze/fallback.

use pagesift_stat::Delta;
use pagesift_stat::bocpd::BernoulliBocpd;
use pagesift_stat::calib::Isotonic;
use pagesift_stat::change::ChangeDetector;
use serde::{Deserialize, Serialize};

// [策略] 至少积累 16 条外部/接地强验证样本后才启用保序回归校准，冷启动期走确定性先验曲线
const MIN_CALIBRATION_SAMPLES: usize = 16;
// [策略] 滑动历史校准样本上限 512 条，控制内存与 PAV 拟合耗时 < 50us
const MAX_CALIBRATION_SAMPLES: usize = 512;
// [策略] 需要分辨的接地命中率漂移幅度 0.20（20 个百分点下降触发回退）
const DRIFT_RESOLUTION: f64 = 0.20;
// [策略] BOCPD 默认危险率 1/50 = 0.02
const DEFAULT_BOCPD_HAZARD: f64 = 0.02;

/// Drift detector choice for online adaptation experiments and production guardrails.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DriftGuardKind {
    #[default]
    CusumPageHinkley,
    BernoulliBocpd,
    None,
}

/// Versioned, serializable calibration and strategy state with deterministic fallback.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextLearnState {
    pub version: u32,
    pub enabled: bool,
    pub frozen: bool,
    pub drift_guard: DriftGuardKind,
    pub grounded_scores: Vec<f64>,
    pub grounded_outcomes: Vec<bool>,
    pub drift_fallbacks: u64,
    pub last_fallback_reason: Option<String>,
    #[serde(skip)]
    cached_iso: Option<Isotonic>,
    #[serde(skip)]
    cusum: Option<ChangeDetector>,
    #[serde(skip)]
    bocpd: Option<BernoulliBocpd>,
}

impl Default for ContextLearnState {
    fn default() -> Self {
        Self {
            version: 1,
            enabled: true,
            frozen: false,
            drift_guard: DriftGuardKind::CusumPageHinkley,
            grounded_scores: Vec::new(),
            grounded_outcomes: Vec::new(),
            drift_fallbacks: 0,
            last_fallback_reason: None,
            cached_iso: None,
            cusum: Some(ChangeDetector::new(DRIFT_RESOLUTION, Delta::DEFAULT)),
            bocpd: Some(BernoulliBocpd::new(DEFAULT_BOCPD_HAZARD, Delta::DEFAULT)),
        }
    }
}

impl ContextLearnState {
    /// Construct a fresh learn state configured with the specified `drift_guard`.
    pub fn with_drift_guard(drift_guard: DriftGuardKind) -> Self {
        Self {
            drift_guard,
            ..Self::default()
        }
    }

    /// Record a verified/grounded outcome (`is_hard_ground_truth == true` only for hard checks).
    /// Weak signals (`is_hard_ground_truth == false`) and non-finite scores are strictly ignored!
    pub fn observe_feedback(
        &mut self,
        raw_score: f32,
        grounded_correct: bool,
        is_hard_ground_truth: bool,
    ) -> bool {
        if !self.enabled || self.frozen || !is_hard_ground_truth || !raw_score.is_finite() {
            return false;
        }
        // Rehydrate skipped in-memory detectors from persisted `grounded_outcomes` after serde roundtrips!
        self.ensure_detectors();

        // Advance BOTH detectors in lockstep so mid-stream `drift_guard` switches never lose run-length history!
        let cusum_drifted = self
            .cusum
            .as_mut()
            .and_then(|c| c.push(f64::from(u8::from(grounded_correct))))
            .is_some_and(|chg| chg.shift == pagesift_stat::change::Shift::Down);
        let bocpd_drifted = self
            .bocpd
            .as_mut()
            .is_some_and(|b| b.push(grounded_correct));

        let drifted = match self.drift_guard {
            DriftGuardKind::CusumPageHinkley => cusum_drifted,
            DriftGuardKind::BernoulliBocpd => bocpd_drifted,
            DriftGuardKind::None => false,
        };

        if drifted {
            // Distribution shift detected: discard stale calibration window & reset both detectors cleanly!
            self.grounded_scores.clear();
            self.grounded_outcomes.clear();
            self.cached_iso = None;
            self.cusum = Some(ChangeDetector::new(DRIFT_RESOLUTION, Delta::DEFAULT));
            self.bocpd = Some(BernoulliBocpd::new(DEFAULT_BOCPD_HAZARD, Delta::DEFAULT));
            self.drift_fallbacks += 1;
            self.version = self.version.saturating_add(1);
            self.last_fallback_reason =
                Some("distribution_drift_detected_fallback_to_baseline".to_owned());
            return true;
        }

        if self.grounded_scores.len() >= MAX_CALIBRATION_SAMPLES {
            // [策略] 满窗淘汰最旧 1/4 样本保持平滑过渡
            let drain_n = MAX_CALIBRATION_SAMPLES / 4;
            let keep_len = self.grounded_outcomes.len().min(self.grounded_scores.len());
            self.grounded_scores.truncate(keep_len);
            self.grounded_outcomes.truncate(keep_len);
            self.grounded_scores.drain(0..drain_n);
            self.grounded_outcomes.drain(0..drain_n);
        }
        self.grounded_scores
            .push(f64::from(raw_score.clamp(0.0, 1.0)));
        self.grounded_outcomes.push(grounded_correct);
        if self.grounded_scores.len() >= MIN_CALIBRATION_SAMPLES {
            self.cached_iso = Some(Isotonic::fit(
                &self.grounded_scores,
                &self.grounded_outcomes,
            ));
        }
        false
    }

    /// Predict calibrated empirical support probability in `[0.0, 1.0]`.
    pub fn calibrate(&mut self, raw_score: f32, request_learn_enabled: bool) -> (f32, bool) {
        let clamped = if raw_score.is_finite() {
            raw_score.clamp(0.0, 1.0)
        } else {
            0.5
        };
        if !self.enabled || !request_learn_enabled {
            return (conservative_baseline_prob(clamped), false);
        }
        let valid_n = self.grounded_scores.len().min(self.grounded_outcomes.len());
        if valid_n < MIN_CALIBRATION_SAMPLES {
            if self.drift_fallbacks > 0 {
                // [策略] 分布漂移触发回退后的重积累期（<16 样本）返回最大熵中性先验 0.50，防止沿用旧高分过自信
                return (0.50, false);
            }
            return (conservative_baseline_prob(clamped), false);
        }
        if self.cached_iso.is_none() {
            self.cached_iso = Some(Isotonic::fit(
                &self.grounded_scores[..valid_n],
                &self.grounded_outcomes[..valid_n],
            ));
        }
        if let Some(iso) = &self.cached_iso {
            let pred = iso.predict(f64::from(clamped));
            if pred.is_finite() {
                // [策略] 避免极端 0/1 过度自信概率截断在 [0.02, 0.98]
                return (pred.clamp(0.02, 0.98) as f32, true);
            }
        }
        (conservative_baseline_prob(clamped), false)
    }

    fn ensure_detectors(&mut self) {
        if self.cusum.is_none() {
            let mut det = ChangeDetector::new(DRIFT_RESOLUTION, Delta::DEFAULT);
            for &outcome in &self.grounded_outcomes {
                let _ = det.push(f64::from(u8::from(outcome)));
            }
            self.cusum = Some(det);
        }
        if self.bocpd.is_none() {
            let mut det = BernoulliBocpd::new(DEFAULT_BOCPD_HAZARD, Delta::DEFAULT);
            for &outcome in &self.grounded_outcomes {
                let _ = det.push(outcome);
            }
            self.bocpd = Some(det);
        }
    }
}

fn conservative_baseline_prob(raw: f32) -> f32 {
    // [推导] Laplace 平滑收缩 (0.85 * raw + 0.05)，无学习样本时避免把词面分 1.0 直接当成 100% 真实率
    (0.85 * raw + 0.05).clamp(0.05, 0.95)
}
