//! Dynamic AIMD / BBR-style gradient ceiling detection concurrency controller.

use super::telemetry::HardwareTopology;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Operating state for BBR-style gradient ceiling detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerState {
    /// Exploration phase: probing upward (Additive Increase) to discover maximum hardware throughput.
    Probing,
    /// Ceiling detected: throughput derivative turned flat/negative or latency spiked.
    CeilingDetected,
    /// Draining / Backing off: retreating to the recorded peak throughput sweet spot.
    Draining,
    /// Steady cruising: locked around the optimal sweet spot with gentle low-frequency probing.
    SteadyLock,
}

/// Configuration for dynamic AIMD concurrency adjustment.
#[derive(Debug, Clone)]
pub struct ControllerConfig {
    pub min_concurrency: usize,
    pub max_concurrency: usize,
    pub additive_step: usize,
    pub backoff_pct: f64,
    pub memory_safe_threshold: f64,
    pub memory_warning_threshold: f64,
    pub memory_critical_threshold: f64,
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            min_concurrency: 4,
            max_concurrency: 256,
            additive_step: 4,
            backoff_pct: 0.75,
            memory_safe_threshold: 0.70,
            memory_warning_threshold: 0.85,
            memory_critical_threshold: 0.92,
        }
    }
}

#[derive(Debug)]
struct ControllerInternalState {
    mode: ControllerState,
    prev_concurrency: usize,
    prev_qps: f64,
    best_qps: f64,
    best_concurrency: usize,
    steady_ticks: usize,
}

/// Dynamic AIMD Concurrency and Site Throttle Controller with Gradient Ceiling Detection.
pub struct AimdController {
    current_concurrency: AtomicUsize,
    current_active_sites: AtomicUsize,
    config: ControllerConfig,
    internal: Mutex<ControllerInternalState>,
}

impl AimdController {
    /// Creates a controller initialized from detected hardware topology.
    pub fn from_topology(topo: &HardwareTopology) -> Self {
        let config = ControllerConfig {
            min_concurrency: topo.min_concurrency,
            max_concurrency: topo.max_concurrency,
            additive_step: (topo.logical_cores / 2).clamp(2, 8),
            backoff_pct: 0.75,
            memory_safe_threshold: 0.70,
            memory_warning_threshold: 0.85,
            memory_critical_threshold: 0.92,
        };

        let init_concurrency = topo.base_concurrency;
        let init_sites = topo.active_sites_cap;

        Self {
            current_concurrency: AtomicUsize::new(init_concurrency),
            current_active_sites: AtomicUsize::new(init_sites),
            internal: Mutex::new(ControllerInternalState {
                mode: ControllerState::Probing,
                prev_concurrency: init_concurrency,
                prev_qps: 0.0,
                best_qps: 0.0,
                best_concurrency: init_concurrency,
                steady_ticks: 0,
            }),
            config,
        }
    }

    /// Returns current active concurrency limit.
    #[inline]
    pub fn concurrency(&self) -> usize {
        self.current_concurrency.load(Ordering::Relaxed)
    }

    /// Returns current target active sites capacity.
    #[inline]
    pub fn active_sites_cap(&self) -> usize {
        self.current_active_sites.load(Ordering::Relaxed)
    }

    /// Returns the current controller state.
    pub fn state(&self) -> ControllerState {
        self.internal.lock().unwrap().mode
    }

    /// Returns the best recorded concurrency sweet spot.
    pub fn best_concurrency(&self) -> usize {
        self.internal.lock().unwrap().best_concurrency
    }

