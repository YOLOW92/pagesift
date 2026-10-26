//! P2 Step 4.2: controlled per-origin adaptive connection benchmark.

#![forbid(unsafe_code)]

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use clap::Parser;
use futures_util::{StreamExt, stream};
use pagesift_net::{FetchRequest, Fetcher, FetcherConfig, HostConnectionStats, SsrfPolicy};
use serde::Serialize;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Parser)]
struct Opts {
    #[arg(long, default_value_t = 10_000)]
    requests_per_host: u64,
    #[arg(long, default_value_t = 512)]
    dispatch_concurrency: usize,
    #[arg(long, default_value_t = 32)]
    max_connections_per_host: usize,
    #[arg(long, default_value_t = 5)]
    slow_service_ms: u64,
    #[arg(long, default_value_t = 4)]
    slow_capacity: usize,
    #[arg(long, default_value = "p2_host_lifecycle.json")]
    output: std::path::PathBuf,
}

struct OriginState {
    delay: Duration,
    capacity: usize,
    active: AtomicUsize,
    peak_active: AtomicUsize,
    completed: AtomicU64,
    overloaded: AtomicU64,
}

struct ActiveRequest(Arc<OriginState>);

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Debug, Serialize)]
struct HostReport {
    origin: String,
    hot_target: usize,
    max_hot: usize,
    completed: u64,
    failed: u64,
    overloaded: u64,
    cancelled: u64,
    reused: u64,
    connected: u64,
    service_ewma_ms: f64,
    connect_ewma_ms: f64,
}

#[derive(Debug, Serialize)]
struct RunReport {
    mode: &'static str,
    wall_time_s: f64,
    qps: f64,
    transport_failed: u64,
    overload_responses: u64,
    fast_peak_active: usize,
    slow_peak_active: usize,
    hosts: Vec<HostReport>,
}

#[derive(Debug, Serialize)]
struct FinalReport {
    schema: u32,
    requests_per_host: u64,
    dispatch_concurrency: usize,
    max_connections_per_host: usize,
    slow_capacity: usize,
    slow_service_ms: u64,
    fixed: RunReport,
    adaptive: RunReport,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = Opts::parse();
    let fast_state = Arc::new(OriginState {
        delay: Duration::ZERO,
        capacity: usize::MAX,
        active: AtomicUsize::new(0),
        peak_active: AtomicUsize::new(0),
        completed: AtomicU64::new(0),
        overloaded: AtomicU64::new(0),
    });
    let slow_state = Arc::new(OriginState {
        delay: Duration::from_millis(opts.slow_service_ms),
        capacity: opts.slow_capacity.max(1),
        active: AtomicUsize::new(0),
        peak_active: AtomicUsize::new(0),
        completed: AtomicU64::new(0),
        overloaded: AtomicU64::new(0),
    });
    let fast_addr = serve(fast_state.clone()).await?;
    let slow_addr = serve(slow_state.clone()).await?;

    let fixed = run_case(false, &opts, fast_addr, slow_addr, &fast_state, &slow_state).await;
    let adaptive = run_case(true, &opts, fast_addr, slow_addr, &fast_state, &slow_state).await;
    let report = FinalReport {
        schema: 1,
        requests_per_host: opts.requests_per_host,
        dispatch_concurrency: opts.dispatch_concurrency,
        max_connections_per_host: opts.max_connections_per_host,
        slow_capacity: opts.slow_capacity,
        slow_service_ms: opts.slow_service_ms,
        fixed,
        adaptive,
    };
    std::fs::write(&opts.output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

async fn serve(state: Arc<OriginState>) -> std::io::Result<SocketAddr> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let app = Router::new()
        .route("/p/{id}", get(handle))
        .with_state(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(addr)
}

async fn handle(
    Path(id): Path<u64>,
    State(state): State<Arc<OriginState>>,
) -> (StatusCode, String) {
    let active = state.active.fetch_add(1, Ordering::Relaxed) + 1;
    state.peak_active.fetch_max(active, Ordering::Relaxed);
    let _active = ActiveRequest(state.clone());
    if !state.delay.is_zero() {
        tokio::time::sleep(state.delay).await;
    }
    state.completed.fetch_add(1, Ordering::Relaxed);
    if active > state.capacity {
        state.overloaded.fetch_add(1, Ordering::Relaxed);
        return (StatusCode::SERVICE_UNAVAILABLE, "busy".into());
    }
    (StatusCode::OK, format!("<main><p>page {id}</p></main>"))
}

async fn run_case(
    adaptive: bool,
    opts: &Opts,
    fast_addr: SocketAddr,
    slow_addr: SocketAddr,
    fast_state: &OriginState,
    slow_state: &OriginState,
) -> RunReport {
    fast_state.peak_active.store(0, Ordering::Relaxed);
    slow_state.peak_active.store(0, Ordering::Relaxed);
    let before_overload = slow_state.overloaded.load(Ordering::Relaxed);
    let fetcher = Arc::new(
        Fetcher::new(FetcherConfig {
            ssrf: SsrfPolicy {
                allow_ips: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
                ..Default::default()
            },
            adaptive_host_connections: adaptive,
            max_connections_per_host: opts.max_connections_per_host,
            pool_max_idle_per_host: opts.max_connections_per_host,
            ..Default::default()
        })
        .expect("valid benchmark fetcher"),
    );
    let transport_failed = Arc::new(AtomicU64::new(0));
    let overload_responses = Arc::new(AtomicU64::new(0));
    let total = opts.requests_per_host.saturating_mul(2);
    let started = Instant::now();
    stream::iter(0..total)
        .for_each_concurrent(opts.dispatch_concurrency.max(1), |task| {
            let fetcher = fetcher.clone();
            let transport_failed = transport_failed.clone();
            let overload_responses = overload_responses.clone();
            async move {
                let address = if task % 2 == 0 { fast_addr } else { slow_addr };
                let url = format!("http://{address}/p/{}", task / 2);
                match fetcher.fetch(FetchRequest::new(url)).await {
                    Ok(response) if response.status == 503 => {
                        overload_responses.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(_) => {}
                    Err(_) => {
                        transport_failed.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        })
        .await;
    let elapsed = started.elapsed().as_secs_f64();
    let observed_overload = slow_state
        .overloaded
        .load(Ordering::Relaxed)
        .saturating_sub(before_overload);
    RunReport {
        mode: if adaptive { "adaptive" } else { "fixed" },
        wall_time_s: elapsed,
        qps: total as f64 / elapsed.max(1e-9),
        transport_failed: transport_failed.load(Ordering::Relaxed),
        overload_responses: overload_responses
            .load(Ordering::Relaxed)
            .max(observed_overload),
        fast_peak_active: fast_state.peak_active.load(Ordering::Relaxed),
        slow_peak_active: slow_state.peak_active.load(Ordering::Relaxed),
        hosts: fetcher
            .host_connection_stats()
            .into_iter()
            .map(host_report)
            .collect(),
    }
}

fn host_report(stats: HostConnectionStats) -> HostReport {
    HostReport {
        origin: stats.origin,
        hot_target: stats.hot_target,
        max_hot: stats.max_hot,
        completed: stats.completed,
        failed: stats.failed,
        overloaded: stats.overloaded,
        cancelled: stats.cancelled,
        reused: stats.reused,
        connected: stats.connected,
        service_ewma_ms: stats.service_ewma.as_secs_f64() * 1_000.0,
        connect_ewma_ms: stats.connect_ewma.as_secs_f64() * 1_000.0,
    }
}
