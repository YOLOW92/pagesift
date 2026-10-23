//! Autonomous response surface modeling and constrained coordinate descent optimizer.
//!
//! Replaces hardcoded step-sizes and arbitrary latency thresholds with dynamic
//! quadratic surface fitting and Pareto-frontier tracking.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// A single observation sample on the performance surface.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SurfacePoint {
    pub concurrency: usize,
    pub qps: f64,
    pub p95_latency_ms: f64,
    pub cpu_pct: f64,
    pub rss_mb: f64,
    pub timestamp_ms: u64,
}

/// Safety boundary evaluation result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyStatus {
    /// Safe operating zone.
    Optimal,
    /// Memory approaching budget threshold (>85%).
    MemoryThrottled,
    /// Error rate or connection timeouts exceeding 1%.
    ErrorRateSpiked,
    /// Dispatch / scheduling latency degraded (>3x baseline).
    LatencySaturated,
    /// File descriptor usage exceeding 80% of ulimit.
    FdExhausted,
}

/// Dynamic response surface tracker and optimizer.
#[derive(Debug, Clone)]
pub struct ResponseSurfaceOptimizer {
    history: VecDeque<SurfacePoint>,
    max_history: usize,
    baseline_latency_ms: f64,
    best_point: Option<SurfacePoint>,
    min_concurrency: usize,
    max_concurrency: usize,
    total_memory_mb: f64,
    fd_limit: usize,
}

impl ResponseSurfaceOptimizer {
    /// Creates a new surface optimizer with dynamic bounds.
    pub fn new(min_c: usize, max_c: usize, total_mem_mb: f64, fd_limit: usize) -> Self {
        Self {
            history: VecDeque::with_capacity(64),
            max_history: 32,
            baseline_latency_ms: 2.0,
            best_point: None,
            min_concurrency: min_c.max(2),
            max_concurrency: max_c.max(min_c),
            total_memory_mb: total_mem_mb.max(512.0),
            fd_limit: fd_limit.max(1024),
        }
    }

    /// Evaluates system health against the multi-dimensional safety boundaries.
    pub fn evaluate_safety(
        &self,
        rss_mb: f64,
        error_rate_pct: f64,
        p95_latency_ms: f64,
        current_fds: usize,
    ) -> SafetyStatus {
        // Guard 1: Memory boundary (RSS <= 85% total RAM)
        if rss_mb > self.total_memory_mb * 0.85 {
            return SafetyStatus::MemoryThrottled;
        }

        // Guard 2: Error rate boundary (<= 1.0%)
        if error_rate_pct > 1.0 {
            return SafetyStatus::ErrorRateSpiked;
        }

        // Guard 3: Latency saturation guard (p95 > 3.5x baseline)
        if p95_latency_ms > self.baseline_latency_ms * 3.5 && p95_latency_ms > 8.0 {
            return SafetyStatus::LatencySaturated;
        }

        // Guard 4: File descriptor boundary (<= 80% ulimit)
        if current_fds > (self.fd_limit * 8) / 10 {
            return SafetyStatus::FdExhausted;
        }

        SafetyStatus::Optimal
    }

    /// Records a new observation point and updates the Pareto frontier.
    pub fn record_observation(&mut self, point: SurfacePoint) {
        if self.baseline_latency_ms <= 0.0 || self.history.is_empty() {
            self.baseline_latency_ms = point.p95_latency_ms.max(0.5);
        } else {
            // Smoothly update moving baseline latency
            self.baseline_latency_ms = self.baseline_latency_ms * 0.90 + point.p95_latency_ms * 0.10;
        }

        if let Some(best) = self.best_point {
            if point.qps > best.qps * 1.02 {
                self.best_point = Some(point);
            }
        } else {
            self.best_point = Some(point);
        }

        if self.history.len() >= self.max_history {
            self.history.pop_front();
        }
        self.history.push_back(point);
    }

