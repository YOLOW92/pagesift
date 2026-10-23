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
            additive_step: (topo.logical_cores / 2).clamp(2, 64),
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

    /// Gradient ceiling detection evaluation using adaptive latency limits.
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

        if mem_ratio >= self.config.memory_warning_threshold || error_rate_pct > 1.0 {
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
                if instant_qps > inner.best_qps * 1.01 {
                    inner.best_qps = instant_qps;
                    inner.best_concurrency = curr;
                }

                // Adaptive latency ceiling: scales with concurrency
                let latency_threshold = (2.0 + (curr as f64 / 256.0)).clamp(3.0, 15.0);
                let qps_stalled_or_dropped =
                    delta_c > 0 && delta_qps < -200.0 && curr > self.config.min_concurrency;
                let latency_spiked = dispatch_latency_ms > latency_threshold;

                if (qps_stalled_or_dropped || latency_spiked) && inner.best_concurrency > 0 {
                    inner.mode = ControllerState::CeilingDetected;
                    new_c = inner.best_concurrency;
                    inner.mode = ControllerState::SteadyLock;
                    inner.steady_ticks = 0;
                } else if curr < self.config.max_concurrency && dispatch_latency_ms < latency_threshold * 0.75 {
                    new_c = (curr + self.config.additive_step).min(self.config.max_concurrency);
                } else {
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
                if inner.steady_ticks >= 20 {
                    inner.mode = ControllerState::Probing;
                    inner.steady_ticks = 0;
                }
            }
        }

        inner.prev_concurrency = curr;
        inner.prev_qps = instant_qps;

        self.store_concurrency(new_c);
        (new_c, inner.mode)
    }

    #[inline]
    fn store_concurrency(&self, c: usize) {
        self.current_concurrency.store(c, Ordering::Relaxed);
    }
}

/// Comprehensive performance bottleneck diagnosis output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceDiagnosis {
    pub logical_cores: usize,
    pub total_memory_mb: f64,
    pub process_rss_mb: f64,
    pub cpu_saturation_pct: f64,
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
            worker_pool_size: topo.logical_cores,
            active_sites_cap: topo.active_sites_cap,
            per_host_concurrency: (topo.logical_cores / 4).clamp(2, 16),
            inflight_memory_budget_bytes: topo.inflight_payload_budget_bytes,
            queue_capacity: topo.base_concurrency * 16,
            http_pool_idle_per_host: topo.base_concurrency * 2,
            http_pool_idle_timeout_secs: 90,
            batch_size: (topo.logical_cores * 16).clamp(32, 512),
            connect_timeout_ms: 3500,
            request_timeout_ms: 10000,
            rendering_pool_concurrency: (topo.logical_cores / 8).clamp(1, 16),
        };

        let optimizer = ResponseSurfaceOptimizer::new(
            topo.min_concurrency,
            topo.max_concurrency,
            total_mem_mb,
            topo.fd_limit,
        );

        Self {
            knobs: Arc::new(AtomicTuningKnobs::new(initial_knobs)),
            latencies: Arc::new(QuantileLatencyTracker::new(256)),
            cpu_tracker: Arc::new(CpuSaturationTracker::new()),
            rss_tracker: Arc::new(RssDerivativeTracker::new()),
            optimizer: Mutex::new(optimizer),
            topo,
            start_time: Instant::now(),
        }
    }

    /// Evaluates current metrics and adjusts the 10-dimensional execution knobs.
    pub fn tick(&self, instant_qps: f64, error_rate_pct: f64, current_fds: usize) -> TuningKnobs {
        let (rss_mb, _d_rss) = self.rss_tracker.sample();
        let cpu_pct = self.cpu_tracker.sample_utilization();
        let (_p50, p95, _p99) = self.latencies.quantiles();

        let mut opt = self.optimizer.lock().unwrap();
        let curr_c = self.knobs.global_concurrency();

        // 1. Check safety status
        let safety = opt.evaluate_safety(rss_mb, error_rate_pct, p95, current_fds);

        // 2. Record observation point
        opt.record_observation(SurfacePoint {
            concurrency: curr_c,
            qps: instant_qps,
            p95_latency_ms: p95,
            cpu_pct,
            rss_mb,
            timestamp_ms: self.start_time.elapsed().as_millis() as u64,
        });

        // 3. Compute target concurrency via CCD & Response Surface fit
        let target_c = opt.next_probe_target(curr_c, safety);

        // 4. Update the 10 knobs dynamically
        let mut current_knobs = self.knobs.snapshot();
        current_knobs.global_concurrency = target_c;
        current_knobs.http_pool_idle_per_host = target_c * 2;
        current_knobs.queue_capacity = target_c * 16;

        // Dynamic RTT-aware timeout adjustment (3x p95 latency, bounded between 1.5s and 10s)
        let dynamic_conn_timeout = ((p95 * 3.0) as u64).clamp(1500, 10000);
        current_knobs.connect_timeout_ms = dynamic_conn_timeout;

        self.knobs.update(&current_knobs);
        current_knobs
    }

    /// Generates a comprehensive performance diagnosis of the host machine.
    pub fn diagnose(&self) -> PerformanceDiagnosis {
        let mem = HardwareTopology::read_memory();
        let total_mb = mem.total_bytes as f64 / (1024.0 * 1024.0);
        let rss_mb = mem.process_rss_bytes as f64 / (1024.0 * 1024.0);
        let cpu_pct = self.cpu_tracker.sample_utilization();
        let (p50, p95, p99) = self.latencies.quantiles();
        let curr_c = self.knobs.global_concurrency();

        let (bottleneck, recommendation) = if cpu_pct > 90.0 {
            ("CPU_SATURATED", "CPU utilization exceeds 90%. System is CPU-bound on parsing/regex.")
        } else if cpu_pct < 45.0 && curr_c < self.topo.max_concurrency {
            ("CONCURRENCY_STARVED", "CPU utilization is under 45%. System concurrency ceiling should be unlocked.")
        } else if rss_mb > total_mb * 0.80 {
            ("MEMORY_PRESSURE", "Memory usage exceeds 80% threshold. Backpressure asserted.")
        } else if p95 > 25.0 {
            ("NETWORK_LATENCY_BOTTLENECK", "High tail latency detected. Check network RTT or proxy.")
        } else {
            ("BALANCED_OPTIMAL", "Hardware resources are well-balanced and operating along the Pareto frontier.")
        };

        PerformanceDiagnosis {
            logical_cores: self.topo.logical_cores,
            total_memory_mb: total_mb,
            process_rss_mb: rss_mb,
            cpu_saturation_pct: cpu_pct,
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
