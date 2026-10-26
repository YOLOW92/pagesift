//! PageSift 0.70 Step 4.5: frozen real-web adaptive release gate.
//! Usage: `bench_real_web_mt <url_file> [mode=adaptive] [reserved=0] [limit=100000] [out_json]`

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use pagesift::adaptive::{HostHardware, TuningMode, TuningProfile};
use pagesift::api::ErrorCode;
use pagesift::api::types::{Format, ScrapeOptions};
use pagesift::{Scraper, ScraperConfig};
use tokio::sync::{Mutex, mpsc};

#[derive(Default)]
struct Metrics {
    processed: AtomicUsize,
    success: AtomicUsize,
    valid_content: AtomicUsize,
    duplicate: AtomicUsize,
    dns_or_connect_err: AtomicUsize,
    robots_disallowed: AtomicUsize,
    access_barrier: AtomicUsize,
    rate_limited: AtomicUsize,
    ssrf_blocked: AtomicUsize,
    too_large: AtomicUsize,
    dns_failure: AtomicUsize,
    network_error: AtomicUsize,
    other_error: AtomicUsize,
    http_4xx: AtomicUsize,
    http_5xx: AtomicUsize,
    timeout: AtomicUsize,
    total_bytes: AtomicUsize,
    structured_data_count: AtomicUsize,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let url_file = args
        .get(1)
        .map(|s| s.as_str())
        .unwrap_or("benchmarks/data/top1m.txt");

    // Parse mode: adaptive (default) vs conservative
    let mode_str = args.get(2).map(|s| s.as_str()).unwrap_or("adaptive");
    let mode = TuningMode::from_str(mode_str).unwrap_or(TuningMode::Adaptive);

    // Hardware detection & bottleneck-constrained adaptive tuning profile generation
    let hw = HostHardware::detect();
    let profile = TuningProfile::new(mode);

    // The release gate must prove autonomous convergence. Manual capacity
    // overrides are intentionally rejected instead of becoming another grid.
    if let Some(concurrency_override) = args.get(3).and_then(|s| s.parse::<usize>().ok())
        && concurrency_override > 0
    {
        return Err("Step 4.5 forbids manual concurrency overrides".into());
    }

    let limit: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(100_000);
    let out_json = args
        .get(5)
        .map(|s| s.as_str())
        .unwrap_or("benchmarks/scale/results/0.70_p2_step4_5_real_https_100k.json");

    eprintln!("================================================================================");
    eprintln!("[PageSift 0.70] Stage-Isolated Adaptive Engine Initialized");
    eprintln!(
        "Host Detected : {} logical cores, {:.1} GB RAM, FD Limit: {} | Tier: {}",
        hw.logical_cores,
        hw.memory_gb(),
        hw.fd_limit,
        hw.tier.name()
    );
    eprintln!(
        "Tuning Profile: Mode [{}] | Concurrency: {} (Bottleneck Safe) | Connect Timeout: {:.1}s | Total Timeout: {:.1}s",
        profile.mode,
        profile.global_concurrency,
        profile.connect_timeout.as_secs_f64(),
        profile.total_timeout.as_secs_f64()
    );
    eprintln!(
        "Target Dataset: {} (Limit: {} URLs) | Channel Buffer: {}",
        url_file, limit, profile.mpsc_buffer_capacity
    );
    eprintln!("================================================================================");

    let mut cfg = ScraperConfig::default();
    profile.apply_to_config(&mut cfg);

    let concurrency = profile.global_concurrency;
    let channel_cap = profile.mpsc_buffer_capacity;

    let scraper = Arc::new(Scraper::new(cfg)?);
    let metrics = Arc::new(Metrics::default());
    let seen_hashes = Arc::new(Mutex::new(HashSet::<[u8; 16]>::new()));
    let opts = Arc::new(ScrapeOptions {
        formats: vec![Format::new("markdown")],
        engine: Some("http".to_string()),
        store_in_cache: false,
        ..Default::default()
    });

    let (tx, rx) = mpsc::channel::<String>(channel_cap);
    let rx = Arc::new(Mutex::new(rx));

