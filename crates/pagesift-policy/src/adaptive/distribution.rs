//! Closed-loop tuning for NUMA-local batching and starvation-only stealing.
//!
//! The data plane owns the queues. This controller only publishes small,
//! bounded allocation changes; it never scans a parameter grid or creates
//! worker threads in the control path.
//!
//! Like the pipeline controller, every decision compares window means. A single
//! telemetry sample is dominated by scheduler and queue noise, so it is never
//! used on its own as evidence for a level change.

use super::evidence::{SampleWindow, WindowEvidence};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct DistributionControllerConfig {
    pub workers_per_node: usize,
    /// Telemetry samples ignored after a level change.
    pub settle_ticks: usize,
    /// Quiet samples between isolated re-probes of a locked level.
    pub lock_ticks_before_reprobe: usize,
    /// Smallest relative QPS change treated as a real effect. This is only a
    /// floor: the actual threshold also scales with the measured standard
    /// error, so a quiet host can accept a small real gain.
    pub min_effect_ratio: f64,
    /// How many combined standard errors a change must exceed before it counts
    /// as real.
    pub noise_sigmas: f64,
    pub min_window_samples: usize,
    pub max_window_samples: usize,
    pub relative_sem_target: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DistributionSample {
    pub qps: f64,
    pub completed: u64,
    pub queue_depth: usize,
    pub queue_capacity: usize,
    pub queue_imbalance_ratio: f64,
    pub queue_backpressure: u64,
    pub steal_attempts: u64,
    pub steal_successes: u64,
    pub stolen_jobs: u64,
    pub error_rate_pct: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DistributionControlState {
    Probing,
    Settling,
    Backpressure,
    Locked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionAllocation {
    /// Jobs claimed from a queue after one blocking receive.
    pub local_batch_size: usize,
    /// Consecutive empty local polls required before trying another NUMA node.
    pub empty_polls_before_steal: usize,
    /// Maximum jobs moved in one successful remote claim.
    pub steal_batch_size: usize,
    pub state: DistributionControlState,
}

#[derive(Debug)]
struct State {
    level: usize,
    best_level: usize,
    best_evidence: WindowEvidence,
    qps: SampleWindow,
    pressure: SampleWindow,
    settle_remaining: usize,
    locked_ticks: usize,
    direction: isize,
    challenge_pending: bool,
    challenge_baseline: WindowEvidence,
    rejected_low: bool,
    rejected_high: bool,
    last_evidence: WindowEvidence,
    mode: DistributionControlState,
}

/// One-dimensional bounded controller. Higher levels claim larger local
/// batches and allow earlier, slightly larger steals. It climbs while window
/// means improve and remembers both rejected neighbours so it cannot oscillate
/// across a confirmed local optimum.
pub struct DistributionController {
    config: DistributionControllerConfig,
    max_level: usize,
    state: Mutex<State>,
}

impl DistributionController {
    pub fn new(mut config: DistributionControllerConfig) -> Self {
        config.workers_per_node = config.workers_per_node.max(1);
        config.settle_ticks = config.settle_ticks.max(1);
        config.lock_ticks_before_reprobe = config.lock_ticks_before_reprobe.max(2);
        config.min_effect_ratio = config.min_effect_ratio.clamp(0.0, 1.0);
        config.noise_sigmas = if config.noise_sigmas.is_finite() {
            config.noise_sigmas.clamp(0.0, 10.0)
        } else {
            0.0
        };
        config.min_window_samples = config.min_window_samples.max(2);
        config.max_window_samples = config
            .max_window_samples
            .max(config.min_window_samples)
            .max(2);
        config.relative_sem_target = config.relative_sem_target.max(0.0);
        let maximum_batch = config.workers_per_node.div_ceil(2).clamp(2, 16);
        let max_level = maximum_batch.next_power_of_two().ilog2() as usize;
        // A small machine starts at one job. Wider NUMA nodes amortize the
        // shared receiver operation from a topology-derived batch, then the
        // controller may still move in either direction.
        let initial_batch = config
            .workers_per_node
            .div_ceil(32)
            .next_power_of_two()
            .clamp(1, maximum_batch);
        let initial_level = initial_batch.ilog2() as usize;
        let window = config.max_window_samples;
        Self {
            config,
            max_level,
            state: Mutex::new(State {
                level: initial_level,
                best_level: initial_level,
                best_evidence: WindowEvidence::default(),
                qps: SampleWindow::new(window),
                pressure: SampleWindow::new(window),
                settle_remaining: 0,
                locked_ticks: 0,
                direction: if initial_level > 0 { -1 } else { 1 },
                challenge_pending: false,
                challenge_baseline: WindowEvidence::default(),
                rejected_low: false,
                rejected_high: false,
                last_evidence: WindowEvidence::default(),
                mode: DistributionControlState::Probing,
            }),
        }
    }

    pub fn allocation(&self) -> DistributionAllocation {
        let state = self.state.lock().expect("distribution lock poisoned");
        self.allocation_for(&state)
    }

    /// Evidence window behind the most recent level decision.
    pub fn last_evidence(&self) -> WindowEvidence {
        self.state
            .lock()
            .expect("distribution lock poisoned")
            .last_evidence
    }

    pub fn tick(&self, sample: DistributionSample) -> DistributionAllocation {
        let mut state = self.state.lock().expect("distribution lock poisoned");

        if sample.error_rate_pct > 1.0 {
            state.level = state.best_level;
            state.challenge_pending = false;
            state.mode = DistributionControlState::Locked;
            state.settle_remaining = self.config.settle_ticks;
            state.locked_ticks = 0;
            state.qps.clear();
            state.pressure.clear();
            return self.allocation_for(&state);
        }

        if state.settle_remaining > 0 {
            state.settle_remaining -= 1;
            if !matches!(
                state.mode,
                DistributionControlState::Locked | DistributionControlState::Backpressure
            ) {
                state.mode = DistributionControlState::Settling;
            }
            return self.allocation_for(&state);
        }

        let queue_ratio = if sample.queue_capacity == 0 {
            0.0
        } else {
            sample.queue_depth as f64 / sample.queue_capacity as f64
        };
        let pressure_rate = sample.queue_backpressure as f64 / sample.completed.max(1) as f64;
        let pressured = queue_ratio >= 0.75 || pressure_rate >= 0.01;

        state.qps.push(sample.qps);
        state.pressure.push(if pressured { 1.0 } else { 0.0 });
        if state.mode == DistributionControlState::Locked {
            state.locked_ticks = state.locked_ticks.saturating_add(1);
        }
        if !state.qps.ready(
            self.config.min_window_samples,
            self.config.relative_sem_target,
        ) {
            return self.allocation_for(&state);
        }

        let mean = state.qps.mean();
        let pressure_share = state.pressure.mean();
        let evidence = state.qps.evidence();
        state.last_evidence = evidence;
        state.qps.clear();
        state.pressure.clear();

        if state.best_evidence.mean <= 0.0 && mean > 0.0 {
            state.best_evidence = evidence;
            state.best_level = state.level;
        }

        if state.challenge_pending {
            let improved = evidence.is_improvement(
                &state.challenge_baseline,
                self.config.min_effect_ratio,
                self.config.noise_sigmas,
            );
            if improved {
                state.best_evidence = evidence;
                state.best_level = state.level;
                if state.direction < 0 {
                    state.rejected_high = true;
                } else {
                    state.rejected_low = true;
                }
                state.mode = DistributionControlState::Probing;
            } else {
                if state.direction < 0 {
                    state.rejected_low = true;
                } else {
                    state.rejected_high = true;
                }
                state.level = state.best_level;
                state.direction = -state.direction;
                state.mode = DistributionControlState::Locked;
            }
            state.challenge_pending = false;
            state.locked_ticks = 0;
            state.settle_remaining = self.config.settle_ticks;
            return self.allocation_for(&state);
        }

        if evidence.is_improvement(
            &state.best_evidence,
            self.config.min_effect_ratio,
            self.config.noise_sigmas,
        ) {
            state.best_evidence = evidence;
            state.best_level = state.level;
        }

        // Queue pressure is direct evidence about the local claim rate, so it
        // may override a previously rejected larger-batch neighbour.
        if pressure_share >= 0.5 && state.level < self.max_level && self.probe_toward(&mut state, 1)
        {
            state.mode = DistributionControlState::Backpressure;
            return self.allocation_for(&state);
        }

        match state.mode {
            DistributionControlState::Locked => {
                let both_rejected = state.rejected_low && state.rejected_high;
                let wait = if both_rejected {
                    self.config.lock_ticks_before_reprobe.saturating_mul(5)
                } else {
                    self.config.lock_ticks_before_reprobe
                };
                if state.locked_ticks >= wait {
                    state.locked_ticks = 0;
                    if both_rejected {
                        state.rejected_low = false;
                        state.rejected_high = false;
                    }
                    if !self.probe(&mut state) {
                        state.mode = DistributionControlState::Locked;
                    }
                }
            }
            _ => {
                if !self.probe(&mut state) {
                    state.mode = DistributionControlState::Locked;
                    state.locked_ticks = 0;
                }
            }
        }
        self.allocation_for(&state)
    }

    fn probe(&self, state: &mut State) -> bool {
        let base = state.best_level;
        let down = base.saturating_sub(1);
        let up = (base + 1).min(self.max_level);
        if down == base {
            state.rejected_low = true;
        }
        if up == base {
            state.rejected_high = true;
        }
        let candidates = if state.direction < 0 {
            [(down, -1isize), (up, 1isize)]
        } else {
            [(up, 1isize), (down, -1isize)]
        };
        for (candidate, direction) in candidates {
            if candidate == base {
                continue;
            }
            let rejected = if direction < 0 {
                state.rejected_low
            } else {
                state.rejected_high
            };
            if rejected {
                continue;
            }
            self.start_probe(state, candidate, direction);
            return true;
        }
        false
    }

    fn probe_toward(&self, state: &mut State, direction: isize) -> bool {
        let base = state.best_level;
        let candidate = base.saturating_add_signed(direction).min(self.max_level);
        if candidate == base {
            return false;
        }
        self.start_probe(state, candidate, direction);
        true
    }

    fn start_probe(&self, state: &mut State, candidate: usize, direction: isize) {
        state.level = candidate;
        state.direction = direction;
        state.challenge_baseline = state.best_evidence;
        state.challenge_pending = true;
        state.settle_remaining = self.config.settle_ticks;
        state.mode = DistributionControlState::Settling;
    }

    fn allocation_for(&self, state: &State) -> DistributionAllocation {
        let local_batch_size = 1usize.checked_shl(state.level as u32).unwrap_or(16).min(16);
        DistributionAllocation {
            local_batch_size,
            empty_polls_before_steal: (8usize >> state.level.min(3)).max(1),
            steal_batch_size: local_batch_size.div_ceil(2).max(1),
            state: state.mode,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller() -> DistributionController {
        DistributionController::new(DistributionControllerConfig {
            workers_per_node: 36,
            settle_ticks: 1,
            lock_ticks_before_reprobe: 4,
            min_effect_ratio: 0.02,
            noise_sigmas: 2.0,
            min_window_samples: 2,
            max_window_samples: 3,
            relative_sem_target: 0.005,
        })
    }

    fn quiet(qps: f64) -> DistributionSample {
        DistributionSample {
            qps,
            ..Default::default()
        }
    }

    fn tick_n(controller: &DistributionController, qps: f64, n: usize) -> DistributionAllocation {
        let mut allocation = controller.allocation();
        for _ in 0..n {
            allocation = controller.tick(quiet(qps));
        }
        allocation
    }

    #[test]
    fn healthy_load_probes_one_adjacent_batch() {
        let controller = controller();
        let initial = controller.allocation();
        let next = tick_n(&controller, 100.0, 2);
        assert_ne!(next.local_batch_size, initial.local_batch_size);
        assert_eq!(next.state, DistributionControlState::Settling);
    }

    #[test]
    fn sustained_queue_pressure_raises_the_local_batch() {
        let controller = controller();
        let initial = controller.allocation();
        let mut next = initial;
        for _ in 0..2 {
            next = controller.tick(DistributionSample {
                qps: 100.0,
                completed: 100,
                queue_depth: 80,
                queue_capacity: 100,
                queue_imbalance_ratio: 0.5,
                queue_backpressure: 5,
                steal_attempts: 10,
                steal_successes: 8,
                stolen_jobs: 1,
                ..Default::default()
            });
        }
        assert_eq!(next.state, DistributionControlState::Backpressure);
        assert!(next.local_batch_size > initial.local_batch_size);
    }

    #[test]
    fn one_wild_sample_is_not_evidence() {
        let controller = controller();
        tick_n(&controller, 100.0, 2);
        tick_n(&controller, 100.0, 3);
        let before = controller.allocation();
        let after = controller.tick(quiet(400.0));
        assert_eq!(after, before);
    }

    #[test]
    fn failed_probe_restores_best_and_locks() {
        let controller = controller();
        let initial = controller.allocation();
        tick_n(&controller, 100.0, 2);
        let locked = tick_n(&controller, 60.0, 3);
        assert_eq!(locked.local_batch_size, initial.local_batch_size);
        assert_eq!(locked.state, DistributionControlState::Locked);
    }

    #[test]
    fn confirmed_optimum_does_not_oscillate() {
        let controller = controller();
        tick_n(&controller, 100.0, 2);
        tick_n(&controller, 60.0, 3);
        let mut allocation = controller.allocation();
        for _ in 0..(4 * 5 - 1) {
            allocation = controller.tick(quiet(100.0));
        }
        assert_eq!(allocation.state, DistributionControlState::Locked);
    }
}
