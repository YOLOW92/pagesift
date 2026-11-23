//! Conservative domain- and claim-kind-stratified strategy learning for 1.7 Resolve.
//!
//! Learns ONLY verifiable research/verification strategy signals (empirical calibration on hard
//! external ground truth and counter-probe discovery rates). NEVER treats LLM self-grades or user
//! silence as truth, NEVER emits pseudo-precise probabilities when uncalibrated (`None`), and
//! automatically falls back to deterministic neutral baselines upon Page-Hinkley drift or verifier
//! version change.

use std::collections::BTreeMap;

use pagesift_graph::resolve::{
    CounterProbeKind, DEFAULT_VERIFIER_VERSION, ResolveLearnFeedback, ResolveLearnSummary,
};
use pagesift_stat::Delta;
use pagesift_stat::beta::{BetaCounts, Prior};
use pagesift_stat::calib::Isotonic;
use pagesift_stat::change::{ChangeDetector, Shift};
use pagesift_stat::verdict::{Bin, brier, reliability};
use serde::{Deserialize, Serialize};

// [策略] 单桶至少积累 16 条外部强真值样本后才允许输出保序校准概率，否则返回 None（只保留定性证据状态）
pub const MIN_RESOLVE_CALIB_SAMPLES: usize = 16;
// [策略] 单桶滑动历史窗口上限 256 条样本，控制内存与 PAV 拟合耗时
const MAX_BUCKET_SAMPLES: usize = 256;
// [策略] 最多独立维护 64 个 `(domain, claim_kind)` 分域校准桶防内存无界增长
const MAX_LEARN_BUCKETS: usize = 64;
// [策略] Page-Hinkley 漂移检测分辨幅度 0.20（命中率下降 20 个百分点触发回退）
const RESOLVE_DRIFT_RESOLUTION: f64 = 0.20;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveCalibBucket {
    pub scores: Vec<f64>,
    pub outcomes: Vec<bool>,
    #[serde(skip)]
    cached_iso: Option<Isotonic>,
    #[serde(skip)]
    cusum: Option<ChangeDetector>,
}

impl ResolveCalibBucket {
    fn ensure_detector(&mut self) {
        if self.cusum.is_none() {
            let mut det = ChangeDetector::new(RESOLVE_DRIFT_RESOLUTION, Delta::DEFAULT);
            for &y in &self.outcomes {
                let _ = det.push(f64::from(u8::from(y)));
            }
            self.cusum = Some(det);
        }
    }

    fn observe(&mut self, score: f64, outcome: bool) -> bool {
        self.ensure_detector();
        let drifted = self
            .cusum
            .as_mut()
            .and_then(|c| c.push(f64::from(u8::from(outcome))))
            .is_some_and(|chg| chg.shift == Shift::Down);
        if drifted {
            self.scores.clear();
            self.outcomes.clear();
            self.cached_iso = None;
            self.cusum = Some(ChangeDetector::new(
                RESOLVE_DRIFT_RESOLUTION,
                Delta::DEFAULT,
            ));
            return true;
        }
        if self.scores.len() >= MAX_BUCKET_SAMPLES {
            // [策略] 满窗淘汰最旧 1/4 样本保持平滑更新
            let drop_n = MAX_BUCKET_SAMPLES / 4;
            let keep = self.scores.len().min(self.outcomes.len());
            self.scores.truncate(keep);
            self.outcomes.truncate(keep);
            self.scores.drain(0..drop_n);
            self.outcomes.drain(0..drop_n);
        }
        self.scores.push(score.clamp(0.0, 1.0));
        self.outcomes.push(outcome);
        if self.scores.len() >= MIN_RESOLVE_CALIB_SAMPLES {
            self.cached_iso = Some(Isotonic::fit(&self.scores, &self.outcomes));
        }
        false
    }