    // Producer: streams lines lazily to preserve strict O(1) memory
    let url_file_clone = url_file.to_string();
    let producer = tokio::spawn(async move {
        let file = File::open(&url_file_clone)
            .map_err(|error| format!("cannot open frozen dataset {url_file_clone}: {error}"))?;
        let reader = BufReader::new(file);
        let mut count = 0;
        let mut targets = HashSet::with_capacity(limit);
        let mut dataset_hash = blake3::Hasher::new();
        for line in reader.lines() {
            if count >= limit {
                break;
            }
            let line = line.map_err(|error| format!("cannot read frozen dataset: {error}"))?;
            if let Some(target) = normalize_target(&line) {
                if !targets.insert(target.clone()) {
                    continue;
                }
                dataset_hash.update(target.as_bytes());
                dataset_hash.update(b"\n");
                count += 1;
                if tx.send(target).await.is_err() {
                    return Err("benchmark workers stopped before dataset was submitted".into());
                }
            }
        }
        Ok::<_, String>((count, dataset_hash.finalize().to_hex().to_string()))
    });

    let cpu_start = read_process_cpu_ticks().unwrap_or(0);
    let t0 = Instant::now();
    let mut workers = Vec::with_capacity(concurrency);
    let milestone_50k_done = Arc::new(AtomicBool::new(false));

