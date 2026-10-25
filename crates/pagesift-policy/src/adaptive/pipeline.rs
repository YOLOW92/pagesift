//! Closed-loop allocation between I/O reactors and content processing.
//!
//! Threads are created and pinned once. The controller changes active work
//! quotas, so adaptation does not pay thread creation or affinity migration.
//!
//! Every decision compares window means. Inline parsing borrows reactor cycles
//! from socket servicing, so the search range is the full legal
//! `[min_content, max_content]` interval: the controller is free to explore all
//! of it, and statistical evidence rather than a machine-specific cap decides
//! where the search stops.

use super::evidence::{SampleWindow, WindowEvidence};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct PipelineControllerConfig {
    pub total_workers: usize,
    pub initial_reactors: usize,
    pub numa_nodes: usize,
    pub adjustment_quantum: usize,
    /// Telemetry samples ignored after a quota change so queues and sockets can
    /// reach the new steady state.
    pub settle_ticks: usize,
    /// Quiet samples between isolated re-probes of a locked incumbent.
    pub lock_ticks_before_reprobe: usize,
    /// Smallest relative QPS change treated as a real effect. This is only a
    /// floor: the actual threshold also scales with the measured standard
    /// error, so a quiet host can accept a small real gain.
    pub min_effect_ratio: f64,
    /// How many combined standard errors a change must exceed before it counts
    /// as real. Two sigmas is roughly 95% confidence for a normal window mean.
    pub noise_sigmas: f64,
    /// Minimum samples before a window may be used as evidence.
    pub min_window_samples: usize,
    /// Hard sample cap so a noisy host cannot stall adaptation forever.
    pub max_window_samples: usize,
    /// Early-stop target for the relative standard error of the window mean.
    pub relative_sem_target: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PipelineSample {
    pub qps: f64,
    pub process_cpu_pct: f64,
    pub completed: u64,
    pub queue_depth: usize,
    pub queue_capacity: usize,
    pub queue_backpressure: u64,
    pub error_rate_pct: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PipelineControlState {
    Probing,
    Settling,
    Backpressure,
    Locked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineAllocation {
    /// Reactor-equivalent work budget. This includes dedicated reactors plus
    /// the CPU share reclaimed through inline parsing.
    pub reactor_budget: usize,
    /// Number of content workers allowed to consume NUMA-local parse queues.
    pub content_budget: usize,
    /// Deterministic percentage of responses parsed by their owning reactor.
    pub inline_parse_percent: usize,
    pub state: PipelineControlState,
}

#[derive(Debug)]
struct State {
    content_budget: usize,
    best_content_budget: usize,
    /// Window evidence at the incumbent allocation. A single sample is never
    /// stored here; `SampleWindow::ready` guarantees enough observations.
    best_evidence: WindowEvidence,
    qps: SampleWindow,
    pressure: SampleWindow,
    settle_remaining: usize,
    locked_ticks: usize,
    direction: isize,
    /// A probe is in flight and the next ready window decides its fate.
    challenge_pending: bool,
    challenge_baseline: WindowEvidence,
    /// Neighbour already measured and rejected at the current incumbent.
    /// Remembering both sides prevents the controller from oscillating between
    /// two known-worse points after it has found a local optimum.
    rejected_low: bool,
    rejected_high: bool,
    last_evidence: WindowEvidence,
    mode: PipelineControlState,
}

/// Event-driven controller: one telemetry sample causes at most one bounded
/// quota change. There is no grid scan and no background search loop.
pub struct PipelineController {
    config: PipelineControllerConfig,
    min_reactors: usize,
    min_content: usize,
    max_content: usize,
    state: Mutex<State>,
}

impl PipelineController {
    pub fn new(mut config: PipelineControllerConfig) -> Self {
        config.total_workers = config.total_workers.max(2);
        config.numa_nodes = config.numa_nodes.max(1);
        config.adjustment_quantum = config.adjustment_quantum.max(1);
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
        let topology_min_reactors = config.numa_nodes.min(config.total_workers - 1).max(1);
        let min_content = config
            .numa_nodes
            .min(config.total_workers - topology_min_reactors)
            .max(1);
        let initial_reactors = config
            .initial_reactors
            .clamp(topology_min_reactors, config.total_workers - min_content);
        let initial_content = config.total_workers - initial_reactors;
        // Dedicated reactor owners cannot be parked without migrating their
        // sockets. The startup topology split is therefore the hard reactor
        // floor; adaptation can reclaim content CPU for reactor-inline work,
        // then restore it, but never invents content capacity that is not
        // backed by a pre-created worker.
        let min_reactors = initial_reactors;
        let max_content = initial_content;
        let window = config.max_window_samples;
        Self {
            config,
            min_reactors,
            min_content,
            max_content,
            state: Mutex::new(State {
                content_budget: initial_content,
                best_content_budget: initial_content,
                best_evidence: WindowEvidence::default(),
                qps: SampleWindow::new(window),
                pressure: SampleWindow::new(window),
                settle_remaining: 0,
                locked_ticks: 0,
                // The topology split begins content-heavy. First test whether
                // reclaiming a small content slice for inline reactor work helps.
                direction: -1,
                challenge_pending: false,
                challenge_baseline: WindowEvidence::default(),
                rejected_low: false,
                rejected_high: false,
                last_evidence: WindowEvidence::default(),
                mode: PipelineControlState::Probing,
            }),
        }
    }

    pub fn allocation(&self) -> PipelineAllocation {
        let state = self
            .state
            .lock()
            .expect("pipeline controller lock poisoned");
        self.allocation_for(&state)
    }

    /// Evidence window behind the most recent quota decision.
    pub fn last_evidence(&self) -> WindowEvidence {
        self.state
            .lock()
            .expect("pipeline controller lock poisoned")
            .last_evidence
    }

    pub fn tick(&self, sample: PipelineSample) -> PipelineAllocation {
        let mut state = self
            .state
            .lock()
            .expect("pipeline controller lock poisoned");

        // A hard safety signal does not need a performance window: errors mean
        // the current split is actively harmful, so restore content capacity
        // immediately instead of waiting for the next decision point.
        if sample.error_rate_pct > 1.0 {
            state.content_budget =
                (state.content_budget + self.config.adjustment_quantum).min(self.max_content);
            state.challenge_pending = false;
            state.mode = PipelineControlState::Backpressure;
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
                PipelineControlState::Locked | PipelineControlState::Backpressure
            ) {
                state.mode = PipelineControlState::Settling;
            }
            return self.allocation_for(&state);
        }

        let queue_ratio = if sample.queue_capacity == 0 {
            0.0
        } else {
            sample.queue_depth as f64 / sample.queue_capacity as f64
        };
        let backpressure_rate = sample.queue_backpressure as f64 / sample.completed.max(1) as f64;
        let pressured = queue_ratio >= 0.75 || backpressure_rate >= 0.01;

        state.qps.push(sample.qps);
        state.pressure.push(if pressured { 1.0 } else { 0.0 });
        if state.mode == PipelineControlState::Locked {
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
            state.best_content_budget = state.content_budget;
        }

        if state.challenge_pending {
            let improved = evidence.is_improvement(
                &state.challenge_baseline,
                self.config.min_effect_ratio,
                self.config.noise_sigmas,
            );
            if improved {
                state.best_evidence = evidence;
                state.best_content_budget = state.content_budget;
                // The previous incumbent is now known to be worse, so do not
                // send the next probe back across it.
                if state.direction < 0 {
                    state.rejected_high = true;
                } else {
                    state.rejected_low = true;
                }
                state.mode = PipelineControlState::Probing;
            } else {
                if state.direction < 0 {
                    state.rejected_low = true;
                } else {
                    state.rejected_high = true;
                }
                state.content_budget = state.best_content_budget;
                state.direction = -state.direction;
                state.mode = PipelineControlState::Locked;
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
            state.best_content_budget = state.content_budget;
        }

        // Sustained content-queue pressure is a direct signal that the content
        // lane is starved. It overrides a previously rejected neighbour because
        // queue pressure measures the lane itself, not QPS noise. The next
        // window still decides whether the move helped.
        if pressure_share >= 0.5
            && state.content_budget < self.max_content
            && self.probe_toward(&mut state, 1)
        {
            state.mode = PipelineControlState::Backpressure;
            return self.allocation_for(&state);
        }

        match state.mode {
            PipelineControlState::Locked => {
                let both_rejected = state.rejected_low && state.rejected_high;
                let wait = if both_rejected {
                    // Both neighbours are known worse. Re-validate the local
                    // optimum only after a long quiet period instead of
                    // oscillating across it every lock interval.
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
                        state.mode = PipelineControlState::Locked;
                    }
                }
            }
            _ => {
                if !self.probe(&mut state) {
                    state.mode = PipelineControlState::Locked;
                    state.locked_ticks = 0;
                }
            }
        }
        self.allocation_for(&state)
    }

    /// Starts one probe at the nearest untested neighbour of the incumbent.
    /// Returns false when both neighbours are already rejected.
    fn probe(&self, state: &mut State) -> bool {
        let base = state.best_content_budget;
        let down = base
            .saturating_sub(self.config.adjustment_quantum)
            .max(self.min_content);
        let up = base
            .saturating_add(self.config.adjustment_quantum)
            .min(self.max_content);
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

    /// Starts a probe in one explicit direction, ignoring the rejected-side
    /// memory. Used by the queue-pressure path, which is direct evidence about
    /// the content lane rather than a QPS comparison.
    fn probe_toward(&self, state: &mut State, direction: isize) -> bool {
        let base = state.best_content_budget;
        let candidate = base
            .saturating_add_signed(self.config.adjustment_quantum as isize * direction)
            .clamp(self.min_content, self.max_content);
        if candidate == base {
            return false;
        }
        self.start_probe(state, candidate, direction);
        true
    }

    fn start_probe(&self, state: &mut State, candidate: usize, direction: isize) {
        state.content_budget = candidate;
        state.direction = direction;
        state.challenge_baseline = state.best_evidence;
        state.challenge_pending = true;
        state.settle_remaining = self.config.settle_ticks;
        state.mode = PipelineControlState::Settling;
    }

    fn allocation_for(&self, state: &State) -> PipelineAllocation {
        let reactor_budget = self.config.total_workers - state.content_budget;
        let reclaimed = self.max_content.saturating_sub(state.content_budget);
        let inline_parse_percent = reclaimed
            .saturating_mul(100)
            .div_ceil(self.max_content.max(1))
            .min(100);
        PipelineAllocation {
            reactor_budget: reactor_budget.max(self.min_reactors),
            content_budget: state.content_budget,
            inline_parse_percent,
            state: state.mode,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller() -> PipelineController {
        PipelineController::new(PipelineControllerConfig {
            total_workers: 192,
            initial_reactors: 48,
            numa_nodes: 2,
            adjustment_quantum: 8,
            settle_ticks: 1,
            lock_ticks_before_reprobe: 8,
            min_effect_ratio: 0.02,
            noise_sigmas: 2.0,
            min_window_samples: 2,
            max_window_samples: 3,
            relative_sem_target: 0.005,
        })
    }

    fn quiet(qps: f64) -> PipelineSample {
        PipelineSample {
            qps,
            process_cpu_pct: 70.0,
            ..Default::default()
        }
    }

    /// Ticks exactly `n` samples. The evidence window is two samples wide in
    /// these tests, so the sequence of probe/resolve ticks is deterministic.
    fn tick_n(controller: &PipelineController, qps: f64, n: usize) -> PipelineAllocation {
        let mut allocation = controller.allocation();
        for _ in 0..n {
            allocation = controller.tick(quiet(qps));
        }
        allocation
    }

    #[test]
    fn starts_from_topology_split_and_probes_without_rebuilding_threads() {
        let controller = controller();
        let initial = controller.allocation();
        assert_eq!((initial.reactor_budget, initial.content_budget), (48, 144));
        let next = tick_n(&controller, 100.0, 2);
        assert_eq!((next.reactor_budget, next.content_budget), (56, 136));
        assert!(next.inline_parse_percent > 0);
        assert_eq!(next.state, PipelineControlState::Settling);
    }

    #[test]
    fn one_wild_sample_is_not_evidence() {
        let controller = controller();
        tick_n(&controller, 100.0, 2);
        // Reject the probe, then feed one huge outlier.
        tick_n(&controller, 100.0, 3);
        let before = controller.allocation();
        let after = controller.tick(quiet(400.0));
        assert_eq!(after, before);
    }

    #[test]
    fn failed_probe_restores_incumbent_and_locks() {
        let controller = controller();
        tick_n(&controller, 100.0, 2);
        let locked = tick_n(&controller, 80.0, 3);
        assert_eq!(locked.content_budget, 144);
        assert_eq!(locked.state, PipelineControlState::Locked);
    }

    #[test]
    fn successful_probe_keeps_climbing_in_the_same_direction() {
        let controller = controller();
        tick_n(&controller, 100.0, 2);
        let first = tick_n(&controller, 110.0, 3);
        assert_eq!(first.content_budget, 136);
        assert_eq!(first.state, PipelineControlState::Probing);
        let second = tick_n(&controller, 120.0, 3);
        assert_eq!(second.content_budget, 128);
    }

    #[test]
    fn confirmed_optimum_does_not_oscillate_between_two_worse_neighbours() {
        let controller = controller();
        tick_n(&controller, 100.0, 2);
        tick_n(&controller, 80.0, 3);
        let mut allocation = controller.allocation();
        for _ in 0..(8 * 5 - 1) {
            allocation = controller.tick(quiet(100.0));
        }
        assert_eq!(allocation.content_budget, 144);
        assert_eq!(allocation.state, PipelineControlState::Locked);
    }

    #[test]
    fn sustained_pressure_overrides_a_rejected_neighbour() {
        let controller = controller();
        tick_n(&controller, 100.0, 2);
        // Improve at 136 so the incumbent moves down and 144 is rejected.
        tick_n(&controller, 110.0, 3);
        let incumbent = controller.allocation();
        assert_eq!(incumbent.content_budget, 136);
        // Now the content lane backs up. Pressure must be able to reclaim
        // content capacity even though 144 measured worse on QPS alone.
        let mut pressured = incumbent;
        for _ in 0..3 {
            pressured = controller.tick(PipelineSample {
                qps: 100.0,
                completed: 100,
                queue_depth: 90,
                queue_capacity: 100,
                queue_backpressure: 5,
                ..Default::default()
            });
        }
        assert_eq!(pressured.content_budget, 144);
        assert_eq!(pressured.state, PipelineControlState::Backpressure);
    }
}
