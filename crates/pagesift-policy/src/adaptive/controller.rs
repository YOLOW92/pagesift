//! Dynamic AIMD / Response-surface autonomous performance tuning engine.

use super::knobs::{AtomicTuningKnobs, TuningKnobs};
use super::surface::{ResponseSurfaceOptimizer, SurfacePoint};
use super::telemetry::{
    CpuSaturationTracker, HardwareTopology, QuantileLatencyTracker, RssDerivativeTracker,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Operating state for BBR-style gradient ceiling detection and surface optimization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControllerState {
    /// Exploration phase: probing upward (Additive Increase / CCD) to discover maximum hardware throughput.
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
    /// Samples held after a concurrency change before judging its effect.
    pub settle_ticks: usize,
    /// Consecutive healthy samples required before probing upward.
    pub good_samples_before_probe: usize,
    /// Consecutive regressions required before abandoning a probe.
    pub bad_samples_before_backoff: usize,
    /// Stable samples between low-frequency re-probes.
    pub steady_probe_interval: usize,
    /// Relative QPS loss that marks a probe as regressing.
    pub qps_drop_ratio: f64,
    /// Relative latency growth that marks a probe as saturated.
    pub latency_growth_ratio: f64,
    /// Do not probe upward once the PageSift process itself reaches this CPU level.
    pub process_cpu_ceiling_pct: f64,
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
            settle_ticks: 3,
            good_samples_before_probe: 3,
            bad_samples_before_backoff: 2,
            steady_probe_interval: 12,
            qps_drop_ratio: 0.03,
            latency_growth_ratio: 1.50,
            process_cpu_ceiling_pct: 92.0,
        }
    }
}