    // Live Reporter & Checkpoint task
    let metrics_rep = Arc::clone(&metrics);
    let scraper_rep = Arc::clone(&scraper);
    let milestone_flag = Arc::clone(&milestone_50k_done);
    let out_json_rep = out_json.to_string();
    let reporter = tokio::spawn(async move {
        let mut last_processed = 0;
        let mut last_t = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            let current = metrics_rep.processed.load(Ordering::Relaxed);
            let delta = current.saturating_sub(last_processed);
            let dt = last_t.elapsed().as_secs_f64();
            let inst_qps = delta as f64 / dt.max(0.001);
            let total_el = t0.elapsed().as_secs_f64();
            let overall_qps = current as f64 / total_el.max(0.001);
            let succ = metrics_rep.success.load(Ordering::Relaxed);
            let valid = metrics_rep.valid_content.load(Ordering::Relaxed);
            let dup = metrics_rep.duplicate.load(Ordering::Relaxed);
            let fail = metrics_rep.dns_or_connect_err.load(Ordering::Relaxed)
                + metrics_rep.http_4xx.load(Ordering::Relaxed)
                + metrics_rep.http_5xx.load(Ordering::Relaxed)
                + metrics_rep.timeout.load(Ordering::Relaxed)
                + metrics_rep.robots_disallowed.load(Ordering::Relaxed)
                + metrics_rep.access_barrier.load(Ordering::Relaxed)
                + metrics_rep.rate_limited.load(Ordering::Relaxed)
                + metrics_rep.ssrf_blocked.load(Ordering::Relaxed)
                + metrics_rep.too_large.load(Ordering::Relaxed)
                + metrics_rep.dns_failure.load(Ordering::Relaxed)
                + metrics_rep.network_error.load(Ordering::Relaxed)
                + metrics_rep.other_error.load(Ordering::Relaxed);

            eprintln!(
                "[PageSift Monitor] Progress: {}/{} ({:.1}%) | Inst QPS: {:.1} | Sustained QPS: {:.1} | Succ: {} (Valid: {}, Dup: {}) | Fail: {} | Elapsed: {:.1}s",
                current,
                limit,
                (current as f64 / limit as f64) * 100.0,
                inst_qps,
                overall_qps,
                succ,
                valid,
                dup,
                fail,
                total_el
            );

            let stage_state = scraper_rep
                .network_stage_snapshots()
                .into_iter()
                .map(|stage| format!("{:?}={}/{}", stage.stage, stage.active, stage.limit))
                .collect::<Vec<_>>()
                .join(" ");
            eprintln!("[PageSift Stages] {stage_state}");

            // Periodic WAL Checkpoint dump
            let checkpoint = serde_json::json!({
                "timestamp_epoch_s": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                "processed": current,
                "target_limit": limit,
                "elapsed_s": total_el,
                "sustained_qps": overall_qps,
                "success": succ,
                "valid_content": valid,
                "duplicate": dup,
                "fail": fail,
                "adaptive_stages": scraper_rep.network_stage_snapshots().into_iter().map(|stage| serde_json::json!({
                    "stage": format!("{:?}", stage.stage).to_ascii_lowercase(),
                    "active": stage.active,
                    "limit": stage.limit,
                })).collect::<Vec<_>>(),
            });
            let cp_path = format!("{}.checkpoint.wal", out_json_rep);
            if let Ok(mut f) = File::create(&cp_path) {
                let _ = f.write_all(checkpoint.to_string().as_bytes());
            }

            // Milestone 50K Checkpoint
            if current >= 50_000 && !milestone_flag.swap(true, Ordering::SeqCst) {
                let ms_path = format!("{}.milestone_50k.json", out_json_rep);
                if let Ok(mut f) = File::create(&ms_path) {
                    let _ = f.write_all(checkpoint.to_string().as_bytes());
                }
                eprintln!(
                    "================================================================================"
                );
                eprintln!(
                    "🏆 [MILESTONE 50K REACHED] Processed: {} URLs in {:.2}s ({:.1} Sustained QPS) | Succ: {} (Valid: {}, {:.1}%) | Dup: {} | Fail: {}",
                    current,
                    total_el,
                    overall_qps,
                    succ,
                    valid,
                    (valid as f64 / succ.max(1) as f64) * 100.0,
                    dup,
                    fail
                );
                eprintln!(
                    "================================================================================"
                );
            }

            last_processed = current;
            last_t = Instant::now();
            if current >= limit {
                break;
            }
        }
    });

    for _ in 0..concurrency {
        let scraper = Arc::clone(&scraper);
        let metrics = Arc::clone(&metrics);
        let seen = Arc::clone(&seen_hashes);
        let opts = Arc::clone(&opts);
        let rx = Arc::clone(&rx);

        workers.push(tokio::spawn(async move {
            loop {
                let url = {
                    let mut lock = rx.lock().await;
                    lock.recv().await
                };
                let Some(url) = url else {
                    break;
                };

                match scraper.scrape(&url, &opts).await {
                    Ok(doc) => {
                        metrics.success.fetch_add(1, Ordering::Relaxed);
                        if let Some(md) = doc.markdown {
                            let len = md.len();
                            metrics.total_bytes.fetch_add(len, Ordering::Relaxed);
                            if len > 100 {
                                metrics.valid_content.fetch_add(1, Ordering::Relaxed);
                                let mut hash_bytes = [0u8; 16];
                                let b3 = blake3::hash(md.as_bytes());
                                hash_bytes.copy_from_slice(&b3.as_bytes()[..16]);
                                let mut guard = seen.lock().await;
                                if !guard.insert(hash_bytes) {
                                    metrics.duplicate.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                        if doc.metadata.description.is_some() || doc.json.is_some() {
                            metrics
                                .structured_data_count
                                .fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(err) => {
                        match err.code {
                            ErrorCode::RobotsDisallowed => {
                                metrics.robots_disallowed.fetch_add(1, Ordering::Relaxed)
                            }
                            ErrorCode::AccessBarrier => {
                                metrics.access_barrier.fetch_add(1, Ordering::Relaxed)
                            }
                            ErrorCode::RateLimited
                            | ErrorCode::HostCooldown
                            | ErrorCode::RateLimitExceeded
                            | ErrorCode::ConcurrencyLimitExceeded => {
                                metrics.rate_limited.fetch_add(1, Ordering::Relaxed)
                            }
                            ErrorCode::Timeout => metrics.timeout.fetch_add(1, Ordering::Relaxed),
                            ErrorCode::SsrfBlocked => {
                                metrics.ssrf_blocked.fetch_add(1, Ordering::Relaxed)
                            }
                            ErrorCode::TooLarge => {
                                metrics.too_large.fetch_add(1, Ordering::Relaxed)
                            }
                            ErrorCode::DnsFailure => {
                                metrics.dns_failure.fetch_add(1, Ordering::Relaxed)
                            }
                            ErrorCode::NetworkError => {
                                metrics.network_error.fetch_add(1, Ordering::Relaxed)
                            }
                            ErrorCode::UpstreamError => {
                                let message = err.message.to_lowercase();
                                if message.contains("4xx")
                                    || message.contains(" 4")
                                    || message.contains("status 4")
                                {
                                    metrics.http_4xx.fetch_add(1, Ordering::Relaxed)
                                } else {
                                    metrics.http_5xx.fetch_add(1, Ordering::Relaxed)
                                }
                            }
                            _ => metrics.other_error.fetch_add(1, Ordering::Relaxed),
                        };
                    }
                }
                metrics.processed.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    let (frozen_count, frozen_blake3) = producer.await.map_err(|error| error.to_string())??;
    for w in workers {
        let _ = w.await;
    }
    reporter.abort();

    let elapsed_s = t0.elapsed().as_secs_f64();
    let process_cpu_time_s = read_process_cpu_ticks()
        .unwrap_or(cpu_start)
        .saturating_sub(cpu_start) as f64
        / clock_ticks_per_second();
    let process_cpu_cores = process_cpu_time_s / elapsed_s.max(0.001);
    let succ = metrics.success.load(Ordering::Relaxed);
    let valid = metrics.valid_content.load(Ordering::Relaxed);
    let dup = metrics.duplicate.load(Ordering::Relaxed);
    let timeouts = metrics.timeout.load(Ordering::Relaxed);
    let h4xx = metrics.http_4xx.load(Ordering::Relaxed);
    let h5xx = metrics.http_5xx.load(Ordering::Relaxed);
    let dns_conn = metrics.dns_or_connect_err.load(Ordering::Relaxed);
    let robots = metrics.robots_disallowed.load(Ordering::Relaxed);
    let barriers = metrics.access_barrier.load(Ordering::Relaxed);
    let rate_limited = metrics.rate_limited.load(Ordering::Relaxed);
    let ssrf = metrics.ssrf_blocked.load(Ordering::Relaxed);
    let too_large = metrics.too_large.load(Ordering::Relaxed);
    let dns = metrics.dns_failure.load(Ordering::Relaxed);
    let network = metrics.network_error.load(Ordering::Relaxed);
    let other = metrics.other_error.load(Ordering::Relaxed);
    let failed = timeouts
        + h4xx
        + h5xx
        + dns_conn
        + robots
        + barriers
        + rate_limited
        + ssrf
        + too_large
        + dns
        + network
        + other;
    let bytes = metrics.total_bytes.load(Ordering::Relaxed);
    let struct_count = metrics.structured_data_count.load(Ordering::Relaxed);
    let qps = (succ + failed) as f64 / elapsed_s.max(0.001);

    if frozen_count != limit || succ + failed != limit {
        return Err(format!(
            "dataset produced {frozen_count} targets and completed {}, expected exactly {limit}",
            succ + failed,
        )
        .into());
    }

    let stages = scraper
        .network_stage_snapshots()
        .into_iter()
        .map(|snapshot| {
            serde_json::json!({
                "stage": format!("{:?}", snapshot.stage).to_ascii_lowercase(),
                "limit": snapshot.limit,
                "active": snapshot.active,
                "completed": snapshot.completed,
                "transient_failures": snapshot.transient_failures,
                "overloads": snapshot.overloads,
                "deterministic_failures": snapshot.deterministic_failures,
                "policy_rejections": snapshot.policy_rejections,
                "cancelled": snapshot.cancelled,
                "p50_ms": snapshot.p50.as_secs_f64() * 1000.0,
                "p95_ms": snapshot.p95.as_secs_f64() * 1000.0,
                "p99_ms": snapshot.p99.as_secs_f64() * 1000.0,
            })
        })
        .collect::<Vec<_>>();
    let phases = scraper
        .transport_phase_snapshots()
        .into_iter()
        .map(|snapshot| {
            serde_json::json!({
                "phase": format!("{:?}", snapshot.phase).to_ascii_lowercase(),
                "samples": snapshot.samples,
                "transient_failures": snapshot.transient_failures,
                "overloads": snapshot.overloads,
                "deterministic_failures": snapshot.deterministic_failures,
                "policy_rejections": snapshot.policy_rejections,
                "p50_ms": snapshot.p50.as_secs_f64() * 1000.0,
                "p95_ms": snapshot.p95.as_secs_f64() * 1000.0,
                "p99_ms": snapshot.p99.as_secs_f64() * 1000.0,
            })
        })
        .collect::<Vec<_>>();

    eprintln!(
        "[PageSift Complete] Processed {} URLs in {:.2}s ({:.1} sustained QPS) | Succ: {}, Valid: {}, Dup: {}, Fail: {} (Timeouts: {}, 4xx: {}, 5xx: {}, Conn: {}) | Bytes: {} MB",
        succ + failed,
        elapsed_s,
        qps,
        succ,
        valid,
        dup,
        failed,
        timeouts,
        h4xx,
        h5xx,
        dns_conn,
        bytes / (1024 * 1024)
    );

    let summary = serde_json::json!({
        "schema": 2,
        "engine": "PageSift production Scraper (HTTP engine / Rustls)",
        "dataset": {
            "path": url_file,
            "distinct_https_targets": limit,
            "one_page_per_host": true,
            "normalized_blake3": frozen_blake3,
            "frozen": true,
        },
        "safety": {
            "robots_txt": "respect",
            "ssrf_policy": "production default",
            "tls_certificate_verification": true,
            "browser_fallback": false,
            "honest_user_agent": pagesift_net::DEFAULT_USER_AGENT,
        },
        "tuning_profile": {
            "mode": profile.mode.to_string(),
            "hardware_tier": profile.hardware.tier.name(),
            "detected_cores": profile.hardware.logical_cores,
            "detected_ram_gb": profile.hardware.memory_gb(),
            "detected_fd_limit": profile.hardware.fd_limit,
            "global_concurrency": profile.global_concurrency,
            "connect_timeout_s": profile.connect_timeout.as_secs_f64(),
            "total_timeout_s": profile.total_timeout.as_secs_f64(),
        },
        "urls_processed": succ + failed,
        "elapsed_s": elapsed_s,
        "sustained_qps": qps,
        "process_cpu_time_s": process_cpu_time_s,
        "process_cpu_cores": process_cpu_cores,
        "adaptive_stages": stages,
        "transport_phases": phases,
        "metrics": {
            "success_200": succ,
            "valid_content": valid,
            "duplicate_pages": dup,
            "failed_total": failed,
            "fail_details": {
                "timeout": timeouts,
                "robots_disallowed": robots,
                "access_barrier": barriers,
                "rate_limited": rate_limited,
                "ssrf_blocked": ssrf,
                "too_large": too_large,
                "dns_failure": dns,
                "network_error": network,
                "other_error": other,
                "http_4xx": h4xx,
                "http_5xx": h5xx,
                "dns_or_connect_err": dns_conn,
            },
            "total_bytes_extracted": bytes,
            "structured_data_pages": struct_count,
            "valid_content_ratio": valid as f64 / succ.max(1) as f64,
        }
    });

    if let Some(parent) = std::path::Path::new(out_json).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out_f = File::create(out_json)?;
    out_f.write_all(serde_json::to_string_pretty(&summary)?.as_bytes())?;
    eprintln!("[PageSift] Final metrics written to {}", out_json);

    scraper.shutdown().await;
    Ok(())
}

/// Accept both plain URL lists and Tranco-style `rank,domain` CSV input.
/// Bare domains are deliberately upgraded to HTTPS so this benchmark exercises
/// DNS, TCP, Rustls certificate verification, redirects, robots, and parsing.
fn normalize_target(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = match trimmed.split_once(',') {
        Some((rank, domain)) if rank.bytes().all(|byte| byte.is_ascii_digit()) => domain.trim(),
        _ => trimmed,
    };
    if candidate.is_empty() {
        None
    } else if candidate.starts_with("http://") || candidate.starts_with("https://") {
        Some(candidate.to_string())
    } else {
        Some(format!("https://{candidate}/"))
    }
}

/// Linux process CPU only: `/proc/self/stat` fields utime + stime.
fn read_process_cpu_ticks() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let close = stat.rfind(')')?;
    let fields = stat
        .get(close + 1..)?
        .split_ascii_whitespace()
        .collect::<Vec<_>>();
    Some(fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?)
}

fn clock_ticks_per_second() -> f64 {
    std::process::Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| *value > 0.0)
        .unwrap_or(100.0)
}

#[cfg(test)]
mod tests {
    use super::normalize_target;

    #[test]
    fn normalizes_tranco_and_url_inputs() {
        assert_eq!(
            normalize_target("1,example.com"),
            Some("https://example.com/".into())
        );
        assert_eq!(
            normalize_target("https://example.org/a"),
            Some("https://example.org/a".into())
        );
        assert_eq!(normalize_target("  "), None);
    }
}