    /// Fits a local quadratic response surface: Q(c) = a*c^2 + b*c + c_0.
    /// Returns the estimated optimal concurrency c* = -b / (2a) when curvature a < 0.
    pub fn fit_local_surface(&self) -> Option<usize> {
        if self.history.len() < 5 {
            return None;
        }

        let n = self.history.len() as f64;
        let mut sum_x = 0.0;
        let mut sum_x2 = 0.0;
        let mut sum_x3 = 0.0;
        let mut sum_x4 = 0.0;
        let mut sum_y = 0.0;
        let mut sum_xy = 0.0;
        let mut sum_x2y = 0.0;

        for p in &self.history {
            let x = p.concurrency as f64;
            let y = p.qps;
            let x2 = x * x;
            let x3 = x2 * x;
            let x4 = x3 * x;

            sum_x += x;
            sum_x2 += x2;
            sum_x3 += x3;
            sum_x4 += x4;
            sum_y += y;
            sum_xy += x * y;
            sum_x2y += x2 * y;
        }

        // Solve 3x3 linear system via Cramer's rule for quadratic regression
        // [ sum_x4 sum_x3 sum_x2 ] [ a ]   [ sum_x2y ]
        // [ sum_x3 sum_x2 sum_x1 ] [ b ] = [ sum_xy  ]
        // [ sum_x2 sum_x1   n    ] [ c ]   [ sum_y   ]
        let m00 = sum_x4; let m01 = sum_x3; let m02 = sum_x2;
        let m10 = sum_x3; let m11 = sum_x2; let m12 = sum_x;
        let m20 = sum_x2; let m21 = sum_x;  let m22 = n;

        let det = m00 * (m11 * m22 - m12 * m21)
                - m01 * (m10 * m22 - m12 * m20)
                + m02 * (m10 * m21 - m11 * m20);

        if det.abs() < 1e-9 {
            return None;
        }

        // Determinant for 'a'
        let det_a = sum_x2y * (m11 * m22 - m12 * m21)
                  - m01 * (sum_xy * m22 - m12 * sum_y)
                  + m02 * (sum_xy * m21 - m11 * sum_y);

        // Determinant for 'b'
        let det_b = m00 * (sum_xy * m22 - m12 * sum_y)
                  - sum_x2y * (m10 * m22 - m12 * m20)
                  + m02 * (m10 * sum_y - sum_xy * m20);

        let a = det_a / det;
        let b = det_b / det;

        // If a < 0, surface is concave down with vertex at -b / (2a)
        if a < -1e-6 {
            let opt_c = -b / (2.0 * a);
            if opt_c > 0.0 && opt_c.is_finite() {
                let clamped = (opt_c.round() as usize).clamp(self.min_concurrency, self.max_concurrency);
                return Some(clamped);
            }
        }

        None
    }

    /// Computes the next suggested probe action via Constrained Coordinate Descent (CCD).
    pub fn next_probe_target(&self, current_concurrency: usize, safety: SafetyStatus) -> usize {
        match safety {
            SafetyStatus::MemoryThrottled => {
                // Harsh backoff on memory emergency (retreat to safe 60% level)
                let target = ((current_concurrency as f64) * 0.60).round() as usize;
                target.clamp(self.min_concurrency, self.max_concurrency)
            }
            SafetyStatus::ErrorRateSpiked | SafetyStatus::FdExhausted => {
                // Moderate retreat on error or FD exhaustion
                let target = ((current_concurrency as f64) * 0.80).round() as usize;
                target.clamp(self.min_concurrency, self.max_concurrency)
            }
            SafetyStatus::LatencySaturated => {
                // Revert to best recorded sweet spot
                if let Some(best) = self.best_point {
                    best.concurrency.clamp(self.min_concurrency, self.max_concurrency)
                } else {
                    ((current_concurrency as f64) * 0.85).round() as usize
                }
            }
            SafetyStatus::Optimal => {
                // If local quadratic surface fit is confident, lean towards the fitted peak
                if let Some(opt_c) = self.fit_local_surface() {
                    // Damped approach towards optimal vertex
                    let step = ((opt_c as isize - current_concurrency as isize) / 3).clamp(-32, 64);
                    let target = (current_concurrency as isize + step) as usize;
                    return target.clamp(self.min_concurrency, self.max_concurrency);
                }

                // Otherwise, perform forward gradient exploration
                let step = (current_concurrency / 8).clamp(4, 64);
                (current_concurrency + step).min(self.max_concurrency)
            }
        }
    }

    /// Returns the best recorded sweet spot point so far.
    pub fn best_point(&self) -> Option<SurfacePoint> {
        self.best_point
    }
}