    fn predict(&mut self, raw_score: f32) -> Option<f32> {
        let valid_n = self.scores.len().min(self.outcomes.len());
        if valid_n < MIN_RESOLVE_CALIB_SAMPLES || !raw_score.is_finite() {
            return None;
        }
        if self.cached_iso.is_none() {
            self.cached_iso = Some(Isotonic::fit(
                &self.scores[..valid_n],
                &self.outcomes[..valid_n],
            ));
        }
        let p = self
            .cached_iso
            .as_ref()?
            .predict(f64::from(raw_score.clamp(0.0, 1.0)));
        if p.is_finite() {
            // [策略] 截断在 [0.02, 0.98] 避免伪精确 0.0 / 1.0 绝对承诺
            Some(p.clamp(0.02, 0.98) as f32)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeUtilityStats {
    pub successes: f64,
    pub failures: f64,
}

impl ProbeUtilityStats {
    fn to_beta(&self) -> BetaCounts {
        BetaCounts {
            success: self.successes.max(0.0),
            failure: self.failures.max(0.0),
        }
    }

    pub fn mean(&self) -> f64 {
        self.to_beta().posterior(Prior::UNIFORM).mean()
    }

    pub fn trials(&self) -> u64 {
        (self.successes + self.failures).round().max(0.0) as u64
    }
}

/// Serializable tenant-isolated strategy learning state for 1.7 Resolve.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveLearnState {
    pub version: u32,
    pub verifier_version: String,
    pub enabled: bool,
    pub frozen: bool,
    pub buckets: BTreeMap<String, ResolveCalibBucket>,
    pub probe_stats: BTreeMap<String, ProbeUtilityStats>,
    pub drift_fallbacks: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fallback_reason: Option<String>,
}

impl Default for ResolveLearnState {
    fn default() -> Self {
        Self {
            version: 1,
            verifier_version: DEFAULT_VERIFIER_VERSION.to_owned(),
            enabled: true,
            frozen: false,
            buckets: BTreeMap::new(),
            probe_stats: BTreeMap::new(),
            drift_fallbacks: 0,
            last_fallback_reason: None,
        }
    }
}

impl ResolveLearnState {
    pub fn bucket_key(domain: &str, claim_kind: &str) -> String {
        let d = if domain.trim().is_empty() {
            "general"
        } else {
            domain.trim()
        };
        let k = if claim_kind.trim().is_empty() {
            "existential"
        } else {
            claim_kind.trim()
        };
        format!("{}:{}", d.to_ascii_lowercase(), k.to_ascii_lowercase())
    }

    /// Detect verifier/model version upgrade and invalidate stale calibration buckets automatically.
    pub fn ensure_verifier_version(&mut self, current_version: &str) -> bool {
        if current_version.is_empty() || self.verifier_version == current_version {
            return false;
        }
        self.verifier_version = current_version.to_owned();
        if !self.buckets.is_empty() {
            self.buckets.clear();
            self.drift_fallbacks = self.drift_fallbacks.saturating_add(1);
            self.version = self.version.saturating_add(1);
            self.last_fallback_reason = Some(format!("verifier_version_changed:{current_version}"));
            return true;
        }
        false
    }

    /// Record an external ground-truth feedback event. Weak/ungrounded signals are strictly ignored.
    /// Returns `true` if drift or version change triggered a fallback reset.
    pub fn observe_feedback(&mut self, fb: &ResolveLearnFeedback) -> bool {
        if !self.enabled || self.frozen || !fb.is_hard_ground_truth || !fb.raw_score.is_finite() {
            return false;
        }
        let mut fallback = false;
        if let Some(vv) = fb.verifier_version.as_deref()
            && self.ensure_verifier_version(vv)
        {
            fallback = true;
        }

        let bkey = Self::bucket_key(&fb.domain, &fb.claim_kind);
        if !self.buckets.contains_key(&bkey) && self.buckets.len() >= MAX_LEARN_BUCKETS {
            // Evict lexicographically oldest/smallest bucket if cap reached.
            if let Some(first_k) = self.buckets.keys().next().cloned() {
                self.buckets.remove(&first_k);
            }
        }
        let bucket = self.buckets.entry(bkey.clone()).or_default();
        if bucket.observe(f64::from(fb.raw_score), fb.grounded_supported) {
            self.drift_fallbacks = self.drift_fallbacks.saturating_add(1);
            self.version = self.version.saturating_add(1);
            self.last_fallback_reason = Some(format!("distribution_drift_fallback:{bkey}"));
            fallback = true;
        }

        if let (Some(pk), Some(found)) = (fb.counter_probe_kind, fb.counter_found) {
            let pkey = probe_kind_key(pk).to_owned();
            let st = self.probe_stats.entry(pkey).or_default();
            if found {
                st.successes += 1.0;
            } else {
                st.failures += 1.0;
            }
        }
        fallback
    }

    /// Predict calibrated support probability ONLY when sufficient hard ground-truth samples exist
    /// in the matching `(domain, claim_kind)` bucket. Returns `None` otherwise (no pseudo-precision!).
    pub fn calibrated_probability(
        &mut self,
        domain: &str,
        claim_kind: &str,
        raw_score: f32,
        request_learn_enabled: bool,
    ) -> Option<f32> {
        if !self.enabled || !request_learn_enabled {
            return None;
        }
        let bkey = Self::bucket_key(domain, claim_kind);
        self.buckets.get_mut(&bkey)?.predict(raw_score)
    }

    /// Expected discovery rate for a counter-evidence probe kind (uniform prior 0.50 when cold).
    pub fn probe_discovery_rate(&self, kind: CounterProbeKind) -> f64 {
        if !self.enabled {
            return 0.5;
        }
        self.probe_stats
            .get(probe_kind_key(kind))
            .map(ProbeUtilityStats::mean)
            .unwrap_or(0.5)
    }

    pub fn summary(&self) -> ResolveLearnSummary {
        let total_hard_samples: usize = self.buckets.values().map(|b| b.outcomes.len()).sum();
        let probe_rates: Vec<(String, f64, u64)> = self
            .probe_stats
            .iter()
            .map(|(k, v)| (k.clone(), v.mean(), v.trials()))
            .collect();
        ResolveLearnSummary {
            version: self.version,
            verifier_version: self.verifier_version.clone(),
            enabled: self.enabled,
            frozen: self.frozen,
            bucket_count: self.buckets.len(),
            total_hard_samples,
            drift_fallbacks: self.drift_fallbacks,
            last_fallback_reason: self.last_fallback_reason.clone(),
            probe_rates,
        }
    }
}

pub fn probe_kind_key(kind: CounterProbeKind) -> &'static str {
    match kind {
        CounterProbeKind::DirectContradiction => "directContradiction",
        CounterProbeKind::OfficialErrataOrUpdate => "officialErrataOrUpdate",
        CounterProbeKind::ExceptionOrQualifier => "exceptionOrQualifier",
        CounterProbeKind::ComparativeDomainScan => "comparativeDomainScan",
        CounterProbeKind::CompetingHypothesis => "competingHypothesis",
    }
}

/// Safe wrapper over `pagesift_stat::verdict::{brier, reliability}` guarding empty / tiny slices.
pub fn evaluate_calibration_metrics(probs: &[f64], ys: &[bool]) -> Option<(f64, Vec<Bin>, f64)> {
    // [策略] Brier/ECE 校准评估至少需要 4 个有效样本避免极小样本除零与伪精度
    if probs.len() < 4 || probs.len() != ys.len() {
        return None;
    }
    let brier_score = brier(probs, ys);
    let bins = reliability(probs, ys, Delta::DEFAULT);
    let total = probs.len() as f64;
    let ece: f64 = bins
        .iter()
        .map(|b| (b.n as f64 / total) * (b.observed - b.mean_pred).abs())
        .sum();
    Some((brier_score, bins, ece))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_weak_signals_abstains_when_cold_and_falls_back_on_drift_or_version_change() {
        let mut st = ResolveLearnState::default();
        // Cold start: MUST return None rather than a fabricated probability!
        assert_eq!(
            st.calibrated_probability("pricing", "existential", 0.9, true),
            None
        );

        // Weak feedback (is_hard_ground_truth = false) must be completely ignored.
        for _ in 0..30 {
            st.observe_feedback(&ResolveLearnFeedback {
                domain: "pricing".into(),
                claim_kind: "existential".into(),
                raw_score: 0.95,
                grounded_supported: true,
                is_hard_ground_truth: false,
                counter_probe_kind: None,
                counter_found: None,
                verifier_version: None,
            });
        }
        assert_eq!(st.summary().total_hard_samples, 0);

        // Hard ground truth in "pricing" must NOT leak to "medical" (domain isolation).
        for i in 0..24 {
            let s = if i % 2 == 0 { 0.85 } else { 0.25 };
            let y = i % 2 == 0;
            st.observe_feedback(&ResolveLearnFeedback {
                domain: "pricing".into(),
                claim_kind: "existential".into(),
                raw_score: s,
                grounded_supported: y,
                is_hard_ground_truth: true,
                counter_probe_kind: Some(CounterProbeKind::OfficialErrataOrUpdate),
                counter_found: Some(y),
                verifier_version: None,
            });
        }
        assert!(
            st.calibrated_probability("pricing", "existential", 0.85, true)
                .is_some()
        );
        assert_eq!(
            st.calibrated_probability("medical", "existential", 0.85, true),
            None
        );

        // Verifier version change must automatically clear stale calibration!
        let reset = st.ensure_verifier_version("resolve-det-v2");
        assert!(reset);
        assert_eq!(
            st.calibrated_probability("pricing", "existential", 0.85, true),
            None
        );
        assert_eq!(st.drift_fallbacks, 1);
    }
}
