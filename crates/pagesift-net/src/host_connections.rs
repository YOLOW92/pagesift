use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

const WINDOW: u32 = 16;
const EWMA_ALPHA: f64 = 0.20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostOutcome {
    Success,
    Failure,
    Overload,
    Neutral,
}

/// Runtime state of one origin's self-tuning connection controller.
#[derive(Debug, Clone, PartialEq)]
pub struct HostConnectionStats {
    pub origin: String,
    pub hot_target: usize,
    pub active: usize,
    pub max_hot: usize,
    pub completed: u64,
    pub failed: u64,
    pub overloaded: u64,
    pub cancelled: u64,
    pub reused: u64,
    pub connected: u64,
    pub service_ewma: Duration,
    pub connect_ewma: Duration,
}

#[derive(Debug)]
struct State {
    adaptive: bool,
    target: usize,
    max: usize,
    active: usize,
    completed: u64,
    failed: u64,
    overloaded: u64,
    cancelled: u64,
    reused: u64,
    connected: u64,
    service_ewma_us: f64,
    connect_ewma_us: f64,
    best_service_us: f64,
    window_samples: u32,
    window_failures: u32,
    window_saturated: bool,
    cooldown: u32,
}

/// A cancellation-safe, per-origin AIMD controller. It deliberately has no
/// background scanner: only completed requests change the watermark.
pub(crate) struct HostConnectionControl {
    state: Mutex<State>,
    changed: Notify,
}

pub(crate) struct HostPermit {
    control: Arc<HostConnectionControl>,
    started: Instant,
    reused: bool,
    connect_time: Option<Duration>,
    finished: bool,
}

impl HostConnectionControl {
    pub(crate) fn new(adaptive: bool, max: usize) -> Arc<Self> {
        let max = max.max(1);
        Arc::new(Self {
            state: Mutex::new(State {
                adaptive,
                target: if adaptive { max.min(2) } else { max },
                max,
                active: 0,
                completed: 0,
                failed: 0,
                overloaded: 0,
                cancelled: 0,
                reused: 0,
                connected: 0,
                service_ewma_us: 0.0,
                connect_ewma_us: 0.0,
                best_service_us: 0.0,
                window_samples: 0,
                window_failures: 0,
                window_saturated: false,
                cooldown: 0,
            }),
            changed: Notify::new(),
        })
    }

    pub(crate) async fn acquire(self: &Arc<Self>) -> HostPermit {
        loop {
            let notified = self.changed.notified();
            {
                let mut state = self.state.lock().expect("host connection lock poisoned");
                if state.active < state.target {
                    state.active += 1;
                    if state.active == state.target {
                        state.window_saturated = true;
                    }
                    return HostPermit {
                        control: self.clone(),
                        started: Instant::now(),
                        reused: false,
                        connect_time: None,
                        finished: false,
                    };
                }
            }
            notified.await;
        }
    }

    pub(crate) fn target(&self) -> usize {
        self.state
            .lock()
            .expect("host connection lock poisoned")
            .target
    }

    pub(crate) fn stats(&self, origin: String) -> HostConnectionStats {
        let state = self.state.lock().expect("host connection lock poisoned");
        HostConnectionStats {
            origin,
            hot_target: state.target,
            active: state.active,
            max_hot: state.max,
            completed: state.completed,
            failed: state.failed,
            overloaded: state.overloaded,
            cancelled: state.cancelled,
            reused: state.reused,
            connected: state.connected,
            service_ewma: micros(state.service_ewma_us),
            connect_ewma: micros(state.connect_ewma_us),
        }
    }

    fn complete(
        &self,
        outcome: HostOutcome,
        elapsed: Duration,
        reused: bool,
        connect_time: Option<Duration>,
        cancelled: bool,
    ) {
        let mut state = self.state.lock().expect("host connection lock poisoned");
        state.active = state.active.saturating_sub(1);
        if outcome == HostOutcome::Neutral {
            drop(state);
            self.changed.notify_waiters();
            return;
        }

        state.completed += 1;
        state.window_samples += 1;
        if reused {
            state.reused += 1;
        } else {
            state.connected += 1;
        }
        if cancelled {
            state.cancelled += 1;
        }
        let elapsed_us = elapsed.as_secs_f64() * 1_000_000.0;
        state.service_ewma_us = ewma(state.service_ewma_us, elapsed_us);
        if let Some(duration) = connect_time {
            state.connect_ewma_us =
                ewma(state.connect_ewma_us, duration.as_secs_f64() * 1_000_000.0);
        }

        match outcome {
            HostOutcome::Success => {}
            HostOutcome::Failure => {
                state.failed += 1;
                state.window_failures += 1;
            }
            HostOutcome::Overload => {
                state.failed += 1;
                state.overloaded += 1;
                state.window_failures += 1;
                if state.adaptive && state.cooldown == 0 {
                    // 429/503 are direct overload signals and should back off now,
                    // without waiting for a whole observation window. Multiple
                    // responses from the same overshoot episode must not stack.
                    state.target = state.target.saturating_sub(1).max(1);
                    state.cooldown = WINDOW;
                    reset_window(&mut state);
                }
            }
            HostOutcome::Neutral => unreachable!(),
        }

        if state.adaptive && state.window_samples >= WINDOW {
            if state.cooldown > 0 {
                state.cooldown = state.cooldown.saturating_sub(WINDOW);
                reset_window(&mut state);
                drop(state);
                self.changed.notify_waiters();
                return;
            }
            let error_rate = state.window_failures as f64 / state.window_samples as f64;
            // Sub-millisecond loopback samples are dominated by scheduler and
            // clock noise. Use service-time regression only once the signal is
            // large enough to be meaningful.
            let latency_regressed = state.best_service_us >= 1_000.0
                && state.service_ewma_us > state.best_service_us * 1.50;
            if error_rate >= 0.25 {
                state.target = (state.target / 2).max(1);
                state.cooldown = WINDOW;
            } else if error_rate > 0.05 || latency_regressed {
                state.target = state.target.saturating_sub(1).max(1);
                state.cooldown = WINDOW / 2;
            } else if state.window_saturated && state.cooldown == 0 && state.target < state.max {
                // Additive probing keeps growth bounded and avoids endless sweeps.
                state.target += 1;
            }
            if error_rate == 0.0
                && (state.best_service_us == 0.0 || state.service_ewma_us < state.best_service_us)
            {
                state.best_service_us = state.service_ewma_us;
            }
            reset_window(&mut state);
        }
        drop(state);
        self.changed.notify_waiters();
    }
}