#[derive(Debug)]
struct ControllerInternalState {
    mode: ControllerState,
    best_qps: f64,
    best_concurrency: usize,
    steady_ticks: usize,
    settle_ticks_remaining: usize,
    consecutive_good: usize,
    consecutive_bad: usize,
    ewma_qps: f64,
    ewma_latency_ms: f64,
    probe_baseline_qps: f64,
    probe_baseline_latency_ms: f64,
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
            additive_step: topo.cpu_topology.physical_cores_per_numa().clamp(2, 32),
            backoff_pct: 0.75,
            memory_safe_threshold: 0.70,
            memory_warning_threshold: 0.85,
            memory_critical_threshold: 0.92,
            ..ControllerConfig::default()
        };

        Self::new(topo.base_concurrency, topo.active_sites_cap, config)
    }

    pub fn new(initial_concurrency: usize, active_sites: usize, config: ControllerConfig) -> Self {
        let init_concurrency =
            initial_concurrency.clamp(config.min_concurrency, config.max_concurrency);

        Self {
            current_concurrency: AtomicUsize::new(init_concurrency),
            current_active_sites: AtomicUsize::new(active_sites.max(1)),
            internal: Mutex::new(ControllerInternalState {
                mode: ControllerState::Probing,
                best_qps: 0.0,
                best_concurrency: init_concurrency,
                steady_ticks: 0,
                settle_ticks_remaining: 0,
                consecutive_good: 0,
                consecutive_bad: 0,
                ewma_qps: 0.0,
                ewma_latency_ms: 0.0,
                probe_baseline_qps: 0.0,
                probe_baseline_latency_ms: 0.0,
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

    /// Gradient ceiling detection evaluation using adaptive latency limits.
    pub fn evaluate_and_adjust(
        &self,
        rss_bytes: u64,
        budget_bytes: u64,
        instant_qps: f64,
        dispatch_latency_ms: f64,
        error_rate_pct: f64,
    ) -> (usize, ControllerState) {
        self.evaluate_and_adjust_with_cpu(
            rss_bytes,
            budget_bytes,
            instant_qps,
            dispatch_latency_ms,
            error_rate_pct,
            0.0,
        )
    }

    /// Stable adaptive control using only PageSift process CPU, never host-wide CPU.
    pub fn evaluate_and_adjust_with_cpu(
        &self,
        rss_bytes: u64,
        budget_bytes: u64,
        instant_qps: f64,
        dispatch_latency_ms: f64,
        error_rate_pct: f64,
        process_cpu_pct: f64,
    ) -> (usize, ControllerState) {
        let mem_ratio = if budget_bytes > 0 {
            rss_bytes as f64 / budget_bytes as f64
        } else {
            0.0
        };

        let curr = self.current_concurrency.load(Ordering::Relaxed);
        let mut inner = self.internal.lock().unwrap();

        if instant_qps.is_finite() && instant_qps >= 0.0 {
            inner.ewma_qps = ewma(inner.ewma_qps, instant_qps, 0.25);
        }
        if dispatch_latency_ms.is_finite() && dispatch_latency_ms >= 0.0 {
            inner.ewma_latency_ms = ewma(inner.ewma_latency_ms, dispatch_latency_ms, 0.25);
        }

        // 1. Safety overrides: Memory & Error rate
        if mem_ratio >= self.config.memory_critical_threshold {
            let new_c = ((curr as f64) * 0.50).max(self.config.min_concurrency as f64) as usize;
            inner.mode = ControllerState::Draining;
            inner.settle_ticks_remaining = self.config.settle_ticks;
            inner.consecutive_bad = 0;
            inner.consecutive_good = 0;
            self.store_concurrency(new_c);
            return (new_c, ControllerState::Draining);
        }

        if mem_ratio >= self.config.memory_warning_threshold || error_rate_pct > 1.0 {
            let new_c = ((curr as f64) * self.config.backoff_pct)
                .max(self.config.min_concurrency as f64) as usize;
            inner.mode = ControllerState::Draining;
            inner.settle_ticks_remaining = self.config.settle_ticks;
            inner.consecutive_bad = 0;
            inner.consecutive_good = 0;
            self.store_concurrency(new_c);
            return (new_c, ControllerState::Draining);
        }

        // Do not judge a new NUMA/connection-pool state before it has settled.
        if inner.settle_ticks_remaining > 0 {
            inner.settle_ticks_remaining -= 1;
            return (curr, inner.mode);
        }

        // 2. Gradient & Ceiling Detection
        let mut new_c = curr;
        let qps_regressed = inner.probe_baseline_qps > 0.0
            && inner.ewma_qps < inner.probe_baseline_qps * (1.0 - self.config.qps_drop_ratio);
        let latency_regressed = inner.probe_baseline_latency_ms > 0.0
            && inner.ewma_latency_ms
                > (inner.probe_baseline_latency_ms * self.config.latency_growth_ratio).max(2.0);
        let cpu_saturated = process_cpu_pct >= self.config.process_cpu_ceiling_pct;

        match inner.mode {
            ControllerState::Probing => {
                if inner.ewma_qps > inner.best_qps * 1.01 {
                    inner.best_qps = inner.ewma_qps;
                    inner.best_concurrency = curr;
                }

                if qps_regressed || latency_regressed {
                    inner.consecutive_bad += 1;
                    inner.consecutive_good = 0;
                } else {
                    inner.consecutive_good += 1;
                    inner.consecutive_bad = 0;
                }

                if inner.consecutive_bad >= self.config.bad_samples_before_backoff
                    && inner.best_concurrency > 0
                {
                    inner.mode = ControllerState::CeilingDetected;
                    new_c = inner.best_concurrency;
                    inner.mode = ControllerState::SteadyLock;
                    inner.steady_ticks = 0;
                    inner.settle_ticks_remaining = self.config.settle_ticks;
                    inner.consecutive_bad = 0;
                    inner.consecutive_good = 0;
                } else if inner.consecutive_good >= self.config.good_samples_before_probe
                    && curr < self.config.max_concurrency
                    && !cpu_saturated
                {
                    inner.probe_baseline_qps = inner.ewma_qps;
                    inner.probe_baseline_latency_ms = inner.ewma_latency_ms;
                    new_c = (curr + self.config.additive_step).min(self.config.max_concurrency);
                    inner.settle_ticks_remaining = self.config.settle_ticks;
                    inner.consecutive_good = 0;
                } else if cpu_saturated {
                    inner.mode = ControllerState::SteadyLock;
                    inner.steady_ticks = 0;
                }
            }
            ControllerState::CeilingDetected => {
                new_c = inner
                    .best_concurrency
                    .clamp(self.config.min_concurrency, self.config.max_concurrency);
                inner.mode = ControllerState::SteadyLock;
                inner.steady_ticks = 0;
                inner.settle_ticks_remaining = self.config.settle_ticks;
            }
            ControllerState::Draining => {
                // Hold the backed-off value. Returning immediately to the historic
                // peak would cancel the safety action that put us in Draining.
                inner.mode = ControllerState::SteadyLock;
                inner.steady_ticks = 0;
            }
            ControllerState::SteadyLock => {
                inner.steady_ticks += 1;
                if inner.steady_ticks >= self.config.steady_probe_interval {
                    inner.mode = ControllerState::Probing;
                    inner.steady_ticks = 0;
                    inner.consecutive_good = 0;
                    inner.consecutive_bad = 0;
                    inner.probe_baseline_qps = inner.ewma_qps;
                    inner.probe_baseline_latency_ms = inner.ewma_latency_ms;
                }
            }
        }

        self.store_concurrency(new_c);
        (new_c, inner.mode)
    }

    #[inline]
    fn store_concurrency(&self, c: usize) {
        self.current_concurrency.store(c, Ordering::Relaxed);
    }
}

fn ewma(previous: f64, sample: f64, alpha: f64) -> f64 {
    if previous <= 0.0 {
        sample
    } else {
        previous * (1.0 - alpha) + sample * alpha
    }
}

/// Comprehensive performance bottleneck diagnosis output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceDiagnosis {
    pub logical_cores: usize,
    pub physical_cores: usize,
    pub worker_cores: usize,
    pub runtime_workers: usize,
    pub sockets: usize,
    pub numa_nodes: usize,
    pub threads_per_core: usize,
    pub total_memory_mb: f64,
    pub process_rss_mb: f64,
    pub cpu_saturation_pct: f64,
    pub process_cpu_time_s: f64,
    pub p50_latency_ms: f64,
    pub p95_latency_ms: f64,
    pub p99_latency_ms: f64,
    pub active_concurrency: usize,
    pub active_sites: usize,
    pub fd_limit: usize,
    pub primary_bottleneck: String,
    pub recommendation: String,
}

/// Three-Layer Autonomous Performance Tuning Engine.
///
/// - Layer 1: Physical Observation Layer (CPU saturation, dRSS/dt, quantile latency tracking)
/// - Layer 2: Decision & Response Surface Layer (Quadratic regression vertex & CCD)
/// - Layer 3: Multi-Parameter Execution Layer (10 dynamic knobs)
pub struct AutonomousTuningEngine {
    pub topo: HardwareTopology,
    pub knobs: Arc<AtomicTuningKnobs>,
    pub latencies: Arc<QuantileLatencyTracker>,
    pub cpu_tracker: Arc<CpuSaturationTracker>,
    pub rss_tracker: Arc<RssDerivativeTracker>,
    pub controller: AimdController,
    optimizer: Mutex<ResponseSurfaceOptimizer>,
    start_time: Instant,
}

impl AutonomousTuningEngine {
    /// Initializes the autonomous tuning engine by sensing the host hardware environment.
    pub fn new() -> Self {
        let topo = HardwareTopology::detect();
        let total_mem_mb = (HardwareTopology::read_memory().total_bytes / (1024 * 1024)) as f64;

        let initial_knobs = TuningKnobs {
            global_concurrency: topo.base_concurrency,
            worker_pool_size: topo.runtime_workers,
            dispatch_shards: topo.cpu_topology.recommended_dispatch_shards(),
            active_sites_cap: topo.active_sites_cap,
            per_host_concurrency: (topo.worker_cores / 4).clamp(2, 16),
            inflight_memory_budget_bytes: topo.inflight_payload_budget_bytes,
            queue_capacity: topo.base_concurrency * 16,
            http_pool_idle_per_host: topo.base_concurrency * 2,
            http_pool_idle_timeout_secs: 90,
            batch_size: (topo.worker_cores * 16).clamp(32, 512),
            connect_timeout_ms: 3500,
            request_timeout_ms: 10000,
            rendering_pool_concurrency: (topo.worker_cores / 8).clamp(1, 16),
        };

        let optimizer = ResponseSurfaceOptimizer::new(
            topo.min_concurrency,
            topo.max_concurrency,
            total_mem_mb,
            topo.fd_limit,
        );
        let controller = AimdController::from_topology(&topo);

        Self {
            knobs: Arc::new(AtomicTuningKnobs::new(initial_knobs)),
            latencies: Arc::new(QuantileLatencyTracker::new(256)),
            cpu_tracker: Arc::new(CpuSaturationTracker::with_assigned_cores(topo.worker_cores)),
            rss_tracker: Arc::new(RssDerivativeTracker::new()),
            controller,
            optimizer: Mutex::new(optimizer),
            topo,
            start_time: Instant::now(),
        }
    }

    /// Evaluates current metrics and adjusts the 10-dimensional execution knobs.
    pub fn tick(&self, instant_qps: f64, error_rate_pct: f64, current_fds: usize) -> TuningKnobs {
        let (rss_mb, _d_rss) = self.rss_tracker.sample();
        let cpu = self.cpu_tracker.sample();
        let cpu_pct = cpu.utilization_pct;
        let (_p50, p95, _p99) = self.latencies.quantiles();

        let mut opt = self.optimizer.lock().unwrap();
        let curr_c = self.knobs.global_concurrency();

        // 1. Check safety status
        let safety = opt.evaluate_safety(rss_mb, error_rate_pct, p95, current_fds);

        // 2. Only safe, non-zero observations may influence the response surface.
        if safety == super::surface::SafetyStatus::Optimal && instant_qps > 0.0 {
            opt.record_observation(SurfacePoint {
                concurrency: curr_c,
                qps: instant_qps,
                p95_latency_ms: p95,
                cpu_pct,
                rss_mb,
                timestamp_ms: self.start_time.elapsed().as_millis() as u64,
            });
        }

        // 3. Compute target concurrency via CCD & Response Surface fit
        let memory = HardwareTopology::read_memory();
        let (target_c, _state) = self.controller.evaluate_and_adjust_with_cpu(
            (rss_mb * 1024.0 * 1024.0) as u64,
            memory.total_bytes,
            instant_qps,
            p95,
            error_rate_pct,
            cpu_pct,
        );

        // 4. Update the 10 knobs dynamically
        let mut current_knobs = self.knobs.snapshot();
        current_knobs.global_concurrency = target_c;
        current_knobs.http_pool_idle_per_host =
            (current_knobs.per_host_concurrency * 2).clamp(4, 128);
        current_knobs.queue_capacity = target_c * 16;

        // Dynamic RTT-aware timeout adjustment. Never regress below the 3.5s
        // global-network safety floor because a local benchmark has tiny latency.
        let dynamic_conn_timeout = ((p95 * 3.0) as u64).clamp(3500, 15000);
        current_knobs.connect_timeout_ms = dynamic_conn_timeout;

        self.knobs.update(&current_knobs);
        current_knobs
    }

    /// Generates a comprehensive performance diagnosis of the host machine.
    pub fn diagnose(&self) -> PerformanceDiagnosis {
        let mem = HardwareTopology::read_memory();
        let total_mb = mem.total_bytes as f64 / (1024.0 * 1024.0);
        let rss_mb = mem.process_rss_bytes as f64 / (1024.0 * 1024.0);
        let cpu = self.cpu_tracker.sample();
        let cpu_pct = cpu.utilization_pct;
        let (p50, p95, p99) = self.latencies.quantiles();
        let curr_c = self.knobs.global_concurrency();

        let (bottleneck, recommendation) = if !cpu.valid {
            (
                "PROCESS_CPU_WARMING_UP",
                "A second process CPU sample is required before diagnosing saturation.",
            )
        } else if cpu_pct > 90.0 {
            (
                "CPU_SATURATED",
                "CPU utilization exceeds 90%. System is CPU-bound on parsing/regex.",
            )
        } else if cpu_pct < 45.0 && curr_c < self.topo.max_concurrency {
            (
                "CONCURRENCY_STARVED",
                "CPU utilization is under 45%. System concurrency ceiling should be unlocked.",
            )
        } else if rss_mb > total_mb * 0.80 {
            (
                "MEMORY_PRESSURE",
                "Memory usage exceeds 80% threshold. Backpressure asserted.",
            )
        } else if p95 > 25.0 {
            (
                "NETWORK_LATENCY_BOTTLENECK",
                "High tail latency detected. Check network RTT or proxy.",
            )
        } else {
            (
                "BALANCED_OPTIMAL",
                "Hardware resources are well-balanced and operating along the Pareto frontier.",
            )
        };

        PerformanceDiagnosis {
            logical_cores: self.topo.logical_cores,
            physical_cores: self.topo.physical_cores,
            worker_cores: self.topo.worker_cores,
            runtime_workers: self.topo.runtime_workers,
            sockets: self.topo.sockets,
            numa_nodes: self.topo.numa_nodes,
            threads_per_core: self.topo.threads_per_core,
            total_memory_mb: total_mb,
            process_rss_mb: rss_mb,
            cpu_saturation_pct: cpu_pct,
            process_cpu_time_s: cpu.process_cpu_time_s,
            p50_latency_ms: p50,
            p95_latency_ms: p95,
            p99_latency_ms: p99,
            active_concurrency: curr_c,
            active_sites: self.knobs.active_sites_cap(),
            fd_limit: self.topo.fd_limit,
            primary_bottleneck: bottleneck.to_string(),
            recommendation: recommendation.to_string(),
        }
    }
}

impl Default for AutonomousTuningEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_controller() -> AimdController {
        AimdController::new(
            16,
            32,
            ControllerConfig {
                min_concurrency: 4,
                max_concurrency: 64,
                additive_step: 8,
                settle_ticks: 2,
                good_samples_before_probe: 2,
                bad_samples_before_backoff: 2,
                steady_probe_interval: 4,
                ..ControllerConfig::default()
            },
        )
    }

    #[test]
    fn requires_a_stable_window_before_probing() {
        let controller = test_controller();
        assert_eq!(
            controller
                .evaluate_and_adjust_with_cpu(10, 100, 100.0, 1.0, 0.0, 50.0)
                .0,
            16
        );
        assert_eq!(
            controller
                .evaluate_and_adjust_with_cpu(10, 100, 101.0, 1.0, 0.0, 50.0)
                .0,
            24
        );
        assert_eq!(
            controller
                .evaluate_and_adjust_with_cpu(10, 100, 102.0, 1.0, 0.0, 50.0)
                .0,
            24
        );
        assert_eq!(
            controller
                .evaluate_and_adjust_with_cpu(10, 100, 103.0, 1.0, 0.0, 50.0)
                .0,
            24
        );
    }

    #[test]
    fn backs_off_immediately_on_errors_or_critical_memory() {
        let controller = test_controller();
        let (after_error, state) =
            controller.evaluate_and_adjust_with_cpu(10, 100, 100.0, 1.0, 2.0, 50.0);
        assert_eq!(after_error, 12);
        assert_eq!(state, ControllerState::Draining);
        controller.evaluate_and_adjust_with_cpu(10, 100, 100.0, 1.0, 0.0, 50.0);
        controller.evaluate_and_adjust_with_cpu(10, 100, 100.0, 1.0, 0.0, 50.0);
        let (held, state) = controller.evaluate_and_adjust_with_cpu(10, 100, 100.0, 1.0, 0.0, 50.0);
        assert_eq!(held, 12);
        assert_eq!(state, ControllerState::SteadyLock);

        let controller = test_controller();
        let (after_memory, state) =
            controller.evaluate_and_adjust_with_cpu(95, 100, 100.0, 1.0, 0.0, 50.0);
        assert_eq!(after_memory, 8);
        assert_eq!(state, ControllerState::Draining);
    }

    #[test]
    fn process_cpu_ceiling_prevents_blind_upward_probes() {
        let controller = test_controller();
        controller.evaluate_and_adjust_with_cpu(10, 100, 100.0, 1.0, 0.0, 95.0);
        let (concurrency, state) =
            controller.evaluate_and_adjust_with_cpu(10, 100, 101.0, 1.0, 0.0, 95.0);
        assert_eq!(concurrency, 16);
        assert_eq!(state, ControllerState::SteadyLock);
    }

    #[test]
    fn converges_near_a_synthetic_throughput_peak() {
        let controller = test_controller();
        for _ in 0..80 {
            let concurrency = controller.concurrency();
            let qps = match concurrency {
                0..=16 => 100.0,
                17..=24 => 140.0,
                25..=32 => 160.0,
                33..=40 => 148.0,
                _ => 130.0,
            };
            controller.evaluate_and_adjust_with_cpu(10, 100, qps, 1.0, 0.0, 70.0);
        }
        assert_eq!(controller.best_concurrency(), 32);
        assert!((32..=40).contains(&controller.concurrency()));
    }
}
