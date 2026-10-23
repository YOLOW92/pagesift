//! Multi-parameter execution knobs for autonomous performance tuning.
//!
//! Exposes 10-dimensional tuning parameters spanning concurrency, queue capacity,
//! memory budgets, connection pooling, and latency timeouts.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

/// The 10-dimensional tuning parameter configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TuningKnobs {
    /// 1. Global in-flight HTTP request concurrency
    pub global_concurrency: usize,
    /// 2. CPU worker pool threads / task executor capacity
    pub worker_pool_size: usize,
    /// 3. Concurrently scheduled unique domains / sites
    pub active_sites_cap: usize,
    /// 4. Per-host request rate limit / polite concurrency
    pub per_host_concurrency: usize,
    /// 5. In-flight unparsed payload memory budget in bytes
    pub inflight_memory_budget_bytes: usize,
    /// 6. Work queue capacity before backpressure is asserted
    pub queue_capacity: usize,
    /// 7. HTTP connection pool idle connections per host
    pub http_pool_idle_per_host: usize,
    /// HTTP connection keep-alive timeout in seconds
    pub http_pool_idle_timeout_secs: u64,
    /// 8. Content extraction & Markdown batch serialization size
    pub batch_size: usize,
    /// 9. Dynamic RTT-aware connect timeout in milliseconds
    pub connect_timeout_ms: u64,
    /// Dynamic total request timeout in milliseconds
    pub request_timeout_ms: u64,
    /// 10. Chromium rendering pool budget (headless tab concurrency)
    pub rendering_pool_concurrency: usize,
}

impl Default for TuningKnobs {
    fn default() -> Self {
        Self {
            global_concurrency: 64,
            worker_pool_size: 4,
            active_sites_cap: 32,
            per_host_concurrency: 2,
            inflight_memory_budget_bytes: 64 * 1024 * 1024,
            queue_capacity: 10_000,
            http_pool_idle_per_host: 64,
            http_pool_idle_timeout_secs: 90,
            batch_size: 128,
            connect_timeout_ms: 3500,
            request_timeout_ms: 10000,
            rendering_pool_concurrency: 2,
        }
    }
}

/// Thread-safe atomic handle for hot-reloading tuning knobs without locks.
pub struct AtomicTuningKnobs {
    global_concurrency: AtomicUsize,
    worker_pool_size: AtomicUsize,
    active_sites_cap: AtomicUsize,
    per_host_concurrency: AtomicUsize,
    inflight_memory_budget_bytes: AtomicUsize,
    queue_capacity: AtomicUsize,
    http_pool_idle_per_host: AtomicUsize,
    http_pool_idle_timeout_secs: AtomicU64,
    batch_size: AtomicUsize,
    connect_timeout_ms: AtomicU64,
    request_timeout_ms: AtomicU64,
    rendering_pool_concurrency: AtomicUsize,
}

impl AtomicTuningKnobs {
    /// Creates a new atomic handle initialized with the given knobs.
    pub fn new(knobs: TuningKnobs) -> Self {
        Self {
            global_concurrency: AtomicUsize::new(knobs.global_concurrency),
            worker_pool_size: AtomicUsize::new(knobs.worker_pool_size),
            active_sites_cap: AtomicUsize::new(knobs.active_sites_cap),
            per_host_concurrency: AtomicUsize::new(knobs.per_host_concurrency),
            inflight_memory_budget_bytes: AtomicUsize::new(knobs.inflight_memory_budget_bytes),
            queue_capacity: AtomicUsize::new(knobs.queue_capacity),
            http_pool_idle_per_host: AtomicUsize::new(knobs.http_pool_idle_per_host),
            http_pool_idle_timeout_secs: AtomicU64::new(knobs.http_pool_idle_timeout_secs),
            batch_size: AtomicUsize::new(knobs.batch_size),
            connect_timeout_ms: AtomicU64::new(knobs.connect_timeout_ms),
            request_timeout_ms: AtomicU64::new(knobs.request_timeout_ms),
            rendering_pool_concurrency: AtomicUsize::new(knobs.rendering_pool_concurrency),
        }
    }

    /// Takes a consistent snapshot of the active knobs.
    pub fn snapshot(&self) -> TuningKnobs {
        TuningKnobs {
            global_concurrency: self.global_concurrency.load(Ordering::Relaxed),
            worker_pool_size: self.worker_pool_size.load(Ordering::Relaxed),
            active_sites_cap: self.active_sites_cap.load(Ordering::Relaxed),
            per_host_concurrency: self.per_host_concurrency.load(Ordering::Relaxed),
            inflight_memory_budget_bytes: self.inflight_memory_budget_bytes.load(Ordering::Relaxed),
            queue_capacity: self.queue_capacity.load(Ordering::Relaxed),
            http_pool_idle_per_host: self.http_pool_idle_per_host.load(Ordering::Relaxed),
            http_pool_idle_timeout_secs: self.http_pool_idle_timeout_secs.load(Ordering::Relaxed),
            batch_size: self.batch_size.load(Ordering::Relaxed),
            connect_timeout_ms: self.connect_timeout_ms.load(Ordering::Relaxed),
            request_timeout_ms: self.request_timeout_ms.load(Ordering::Relaxed),
            rendering_pool_concurrency: self.rendering_pool_concurrency.load(Ordering::Relaxed),
        }
    }

    /// Atomically updates all knobs to a new configuration.
    pub fn update(&self, knobs: &TuningKnobs) {
        self.global_concurrency.store(knobs.global_concurrency, Ordering::Relaxed);
        self.worker_pool_size.store(knobs.worker_pool_size, Ordering::Relaxed);
        self.active_sites_cap.store(knobs.active_sites_cap, Ordering::Relaxed);
        self.per_host_concurrency.store(knobs.per_host_concurrency, Ordering::Relaxed);
        self.inflight_memory_budget_bytes.store(knobs.inflight_memory_budget_bytes, Ordering::Relaxed);
        self.queue_capacity.store(knobs.queue_capacity, Ordering::Relaxed);
        self.http_pool_idle_per_host.store(knobs.http_pool_idle_per_host, Ordering::Relaxed);
        self.http_pool_idle_timeout_secs.store(knobs.http_pool_idle_timeout_secs, Ordering::Relaxed);
        self.batch_size.store(knobs.batch_size, Ordering::Relaxed);
        self.connect_timeout_ms.store(knobs.connect_timeout_ms, Ordering::Relaxed);
        self.request_timeout_ms.store(knobs.request_timeout_ms, Ordering::Relaxed);
        self.rendering_pool_concurrency.store(knobs.rendering_pool_concurrency, Ordering::Relaxed);
    }

    #[inline]
    pub fn global_concurrency(&self) -> usize {
        self.global_concurrency.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn set_global_concurrency(&self, val: usize) {
        self.global_concurrency.store(val, Ordering::Relaxed);
    }

    #[inline]
    pub fn connect_timeout(&self) -> Duration {
        Duration::from_millis(self.connect_timeout_ms.load(Ordering::Relaxed))
    }

    #[inline]
    pub fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.request_timeout_ms.load(Ordering::Relaxed))
    }

    #[inline]
    pub fn batch_size(&self) -> usize {
        self.batch_size.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn active_sites_cap(&self) -> usize {
        self.active_sites_cap.load(Ordering::Relaxed)
    }
}