impl HostPermit {
    pub(crate) fn reused(&mut self) {
        self.reused = true;
    }

    pub(crate) fn connected_in(&mut self, elapsed: Duration) {
        self.reused = false;
        self.connect_time = Some(elapsed);
    }

    pub(crate) fn finish(mut self, outcome: HostOutcome) {
        self.finished = true;
        self.control.complete(
            outcome,
            self.started.elapsed(),
            self.reused,
            self.connect_time,
            false,
        );
    }

    pub(crate) fn abandon(mut self) {
        self.finished = true;
        self.control.complete(
            HostOutcome::Neutral,
            self.started.elapsed(),
            self.reused,
            self.connect_time,
            false,
        );
    }
}

impl Drop for HostPermit {
    fn drop(&mut self) {
        if !self.finished {
            self.finished = true;
            self.control.complete(
                HostOutcome::Failure,
                self.started.elapsed(),
                self.reused,
                self.connect_time,
                true,
            );
        }
    }
}

fn reset_window(state: &mut State) {
    state.window_samples = 0;
    state.window_failures = 0;
    state.window_saturated = false;
}

fn ewma(current: f64, sample: f64) -> f64 {
    if current == 0.0 {
        sample
    } else {
        current * (1.0 - EWMA_ALPHA) + sample * EWMA_ALPHA
    }
}

fn micros(value: f64) -> Duration {
    Duration::from_micros(value.max(0.0).min(u64::MAX as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn healthy_saturated_host_grows_without_scanning() {
        let control = HostConnectionControl::new(true, 8);
        for _ in 0..8 {
            let a = control.acquire().await;
            let b = control.acquire().await;
            a.finish(HostOutcome::Success);
            b.finish(HostOutcome::Success);
        }
        let stats = control.stats("http://fast.test".into());
        assert!(stats.hot_target > 2, "healthy saturated host should grow");
        assert_eq!(stats.failed, 0);
    }

    #[tokio::test]
    async fn overload_shrinks_only_its_own_host() {
        let slow = HostConnectionControl::new(true, 8);
        let fast = HostConnectionControl::new(true, 8);
        for _ in 0..16 {
            let permit = fast.acquire().await;
            permit.finish(HostOutcome::Success);
        }
        let permit = slow.acquire().await;
        permit.finish(HostOutcome::Overload);
        assert_eq!(slow.stats("slow".into()).hot_target, 1);
        assert_eq!(fast.stats("fast".into()).hot_target, 2);
    }

    #[tokio::test]
    async fn one_overload_episode_causes_only_one_backoff() {
        let control = HostConnectionControl::new(true, 8);
        while control.stats("host".into()).hot_target < 5 {
            let target = control.stats("host".into()).hot_target;
            for _ in 0..(WINDOW as usize / target).max(1) {
                let mut permits = Vec::new();
                for _ in 0..target {
                    permits.push(control.acquire().await);
                }
                for permit in permits {
                    permit.finish(HostOutcome::Success);
                }
            }
        }
        let mut overshoot = Vec::new();
        for _ in 0..5 {
            overshoot.push(control.acquire().await);
        }
        for permit in overshoot {
            permit.finish(HostOutcome::Overload);
        }
        assert_eq!(control.stats("host".into()).hot_target, 4);
    }

    #[tokio::test]
    async fn cancelled_acquire_is_released_and_counted() {
        let control = HostConnectionControl::new(true, 2);
        let permit = control.acquire().await;
        drop(permit);
        let stats = control.stats("cancelled".into());
        assert_eq!(stats.active, 0);
        assert_eq!(stats.cancelled, 1);
        assert_eq!(stats.failed, 1);
    }
}
