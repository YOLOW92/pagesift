//! Online two-dimensional capacity control for NUMA workers and concurrency.
//!
//! This controller deliberately does not build or sweep a benchmark grid. It
//! moves from the current incumbent in one direction, accepts useful gains,
//! accelerates while the direction remains useful, and rolls back immediately
//! after a regression. Once both dimensions are locally exhausted it locks the
//! incumbent. A locked controller issues one infrequent, isolated challenge;
//! a failed challenge restores the incumbent instead of starting another scan.

use super::telemetry::HardwareTopology;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapacityPhase {
    WorkerProbe,
    ConcurrencyProbe,
    Challenge,
    Locked,
    SafetyBackoff,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CapacitySample {
    pub qps: f64,
    pub queue_p95_ms: f64,
    pub error_rate_pct: f64,
    /// Process CPU is telemetry, not a reason to skip concurrency adaptation.
    pub process_cpu_pct: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityDecision {
    pub active_workers: usize,
    pub total_concurrency: usize,
    pub phase: CapacityPhase,
}

#[derive(Debug, Clone)]
pub struct CapacityControllerConfig {
    /// Samples ignored after changing a capacity setting.
    pub settle_samples: usize,
    pub min_probe_samples: usize,
    pub max_probe_samples: usize,
    pub relative_sem_target: f64,
    pub error_backoff_threshold_pct: f64,
    pub min_concurrency: usize,
    pub max_concurrency: usize,
    pub concurrency_growth_ratio: f64,
    /// Zero chooses one quarter of the topology tiers automatically.
    pub initial_worker_step_tiers: usize,
    pub initial_concurrency_step_tiers: usize,
    /// Gain required before a directional probe replaces the incumbent.
    pub min_improvement_ratio: f64,
    /// Results inside this regression band are inconclusive, not reversals.
    pub inconclusive_regression_ratio: f64,
    /// Hard budget for consecutive inconclusive probes in one direction.
    pub max_inconclusive_probes: usize,
    /// Locked telemetry samples between isolated remote challenges.
    pub locked_reprobe_samples: usize,
    /// A remote challenge must clear a higher bar than a local probe.
    pub challenge_min_improvement_ratio: f64,
}

impl Default for CapacityControllerConfig {
    fn default() -> Self {
        Self {
            settle_samples: 1,
            min_probe_samples: 3,
            max_probe_samples: 5,
            relative_sem_target: 0.005,
            error_backoff_threshold_pct: 1.0,
            min_concurrency: 4,
            max_concurrency: 4096,
            concurrency_growth_ratio: 1.25,
            initial_worker_step_tiers: 0,
            initial_concurrency_step_tiers: 2,
            min_improvement_ratio: 0.005,
            inconclusive_regression_ratio: 0.02,
            max_inconclusive_probes: 2,
            locked_reprobe_samples: 300,
            challenge_min_improvement_ratio: 0.02,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WorkerTierMeasurement {
    pub active_workers: usize,
    pub samples: usize,
    pub mean_qps: f64,
    pub stddev_qps: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ConcurrencyMeasurement {
    pub total_concurrency: usize,
    pub samples: usize,
    pub mean_qps: f64,
    pub stddev_qps: f64,
    pub mean_queue_p95_ms: f64,
    pub mean_process_cpu_pct: f64,
}

#[derive(Debug, Default)]
struct ProbeStats {
    count: usize,
    mean_qps: f64,
    m2_qps: f64,
    queue_p95_sum_ms: f64,
    process_cpu_sum_pct: f64,
}

impl ProbeStats {
    fn record(&mut self, sample: CapacitySample) {
        self.count += 1;
        let qps = finite_nonnegative(sample.qps);
        let delta = qps - self.mean_qps;
        self.mean_qps += delta / self.count as f64;
        self.m2_qps += delta * (qps - self.mean_qps);
        self.queue_p95_sum_ms += finite_nonnegative(sample.queue_p95_ms);
        self.process_cpu_sum_pct += finite_nonnegative(sample.process_cpu_pct);
    }

    fn stddev_qps(&self) -> f64 {
        if self.count > 1 {
            (self.m2_qps / (self.count - 1) as f64).max(0.0).sqrt()
        } else {
            0.0
        }
    }

    fn ready(&self, config: &CapacityControllerConfig) -> bool {
        if self.count < config.min_probe_samples.max(2) {
            return false;
        }
        if self.count >= config.max_probe_samples.max(config.min_probe_samples) {
            return true;
        }
        let sem = self.stddev_qps() / (self.count as f64).sqrt();
        sem / self.mean_qps.max(1.0) <= config.relative_sem_target.max(0.0)
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Debug)]
struct CapacityState {
    phase: CapacityPhase,
    worker_tier: usize,
    concurrency_tier: usize,
    incumbent_worker_tier: usize,
    incumbent_concurrency_tier: usize,
    incumbent_qps: f64,
    probe: ProbeStats,
    settle_remaining: usize,
    direction: i8,
    step: usize,
    inconclusive_probes: usize,
    visited: HashSet<(usize, usize)>,
    worker_measurements: Vec<Option<WorkerTierMeasurement>>,
    concurrency_measurements: Vec<Option<ConcurrencyMeasurement>>,
    locked_samples: usize,
    locked_qps_ewma: f64,
    challenge_cursor: usize,
}

/// A bounded online optimizer. Startup is directional and finite; it is not a
/// grid search. The request hot path remains NUMA-local and lock-free.
#[derive(Debug)]
pub struct AdaptiveCapacityController {
    worker_tiers: Vec<usize>,
    concurrency_tiers: Vec<usize>,
    config: CapacityControllerConfig,
    state: Mutex<CapacityState>,
}

impl AdaptiveCapacityController {
    pub fn from_topology(topology: &HardwareTopology) -> Self {
        let worker_tiers = topology
            .cpu_topology
            .worker_activation_tiers()
            .into_iter()
            .map(|tier| tier.len())
            .collect();
        let config = CapacityControllerConfig {
            min_concurrency: topology.min_concurrency,
            max_concurrency: topology.max_concurrency,
            ..CapacityControllerConfig::default()
        };
        Self::new(worker_tiers, topology.base_concurrency, config)
    }

    pub fn new(
        worker_tiers: Vec<usize>,
        initial_concurrency: usize,
        config: CapacityControllerConfig,
    ) -> Self {
        let initial_worker = worker_tiers.iter().copied().min().unwrap_or(1);
        Self::new_with_initial_worker(worker_tiers, initial_worker, initial_concurrency, config)
    }

    /// Builds a controller whose first measurement starts at a caller-selected
    /// worker tier rather than always at the smallest tier.
    pub fn new_with_initial_worker(
        mut worker_tiers: Vec<usize>,
        initial_worker: usize,
        initial_concurrency: usize,
        config: CapacityControllerConfig,
    ) -> Self {
        worker_tiers.retain(|workers| *workers > 0);
        worker_tiers.sort_unstable();
        worker_tiers.dedup();
        if worker_tiers.is_empty() {
            worker_tiers.push(1);
        }

        let initial = initial_concurrency.clamp(config.min_concurrency, config.max_concurrency);
        let concurrency_tiers = concurrency_probe_tiers(
            config.min_concurrency,
            config.max_concurrency,
            initial,
            config.concurrency_growth_ratio,
        );
        let initial_concurrency_tier = concurrency_tiers.binary_search(&initial).unwrap();
        let initial_worker_tier = nearest_tier(&worker_tiers, initial_worker);
        let worker_count = worker_tiers.len();
        let concurrency_count = concurrency_tiers.len();
        let initial_worker_step = configured_worker_step(&config, worker_count);
        let initial_direction = if initial_worker_tier + 1 == worker_count {
            -1
        } else {
            1
        };
        let initial_settle = config.settle_samples;
        let mut visited = HashSet::new();
        visited.insert((initial_worker_tier, initial_concurrency_tier));

        Self {
            worker_tiers,
            concurrency_tiers,
            config,
            state: Mutex::new(CapacityState {
                phase: CapacityPhase::WorkerProbe,
                worker_tier: initial_worker_tier,
                concurrency_tier: initial_concurrency_tier,
                incumbent_worker_tier: initial_worker_tier,
                incumbent_concurrency_tier: initial_concurrency_tier,
                incumbent_qps: 0.0,
                probe: ProbeStats::default(),
                // Do not let process/runtime warm-up depress the first incumbent.
                settle_remaining: initial_settle,
                direction: initial_direction,
                step: initial_worker_step,
                inconclusive_probes: 0,
                visited,
                worker_measurements: vec![None; worker_count],
                concurrency_measurements: vec![None; concurrency_count],
                locked_samples: 0,
                locked_qps_ewma: 0.0,
                challenge_cursor: 0,
            }),
        }
    }

    pub fn decision(&self) -> CapacityDecision {
        self.decision_for(&self.state.lock().unwrap())
    }

    pub fn worker_measurements(&self) -> Vec<WorkerTierMeasurement> {
        self.state
            .lock()
            .unwrap()
            .worker_measurements
            .iter()
            .flatten()
            .copied()
            .collect()
    }

    pub fn concurrency_measurements(&self) -> Vec<ConcurrencyMeasurement> {
        self.state
            .lock()
            .unwrap()
            .concurrency_measurements
            .iter()
            .flatten()
            .copied()
            .collect()
    }

    pub fn evaluate(&self, sample: CapacitySample) -> CapacityDecision {
        let mut state = self.state.lock().unwrap();

        if sample.error_rate_pct > self.config.error_backoff_threshold_pct {
            self.apply_safety_backoff(&mut state);
            return self.decision_for(&state);
        }

        if state.settle_remaining > 0 {
            state.settle_remaining -= 1;
            if state.settle_remaining == 0 && state.phase == CapacityPhase::SafetyBackoff {
                self.lock(&mut state);
            }
            return self.decision_for(&state);
        }

        match state.phase {
            CapacityPhase::WorkerProbe | CapacityPhase::ConcurrencyProbe => {
                state.probe.record(sample);
                if state.probe.ready(&self.config) {
                    self.finish_directional_probe(&mut state);
                }
            }
            CapacityPhase::Challenge => {
                state.probe.record(sample);
                if state.probe.ready(&self.config) {
                    self.finish_challenge(&mut state);
                }
            }
            CapacityPhase::Locked => self.observe_locked(&mut state, sample),
            CapacityPhase::SafetyBackoff => {}
        }

        self.decision_for(&state)
    }

    fn finish_directional_probe(&self, state: &mut CapacityState) {
        self.save_measurement(state);
        let measured_qps = state.probe.mean_qps;

        if state.incumbent_qps == 0.0 {
            self.promote_current(state, measured_qps);
        } else {
            let required = state.incumbent_qps * (1.0 + self.config.min_improvement_ratio.max(0.0));
            let regression_floor = state.incumbent_qps
                * (1.0 - self.config.inconclusive_regression_ratio.clamp(0.0, 0.5));
            if measured_qps > required {
                self.promote_current(state, measured_qps);
                state.step = state.step.saturating_mul(2).max(1);
                state.inconclusive_probes = 0;
            } else if measured_qps >= regression_floor
                && state.inconclusive_probes < self.config.max_inconclusive_probes
            {
                // A small difference is commonly scheduler/connection-pool
                // noise. Preserve a better raw point, then spend at most two
                // smaller forward probes to distinguish a plateau from a peak.
                if measured_qps > state.incumbent_qps {
                    self.promote_current(state, measured_qps);
                }
                state.step = state.step.div_ceil(2).max(1);
                state.inconclusive_probes += 1;
            } else {
                self.restore_incumbent(state);
                state.inconclusive_probes = 0;
                // A regression after a large step means the search overshot.
                // The peak is usually between the incumbent and the failed
                // point, so reverse and refine at the finest granularity
                // instead of bisecting the same side again. This is what turns
                // a 1536 -> 2272 overshoot into a direct 1536 -> 1453 -> 1162
                // -> 929 descent.
                if state.step > 1 {
                    state.direction = -state.direction;
                    state.step = 1;
                }
            }
        }

        if matches!(
            state.phase,
            CapacityPhase::WorkerProbe | CapacityPhase::ConcurrencyProbe
        ) && !self.select_directional_candidate(state)
        {
            // Exhausting one direction does not prove the incumbent is a local
            // optimum: the other side may never have been measured at the
            // finest granularity. Test it before locking.
            state.direction = -state.direction;
            state.step = 1;
            if !self.select_directional_candidate(state) {
                self.advance_dimension_or_lock(state);
            }
        }

        state.probe.reset();
        if state.phase != CapacityPhase::Locked {
            state.settle_remaining = self.config.settle_samples;
        }
    }

    fn select_directional_candidate(&self, state: &mut CapacityState) -> bool {
        let phase = state.phase;
        let (base, len) = match phase {
            CapacityPhase::WorkerProbe => (state.worker_tier, self.worker_tiers.len()),
            CapacityPhase::ConcurrencyProbe => {
                (state.concurrency_tier, self.concurrency_tiers.len())
            }
            _ => return false,
        };

        let mut step = state.step.max(1);
        loop {
            let candidate = offset_index(base, state.direction, step, len);
            if let Some(index) = candidate {
                let pair = match phase {
                    CapacityPhase::WorkerProbe => (index, state.incumbent_concurrency_tier),
                    CapacityPhase::ConcurrencyProbe => (state.incumbent_worker_tier, index),
                    _ => unreachable!(),
                };
                if !state.visited.contains(&pair) {
                    state.worker_tier = pair.0;
                    state.concurrency_tier = pair.1;
                    state.step = step;
                    state.visited.insert(pair);
                    return true;
                }
            }
            if step == 1 {
                return false;
            }
            step = step.div_ceil(2);
        }
    }

    fn advance_dimension_or_lock(&self, state: &mut CapacityState) {
        self.restore_incumbent(state);
        match state.phase {
            CapacityPhase::WorkerProbe => {
                state.phase = CapacityPhase::ConcurrencyProbe;
                state.direction = 1;
                state.step = self.config.initial_concurrency_step_tiers.max(1);
                state.inconclusive_probes = 0;
                if !self.select_directional_candidate(state) {
                    state.direction = -1;
                    state.step = 1;
                    if !self.select_directional_candidate(state) {
                        self.lock(state);
                    }
                }
            }
            CapacityPhase::ConcurrencyProbe => self.lock(state),
            _ => {}
        }
    }

    fn promote_current(&self, state: &mut CapacityState, qps: f64) {
        state.incumbent_worker_tier = state.worker_tier;
        state.incumbent_concurrency_tier = state.concurrency_tier;
        state.incumbent_qps = qps;
    }

    fn restore_incumbent(&self, state: &mut CapacityState) {
        state.worker_tier = state.incumbent_worker_tier;
        state.concurrency_tier = state.incumbent_concurrency_tier;
    }

    fn save_measurement(&self, state: &mut CapacityState) {
        match state.phase {
            CapacityPhase::WorkerProbe => {
                state.worker_measurements[state.worker_tier] = Some(WorkerTierMeasurement {
                    active_workers: self.worker_tiers[state.worker_tier],
                    samples: state.probe.count,
                    mean_qps: state.probe.mean_qps,
                    stddev_qps: state.probe.stddev_qps(),
                });
            }
            CapacityPhase::ConcurrencyProbe | CapacityPhase::Challenge => {
                state.concurrency_measurements[state.concurrency_tier] =
                    Some(ConcurrencyMeasurement {
                        total_concurrency: self.concurrency_tiers[state.concurrency_tier],
                        samples: state.probe.count,
                        mean_qps: state.probe.mean_qps,
                        stddev_qps: state.probe.stddev_qps(),
                        mean_queue_p95_ms: state.probe.queue_p95_sum_ms
                            / state.probe.count.max(1) as f64,
                        mean_process_cpu_pct: state.probe.process_cpu_sum_pct
                            / state.probe.count.max(1) as f64,
                    });
            }
            _ => {}
        }
    }

    fn lock(&self, state: &mut CapacityState) {
        self.restore_incumbent(state);
        state.phase = CapacityPhase::Locked;
        state.locked_samples = 0;
        state.locked_qps_ewma = state.incumbent_qps;
        state.probe.reset();
        state.settle_remaining = 0;
    }

    fn observe_locked(&self, state: &mut CapacityState, sample: CapacitySample) {
        let qps = finite_nonnegative(sample.qps);
        state.locked_qps_ewma = if state.locked_qps_ewma == 0.0 {
            qps
        } else {
            state.locked_qps_ewma * 0.95 + qps * 0.05
        };
        state.locked_samples += 1;
        if state.locked_samples >= self.config.locked_reprobe_samples.max(1) {
            self.start_next_challenge(state);
        }
    }

    fn start_next_challenge(&self, state: &mut CapacityState) {
        let incumbent = (
            state.incumbent_worker_tier,
            state.incumbent_concurrency_tier,
        );
        let candidates = challenge_pairs(
            self.worker_tiers.len(),
            self.concurrency_tiers.len(),
            incumbent,
        );
        let pair = candidates[state.challenge_cursor % candidates.len()];
        state.challenge_cursor = state.challenge_cursor.wrapping_add(1);
        state.worker_tier = pair.0;
        state.concurrency_tier = pair.1;
        state.phase = CapacityPhase::Challenge;
        state.locked_samples = 0;
        state.probe.reset();
        state.settle_remaining = self.config.settle_samples;
    }

    fn finish_challenge(&self, state: &mut CapacityState) {
        self.save_measurement(state);
        let required = state.locked_qps_ewma.max(1.0)
            * (1.0 + self.config.challenge_min_improvement_ratio.max(0.0));
        if state.probe.mean_qps > required {
            self.promote_current(state, state.probe.mean_qps);
            state.visited.clear();
            state
                .visited
                .insert((state.worker_tier, state.concurrency_tier));
            state.phase = CapacityPhase::WorkerProbe;
            state.direction = 1;
            state.step = 1;
            state.inconclusive_probes = 0;
            if !self.select_directional_candidate(state) {
                state.phase = CapacityPhase::ConcurrencyProbe;
                state.direction = 1;
                state.step = 1;
                if !self.select_directional_candidate(state) {
                    self.lock(state);
                }
            }
        } else {
            self.lock(state);
        }
        state.probe.reset();
        if state.phase != CapacityPhase::Locked {
            state.settle_remaining = self.config.settle_samples;
        }
    }

    fn apply_safety_backoff(&self, state: &mut CapacityState) {
        let current = self.concurrency_tiers[state.concurrency_tier];
        let target = ((current as f64) * 0.75)
            .round()
            .max(self.config.min_concurrency as f64) as usize;
        state.concurrency_tier = nearest_tier(&self.concurrency_tiers, target);
        state.incumbent_worker_tier = state.worker_tier;
        state.incumbent_concurrency_tier = state.concurrency_tier;
        state.incumbent_qps = 0.0;
        state.phase = CapacityPhase::SafetyBackoff;
        state.settle_remaining = self.config.settle_samples.max(1);
        state.probe.reset();
    }

    fn decision_for(&self, state: &CapacityState) -> CapacityDecision {
        CapacityDecision {
            active_workers: self.worker_tiers[state.worker_tier],
            total_concurrency: self.concurrency_tiers[state.concurrency_tier],
            phase: state.phase,
        }
    }
}

fn configured_worker_step(config: &CapacityControllerConfig, len: usize) -> usize {
    if config.initial_worker_step_tiers > 0 {
        config.initial_worker_step_tiers
    } else {
        len.div_ceil(4).max(1)
    }
}

fn offset_index(base: usize, direction: i8, step: usize, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    if direction >= 0 {
        Some(base.saturating_add(step).min(len - 1))
    } else {
        Some(base.saturating_sub(step))
    }
}

fn challenge_pairs(
    worker_len: usize,
    concurrency_len: usize,
    incumbent: (usize, usize),
) -> Vec<(usize, usize)> {
    let worker_last = worker_len.saturating_sub(1);
    let concurrency_last = concurrency_len.saturating_sub(1);
    let (worker, concurrency) = incumbent;
    let candidates = [
        (worker, concurrency / 2),
        (worker, concurrency + (concurrency_last - concurrency) / 2),
        (0, concurrency),
        (worker_last, concurrency),
        (worker_last, concurrency_last),
        (0, 0),
    ];
    let mut result = Vec::new();
    for pair in candidates {
        if pair != incumbent && !result.contains(&pair) {
            result.push(pair);
        }
    }
    if result.is_empty() {
        result.push(incumbent);
    }
    result
}

fn concurrency_probe_tiers(
    min_concurrency: usize,
    max_concurrency: usize,
    initial_concurrency: usize,
    growth_ratio: f64,
) -> Vec<usize> {
    let min = min_concurrency.max(1);
    let max = max_concurrency.max(min);
    let ratio = growth_ratio.max(1.1);
    let mut tiers = BTreeSet::from([min, initial_concurrency.clamp(min, max), max]);
    let mut current = min;
    while current < max {
        let next = ((current as f64 * ratio).ceil() as usize)
            .max(current + 1)
            .min(max);
        tiers.insert(next);
        current = next;
    }
    tiers.into_iter().collect()
}

fn nearest_tier(tiers: &[usize], target: usize) -> usize {
    tiers
        .iter()
        .enumerate()
        .min_by_key(|(_, value)| value.abs_diff(target))
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn finite_nonnegative(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(qps: f64, cpu: f64) -> CapacitySample {
        CapacitySample {
            qps,
            queue_p95_ms: 0.2,
            error_rate_pct: 0.0,
            process_cpu_pct: cpu,
        }
    }

    fn config() -> CapacityControllerConfig {
        CapacityControllerConfig {
            settle_samples: 0,
            min_probe_samples: 2,
            max_probe_samples: 2,
            relative_sem_target: 0.001,
            min_concurrency: 16,
            max_concurrency: 96,
            concurrency_growth_ratio: 1.5,
            initial_worker_step_tiers: 1,
            initial_concurrency_step_tiers: 1,
            min_improvement_ratio: 0.0,
            inconclusive_regression_ratio: 0.02,
            max_inconclusive_probes: 2,
            locked_reprobe_samples: 500,
            challenge_min_improvement_ratio: 0.02,
            error_backoff_threshold_pct: 1.0,
        }
    }

    fn drive<F>(controller: &AdaptiveCapacityController, limit: usize, score: F)
    where
        F: Fn(CapacityDecision) -> f64,
    {
        for _ in 0..limit {
            let decision = controller.decision();
            if decision.phase == CapacityPhase::Locked {
                return;
            }
            controller.evaluate(sample(score(decision), 100.0));
        }
    }

    #[test]
    fn directional_search_finds_pair_without_sweeping_surface() {
        let controller = AdaptiveCapacityController::new(vec![8, 10, 12, 14, 16], 64, config());
        drive(&controller, 200, |decision| {
            10_000.0
                - decision.active_workers.abs_diff(12) as f64 * 100.0
                - decision.total_concurrency.abs_diff(54) as f64 * 10.0
        });
        let decision = controller.decision();
        assert_eq!(decision.active_workers, 12);
        assert_eq!(decision.total_concurrency, 54);
        assert_eq!(decision.phase, CapacityPhase::Locked);
        assert!(controller.state.lock().unwrap().visited.len() <= 10);
    }

    #[test]
    fn caller_selected_worker_tier_is_the_first_decision() {
        let controller = AdaptiveCapacityController::new_with_initial_worker(
            vec![4, 8, 12, 16, 20],
            12,
            64,
            config(),
        );
        assert_eq!(controller.decision().active_workers, 12);
    }

    #[test]
    fn accelerating_steps_reach_a_distant_concurrency_region() {
        let mut cfg = config();
        cfg.min_concurrency = 32;
        cfg.max_concurrency = 12_288;
        cfg.concurrency_growth_ratio = 1.25;
        cfg.initial_concurrency_step_tiers = 2;
        let controller = AdaptiveCapacityController::new(vec![192], 768, cfg);
        drive(&controller, 300, |decision| {
            1_000_000.0 - decision.total_concurrency.abs_diff(1_817) as f64 * 10.0
        });
        let decision = controller.decision();
        assert_eq!(decision.total_concurrency, 1_817);
        assert_eq!(decision.phase, CapacityPhase::Locked);
        assert!(controller.concurrency_measurements().len() <= 10);
    }

    #[test]
    fn bounded_inconclusive_probe_crosses_a_short_noisy_valley() {
        let mut cfg = config();
        cfg.min_concurrency = 32;
        cfg.max_concurrency = 4096;
        cfg.concurrency_growth_ratio = 1.25;
        cfg.initial_concurrency_step_tiers = 1;
        cfg.min_improvement_ratio = 0.005;
        let controller = AdaptiveCapacityController::new(vec![192], 768, cfg);
        drive(&controller, 300, |decision| {
            match decision.total_concurrency {
                929 => 99_000.0,
                1162 => 105_000.0,
                value => 100_000.0 - value.abs_diff(1162) as f64,
            }
        });
        assert_eq!(controller.decision().total_concurrency, 1162);
        assert_eq!(controller.decision().phase, CapacityPhase::Locked);
    }

    #[test]
    fn full_process_cpu_does_not_disable_concurrency_adaptation() {
        let controller = AdaptiveCapacityController::new(vec![8], 64, config());
        drive(&controller, 120, |decision| {
            100_000.0 - decision.total_concurrency.abs_diff(81) as f64 * 10.0
        });
        assert_eq!(controller.decision().total_concurrency, 81);
        assert!(controller.concurrency_measurements().len() >= 2);
    }

    #[test]
    fn errors_back_off_immediately() {
        let controller = AdaptiveCapacityController::new(vec![8, 10], 64, config());
        let before = controller.decision();
        let mut overloaded = sample(100.0, 50.0);
        overloaded.error_rate_pct = 2.0;
        let after = controller.evaluate(overloaded);
        assert_eq!(after.active_workers, before.active_workers);
        assert!(after.total_concurrency < before.total_concurrency);
        assert_eq!(after.phase, CapacityPhase::SafetyBackoff);
    }

    #[test]
    fn locked_controller_runs_one_challenge_then_restores_incumbent() {
        let mut cfg = config();
        cfg.locked_reprobe_samples = 3;
        let controller = AdaptiveCapacityController::new(vec![8, 12, 16], 64, cfg);
        drive(&controller, 200, |decision| {
            100_000.0
                - decision.active_workers.abs_diff(12) as f64 * 1_000.0
                - decision.total_concurrency.abs_diff(54) as f64 * 100.0
        });
        let incumbent = controller.decision();
        assert_eq!(incumbent.phase, CapacityPhase::Locked);

        for _ in 0..4 {
            controller.evaluate(sample(100_000.0, 100.0));
            if controller.decision().phase == CapacityPhase::Challenge {
                break;
            }
        }
        assert_eq!(controller.decision().phase, CapacityPhase::Challenge);
        for _ in 0..4 {
            controller.evaluate(sample(1.0, 100.0));
            if controller.decision().phase == CapacityPhase::Locked {
                break;
            }
        }
        let restored = controller.decision();
        assert_eq!(restored.phase, CapacityPhase::Locked);
        assert_eq!(restored.active_workers, incumbent.active_workers);
        assert_eq!(restored.total_concurrency, incumbent.total_concurrency);
    }

    #[test]
    fn overshoot_reverses_to_fine_search_and_finds_the_lower_peak() {
        let mut cfg = config();
        cfg.min_concurrency = 32;
        cfg.max_concurrency = 4096;
        cfg.concurrency_growth_ratio = 1.25;
        cfg.initial_concurrency_step_tiers = 2;
        cfg.min_improvement_ratio = 0.005;
        cfg.inconclusive_regression_ratio = 0.02;
        // The model-derived initial concurrency is above the real knee.
        let controller = AdaptiveCapacityController::new(vec![192], 1536, cfg);
        drive(&controller, 200, |decision| {
            1_000_000.0 - decision.total_concurrency.abs_diff(929) as f64 * 100.0
        });
        let decision = controller.decision();
        assert_eq!(decision.total_concurrency, 929);
        assert_eq!(decision.phase, CapacityPhase::Locked);
        // A large overshoot must not keep walking further in the wrong
        // direction: every measured point stays within one expansion step of
        // the initial value.
        assert!(
            controller
                .concurrency_measurements()
                .iter()
                .all(|measurement| measurement.total_concurrency <= 2_272)
        );
    }

    #[test]
    fn concurrency_tiers_cover_boundaries_and_initial_value() {
        let tiers = concurrency_probe_tiers(32, 12_288, 768, 1.25);
        assert_eq!(tiers.first(), Some(&32));
        assert_eq!(tiers.last(), Some(&12_288));
        assert!(tiers.contains(&768));
        assert!(tiers.iter().any(|value| *value < 768));
        assert!(tiers.iter().any(|value| *value > 768));
    }
}