    /// Full BBR-style gradient ceiling detection evaluation.
    ///
    /// - Checks memory safety thresholds (emergency / warning throttling).
    /// - Evaluates QPS gradient: ΔQPS / ΔC.
    /// - If increasing concurrency results in flat or declining QPS (or dispatch latency spikes),
    ///   detects the hardware performance ceiling, retreats to the optimal knee-point sweet spot,
    ///   and locks in.
    pub fn evaluate_and_adjust(
        &self,
        rss_bytes: u64,
        budget_bytes: u64,
        instant_qps: f64,
        dispatch_latency_ms: f64,
        error_rate_pct: f64,
    ) -> (usize, ControllerState) {
        let mem_ratio = if budget_bytes > 0 {
            rss_bytes as f64 / budget_bytes as f64
        } else {
            0.0
        };

        let curr = self.current_concurrency.load(Ordering::Relaxed);
        let mut inner = self.internal.lock().unwrap();

        // 1. Safety overrides: Memory & Error rate
        if mem_ratio >= self.config.memory_critical_threshold {
            let new_c = ((curr as f64) * 0.50).max(self.config.min_concurrency as f64) as usize;
            inner.mode = ControllerState::Draining;
            self.store_concurrency(new_c);
            return (new_c, ControllerState::Draining);
        }

        if mem_ratio >= self.config.memory_warning_threshold || error_rate_pct > 5.0 {
            let new_c = ((curr as f64) * self.config.backoff_pct)
                .max(self.config.min_concurrency as f64) as usize;
            inner.mode = ControllerState::Draining;
            self.store_concurrency(new_c);
            return (new_c, ControllerState::Draining);
        }

        // 2. Gradient & Ceiling Detection
        let mut new_c = curr;
        let delta_c = curr as isize - inner.prev_concurrency as isize;
        let delta_qps = instant_qps - inner.prev_qps;

        match inner.mode {
            ControllerState::Probing => {
                // If QPS improved with higher concurrency, update best recorded sweet spot
                if instant_qps > inner.best_qps * 1.01 {
                    inner.best_qps = instant_qps;
                    inner.best_concurrency = curr;
                }

                // Check for Ceiling / Kneepoint:
                // Concurrency increased (delta_c > 0), but QPS did NOT increase or dropped,
                // OR dispatch latency spiked above acceptable threshold (> 3.5ms on loopback)
                let qps_stalled_or_dropped =
                    delta_c > 0 && delta_qps < -200.0 && curr > self.config.min_concurrency;
                let latency_spiked = dispatch_latency_ms > 3.5;

                if (qps_stalled_or_dropped || latency_spiked) && inner.best_concurrency > 0 {
                    // CEILING DETECTED!
                    inner.mode = ControllerState::CeilingDetected;
                    // Immediately retreat to the best recorded knee-point concurrency
                    new_c = inner.best_concurrency;
                    inner.mode = ControllerState::SteadyLock;
                    inner.steady_ticks = 0;
                } else if curr < self.config.max_concurrency && dispatch_latency_ms < 2.5 {
                    // Safe to continue probing upward
                    new_c = (curr + self.config.additive_step).min(self.config.max_concurrency);
                } else {
                    // Hit max_concurrency ceiling
                    inner.mode = ControllerState::SteadyLock;
                }
            }
            ControllerState::CeilingDetected | ControllerState::Draining => {
                new_c = inner
                    .best_concurrency
                    .clamp(self.config.min_concurrency, self.config.max_concurrency);
                inner.mode = ControllerState::SteadyLock;
                inner.steady_ticks = 0;
            }
            ControllerState::SteadyLock => {
                inner.steady_ticks += 1;
                // Cruise locked at best_concurrency.
                // Every 30 ticks (~30s), perform a gentle 1-step exploratory probe to check if conditions changed
                if inner.steady_ticks >= 30 {
                    inner.steady_ticks = 0;
                    inner.mode = ControllerState::Probing;
                    new_c = (curr + self.config.additive_step / 2).min(self.config.max_concurrency);
                } else {
                    new_c = inner.best_concurrency;
                }
            }
        }

        let clamped = new_c.clamp(self.config.min_concurrency, self.config.max_concurrency);
        inner.prev_concurrency = curr;
        inner.prev_qps = instant_qps;
        let mode = inner.mode;
        drop(inner);

        self.store_concurrency(clamped);
        (clamped, mode)
    }

    /// Legacy / simple adjust wrapper.
    pub fn adjust(
        &self,
        rss_bytes: u64,
        budget_bytes: u64,
        error_rate_pct: f64,
        dispatch_latency_ms: f64,
    ) -> usize {
        let (conc, _) = self.evaluate_and_adjust(
            rss_bytes,
            budget_bytes,
            0.0,
            dispatch_latency_ms,
            error_rate_pct,
        );
        conc
    }

    fn store_concurrency(&self, c: usize) {
        self.current_concurrency.store(c, Ordering::Relaxed);
        let sites = (c * 2).clamp(
            self.config.min_concurrency * 2,
            self.config.max_concurrency * 4,
        );
        self.current_active_sites.store(sites, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aimd_ceiling_detection() {
        let topo = HardwareTopology::detect();
        let ctrl = AimdController::from_topology(&topo);
        let init = ctrl.concurrency();

        // Tick 1: QPS is 20,000 -> Should probe up
        let (c1, mode1) = ctrl.evaluate_and_adjust(100_000, 1_000_000_000, 20_000.0, 1.0, 0.0);
        assert!(c1 >= init);
        assert_eq!(mode1, ControllerState::Probing);

        // Tick 2: QPS rises to 35,000 at higher concurrency -> Keep probing
        let (c2, mode2) = ctrl.evaluate_and_adjust(100_000, 1_000_000_000, 35_000.0, 1.0, 0.0);
        assert!(c2 >= c1);
        assert_eq!(mode2, ControllerState::Probing);

        // Tick 3: Concurrency increased to 144, but QPS drops to 30,000 (CEILING DETECTED!)
        let (c3, mode3) = ctrl.evaluate_and_adjust(100_000, 1_000_000_000, 30_000.0, 1.0, 0.0);
        // Should immediately retreat to the best concurrency (c1 = 136) and lock in!
        assert_eq!(c3, c1);
        assert_eq!(mode3, ControllerState::SteadyLock);
    }
}
