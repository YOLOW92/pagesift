//! Independent, event-driven capacity control for the public-network pipeline.
//!
//! Each stage owns its admission limit and learns only from outcomes that are
//! relevant to that stage. There is no parameter grid and no background scan.

use std::array;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

const STAGE_COUNT: usize = 5;
const PHASE_COUNT: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum NetworkStage {
    Dns = 0,
    Handshake = 1,
    Http = 2,
    Parse = 3,
    Tail = 4,
}

impl NetworkStage {
    pub const ALL: [Self; STAGE_COUNT] = [
        Self::Dns,
        Self::Handshake,
        Self::Http,
        Self::Parse,
        Self::Tail,
    ];

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageOutcome {
    Success,
    /// DNS/network/timeout failures that may improve after reducing pressure.
    TransientFailure,
    /// HTTP 429 or another direct remote-overload signal.
    Overload,
    /// Certificate, protocol, malformed-input and other repeatable failures.
    DeterministicFailure,
    /// robots, SSRF and access-policy rejection. Never teaches capacity.
    PolicyRejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum TransportPhase {
    Dns = 0,
    Tcp = 1,
    Tls = 2,
    Ttfb = 3,
    Body = 4,
}

impl TransportPhase {
    pub const ALL: [Self; PHASE_COUNT] = [Self::Dns, Self::Tcp, Self::Tls, Self::Ttfb, Self::Body];
}

#[derive(Debug, Clone, Copy)]
pub struct StageLimit {
    pub initial: usize,
    pub min: usize,
    pub max: usize,
}

impl StageLimit {
    fn normalized(self) -> Self {
        let min = self.min.max(1);
        let max = self.max.max(min);
        Self {
            initial: self.initial.clamp(min, max),
            min,
            max,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StageControllerConfig {
    pub dns: StageLimit,
    pub handshake: StageLimit,
    pub http: StageLimit,
    pub parse: StageLimit,
    pub tail: StageLimit,
    pub observation_window: usize,
    pub latency_samples: usize,
    pub transient_backoff_ratio: f64,
    pub overload_backoff_ratio: f64,
}

impl Default for StageControllerConfig {
    fn default() -> Self {
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .max(1);
        let http = workers.saturating_mul(8).clamp(16, 4096);
        Self {
            dns: StageLimit {
                initial: workers.saturating_mul(2),
                min: 4,
                max: workers.saturating_mul(8).clamp(8, 2048),
            },
            handshake: StageLimit {
                initial: workers.saturating_mul(2),
                min: 4,
                max: workers.saturating_mul(6).clamp(8, 2048),
            },
            http: StageLimit {
                initial: http,
                min: 8,
                max: http.saturating_mul(4).clamp(32, 8192),
            },
            parse: StageLimit {
                initial: workers,
                min: 1,
                max: workers,
            },
            // Tail work is isolated and deliberately bounded below the main path.
            tail: StageLimit {
                initial: workers.div_ceil(4),
                min: 1,
                max: workers.max(2),
            },
            observation_window: 32,
            latency_samples: 2048,
            transient_backoff_ratio: 0.10,
            overload_backoff_ratio: 0.02,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StageSnapshot {
    pub stage: NetworkStage,
    pub limit: usize,
    pub active: usize,
    pub completed: u64,
    pub transient_failures: u64,
    pub overloads: u64,
    pub deterministic_failures: u64,
    pub policy_rejections: u64,
    pub cancelled: u64,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhaseSnapshot {
    pub phase: TransportPhase,
    pub samples: u64,
    pub transient_failures: u64,
    pub overloads: u64,
    pub deterministic_failures: u64,
    pub policy_rejections: u64,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
}

#[derive(Debug, Default)]
struct PhaseState {
    samples: u64,
    transient_failures: u64,
    overloads: u64,
    deterministic_failures: u64,
    policy_rejections: u64,
    latencies: VecDeque<Duration>,
}

#[derive(Debug)]
struct StageState {
    limit: usize,
    min: usize,
    max: usize,
    active: usize,
    completed: u64,
    transient_failures: u64,
    overloads: u64,
    deterministic_failures: u64,
    policy_rejections: u64,
    cancelled: u64,
    window_samples: usize,
    window_transient: usize,
    window_overload: usize,
    window_saturated: bool,
    pressure_armed: bool,
    cooldown_windows: usize,
    latencies: VecDeque<Duration>,
}

#[derive(Debug)]
struct StageGate {
    state: Mutex<StageState>,
    changed: Notify,
}

#[derive(Debug, Clone)]
pub struct StageController {
    gates: [Arc<StageGate>; STAGE_COUNT],
    phases: Arc<[Mutex<PhaseState>; PHASE_COUNT]>,
    observation_window: usize,
    latency_samples: usize,
    transient_backoff_ratio: f64,
    overload_backoff_ratio: f64,
}

#[derive(Debug)]
pub struct StagePermit {
    controller: StageController,
    stage: NetworkStage,
    started: Instant,
    finished: bool,
}

impl StageController {
    pub fn new(config: StageControllerConfig) -> Self {
        let limits = [
            config.dns,
            config.handshake,
            config.http,
            config.parse,
            config.tail,
        ];
        Self {
            gates: array::from_fn(|index| {
                let limit = limits[index].normalized();
                Arc::new(StageGate {
                    state: Mutex::new(StageState {
                        limit: limit.initial,
                        min: limit.min,
                        max: limit.max,
                        active: 0,
                        completed: 0,
                        transient_failures: 0,
                        overloads: 0,
                        deterministic_failures: 0,
                        policy_rejections: 0,
                        cancelled: 0,
                        window_samples: 0,
                        window_transient: 0,
                        window_overload: 0,
                        window_saturated: false,
                        pressure_armed: true,
                        cooldown_windows: 0,
                        latencies: VecDeque::with_capacity(config.latency_samples.max(64)),
                    }),
                    changed: Notify::new(),
                })
            }),
            phases: Arc::new(array::from_fn(|_| Mutex::new(PhaseState::default()))),
            observation_window: config.observation_window.max(8),
            latency_samples: config.latency_samples.max(64),
            transient_backoff_ratio: config.transient_backoff_ratio.clamp(0.0, 1.0),
            overload_backoff_ratio: config.overload_backoff_ratio.clamp(0.0, 1.0),
        }
    }

    pub async fn acquire(&self, stage: NetworkStage) -> StagePermit {
        let gate = &self.gates[stage.index()];
        loop {
            let notified = gate.changed.notified();
            {
                let mut state = gate.state.lock().expect("stage controller lock poisoned");
                if state.active < state.limit {
                    state.active += 1;
                    if state.active == state.limit {
                        state.window_saturated = true;
                    }
                    return StagePermit {
                        controller: self.clone(),
                        stage,
                        started: Instant::now(),
                        finished: false,
                    };
                }
            }
            notified.await;
        }
    }

    pub fn snapshots(&self) -> Vec<StageSnapshot> {
        NetworkStage::ALL
            .into_iter()
            .map(|stage| self.snapshot(stage))
            .collect()
    }

    pub fn snapshot(&self, stage: NetworkStage) -> StageSnapshot {
        let state = self.gates[stage.index()]
            .state
            .lock()
            .expect("stage controller lock poisoned");
        let (p50, p95, p99) = quantiles(&state.latencies);
        StageSnapshot {
            stage,
            limit: state.limit,
            active: state.active,
            completed: state.completed,
            transient_failures: state.transient_failures,
            overloads: state.overloads,
            deterministic_failures: state.deterministic_failures,
            policy_rejections: state.policy_rejections,
            cancelled: state.cancelled,
            p50,
            p95,
            p99,
        }
    }

    pub fn phase_snapshots(&self) -> Vec<PhaseSnapshot> {
        TransportPhase::ALL
            .into_iter()
            .map(|phase| {
                let state = self.phases[phase as usize]
                    .lock()
                    .expect("phase telemetry lock poisoned");
                let (p50, p95, p99) = quantiles(&state.latencies);
                PhaseSnapshot {
                    phase,
                    samples: state.samples,
                    transient_failures: state.transient_failures,
                    overloads: state.overloads,
                    deterministic_failures: state.deterministic_failures,
                    policy_rejections: state.policy_rejections,
                    p50,
                    p95,
                    p99,
                }
            })
            .collect()
    }

    pub fn record_phase(&self, phase: TransportPhase, elapsed: Duration, outcome: StageOutcome) {
        let mut state = self.phases[phase as usize]
            .lock()
            .expect("phase telemetry lock poisoned");
        state.samples += 1;
        match outcome {
            StageOutcome::Success => {}
            StageOutcome::TransientFailure => state.transient_failures += 1,
            StageOutcome::Overload => state.overloads += 1,
            StageOutcome::DeterministicFailure => state.deterministic_failures += 1,
            StageOutcome::PolicyRejected => state.policy_rejections += 1,
        }
        if state.latencies.len() >= self.latency_samples {
            state.latencies.pop_front();
        }
        state.latencies.push_back(elapsed);
    }

    fn finish(
        &self,
        stage: NetworkStage,
        elapsed: Duration,
        outcome: StageOutcome,
        cancelled: bool,
    ) {
        if stage == NetworkStage::Dns {
            self.record_phase(TransportPhase::Dns, elapsed, outcome);
        }
        let gate = &self.gates[stage.index()];
        let mut state = gate.state.lock().expect("stage controller lock poisoned");
        state.active = state.active.saturating_sub(1);
        state.completed += 1;
        if cancelled {
            state.cancelled += 1;
        }
        match outcome {
            StageOutcome::Success => {}
            StageOutcome::TransientFailure => state.transient_failures += 1,
            StageOutcome::Overload => state.overloads += 1,
            StageOutcome::DeterministicFailure => state.deterministic_failures += 1,
            StageOutcome::PolicyRejected => state.policy_rejections += 1,
        }
        if state.latencies.len() >= self.latency_samples {
            state.latencies.pop_front();
        }
        state.latencies.push_back(elapsed);

        // Policy and deterministic outcomes are observable but never capacity signals.
        if matches!(
            outcome,
            StageOutcome::Success | StageOutcome::TransientFailure | StageOutcome::Overload
        ) {
            state.window_samples += 1;
            state.window_transient += usize::from(outcome == StageOutcome::TransientFailure);
            state.window_overload += usize::from(outcome == StageOutcome::Overload);
        }
        if state.window_samples >= self.observation_window {
            let transient = state.window_transient as f64 / state.window_samples as f64;
            let overload = state.window_overload as f64 / state.window_samples as f64;
            let overload_pressure = overload >= self.overload_backoff_ratio;
            let transient_pressure = transient >= self.transient_backoff_ratio;
            let pressured = overload_pressure || transient_pressure;

            // A public-web timeout is often a property of one remote host, not
            // proof that this process is overloaded.  Apply AIMD only once for
            // a continuous pressure episode, and re-arm it after a healthy
            // observation window.  Without this hysteresis a heterogeneous
            // domain list can ratchet a busy stage down to its minimum forever.
            if state.window_saturated && state.pressure_armed && overload_pressure {
                state.limit = (state.limit * 3 / 4).max(state.min);
                state.pressure_armed = false;
                state.cooldown_windows = 2;
            } else if state.window_saturated && state.pressure_armed && transient_pressure {
                state.limit = (state.limit * 7 / 8).max(state.min);
                state.pressure_armed = false;
                state.cooldown_windows = 1;
            } else if !pressured && state.cooldown_windows > 0 {
                state.cooldown_windows -= 1;
            } else if !pressured {
                state.pressure_armed = true;
                if state.window_saturated && state.limit < state.max {
                    // Proportional additive probing converges on large hosts
                    // without turning into a permanent one-slot-at-a-time scan.
                    state.limit = (state.limit + (state.limit / 32).max(1)).min(state.max);
                }
            }
            state.window_samples = 0;
            state.window_transient = 0;
            state.window_overload = 0;
            state.window_saturated = false;
        }
        drop(state);
        gate.changed.notify_waiters();
    }
}

impl Default for StageController {
    fn default() -> Self {
        Self::new(StageControllerConfig::default())
    }
}

impl StagePermit {
    pub fn finish(mut self, outcome: StageOutcome) {
        self.finished = true;
        self.controller
            .finish(self.stage, self.started.elapsed(), outcome, false);
    }
}

impl Drop for StagePermit {
    fn drop(&mut self) {
        if !self.finished {
            self.finished = true;
            self.controller.finish(
                self.stage,
                self.started.elapsed(),
                StageOutcome::TransientFailure,
                true,
            );
        }
    }
}

fn quantiles(samples: &VecDeque<Duration>) -> (Duration, Duration, Duration) {
    if samples.is_empty() {
        return (Duration::ZERO, Duration::ZERO, Duration::ZERO);
    }
    let mut sorted = samples.iter().copied().collect::<Vec<_>>();
    sorted.sort_unstable();
    let at = |percent: usize| sorted[((sorted.len() - 1) * percent) / 100];
    (at(50), at(95), at(99))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> StageControllerConfig {
        let limit = StageLimit {
            initial: 4,
            min: 1,
            max: 8,
        };
        StageControllerConfig {
            dns: limit,
            handshake: limit,
            http: limit,
            parse: limit,
            tail: limit,
            observation_window: 8,
            latency_samples: 64,
            transient_backoff_ratio: 0.10,
            overload_backoff_ratio: 0.02,
        }
    }

    #[tokio::test]
    async fn stages_adapt_independently_and_policy_errors_do_not_backoff() {
        let controller = StageController::new(config());
        for _ in 0..2 {
            let mut permits = Vec::with_capacity(4);
            for _ in 0..4 {
                permits.push(controller.acquire(NetworkStage::Dns).await);
            }
            for permit in permits {
                permit.finish(StageOutcome::TransientFailure);
            }
        }
        for _ in 0..8 {
            controller
                .acquire(NetworkStage::Http)
                .await
                .finish(StageOutcome::PolicyRejected);
        }
        assert!(controller.snapshot(NetworkStage::Dns).limit < 4);
        assert_eq!(controller.snapshot(NetworkStage::Http).limit, 4);
        assert_eq!(controller.snapshot(NetworkStage::Http).policy_rejections, 8);
    }

    #[tokio::test]
    async fn cancellation_releases_capacity_and_is_not_fake_success() {
        let controller = StageController::new(config());
        let permit = controller.acquire(NetworkStage::Tail).await;
        drop(permit);
        let snapshot = controller.snapshot(NetworkStage::Tail);
        assert_eq!(snapshot.active, 0);
        assert_eq!(snapshot.completed, 1);
        assert_eq!(snapshot.cancelled, 1);
        assert_eq!(snapshot.transient_failures, 1);
    }

    #[tokio::test]
    async fn persistent_remote_failures_do_not_ratchet_stage_to_minimum() {
        let controller = StageController::new(config());
        for _ in 0..80 {
            let limit = controller.snapshot(NetworkStage::Dns).limit;
            let mut permits = Vec::with_capacity(limit);
            for _ in 0..limit {
                permits.push(controller.acquire(NetworkStage::Dns).await);
            }
            for permit in permits {
                permit.finish(StageOutcome::TransientFailure);
            }
        }
        assert_eq!(controller.snapshot(NetworkStage::Dns).limit, 3);
    }
}
