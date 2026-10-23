//! PageSift 0.50: Real-World Web Processing Benchmark with Dynamic Adaptive Tuning & 50K Milestone
//! Usage: `bench_real_web_mt <url_file> [mode=adaptive|conservative] [concurrency_override=0] [limit=1000000] [out_json]`

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use pagesift::adaptive::{HostHardware, TuningMode, TuningProfile};
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
        .unwrap_or("bench/data/top1m.txt");

    // Parse mode: adaptive (default) vs conservative
    let mode_str = args.get(2).map(|s| s.as_str()).unwrap_or("adaptive");
    let mode = TuningMode::from_str(mode_str).unwrap_or(TuningMode::Adaptive);

    // Hardware detection & bottleneck-constrained adaptive tuning profile generation
    let hw = HostHardware::detect();
    let mut profile = TuningProfile::for_hardware(hw, mode);

    // Allow optional manual override if passed
    if let Some(concurrency_override) = args.get(3).and_then(|s| s.parse::<usize>().ok()) {
        if concurrency_override > 0 {
            profile.global_concurrency = concurrency_override;
            profile.mpsc_buffer_capacity = concurrency_override * 2;
        }
    }

    let limit: usize = args
        .get(4)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    let out_json = args
        .get(5)
        .map(|s| s.as_str())
        .unwrap_or("bench/results/final/pagesift_real_1m.json");

    eprintln!("================================================================================");
    eprintln!("[PageSift 0.50] Balanced Dynamic Adaptive Engine Initialized");
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
        ..Default::default()
    });

    let (tx, rx) = mpsc::channel::<String>(channel_cap);
    let rx = Arc::new(Mutex::new(rx));

    // Producer: streams lines lazily to preserve strict O(1) memory
    let url_file_clone = url_file.to_string();
    let producer = tokio::spawn(async move {
        if let Ok(file) = File::open(&url_file_clone) {
            let reader = BufReader::new(file);
            let mut count = 0;
            for line in reader.lines() {
                if count >= limit {
                    break;
                }
                if let Ok(l) = line {
                    let l = l.trim().to_string();
                    if !l.is_empty() {
                        count += 1;
                        if tx.send(l).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });

    let t0 = Instant::now();
    let mut workers = Vec::with_capacity(concurrency);
    let milestone_50k_done = Arc::new(AtomicBool::new(false));

    // Live Reporter & Checkpoint task
    let metrics_rep = Arc::clone(&metrics);
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
                + metrics_rep.timeout.load(Ordering::Relaxed);

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
                        let msg = err.message.to_lowercase();
                        if msg.contains("timeout") {
                            metrics.timeout.fetch_add(1, Ordering::Relaxed);
                        } else if msg.contains("403") || msg.contains("404") || msg.contains("429")
                        {
                            metrics.http_4xx.fetch_add(1, Ordering::Relaxed);
                        } else if msg.contains("500")
                            || msg.contains("502")
                            || msg.contains("503")
                            || msg.contains("504")
                        {
                            metrics.http_5xx.fetch_add(1, Ordering::Relaxed);
                        } else {
                            metrics.dns_or_connect_err.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                metrics.processed.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    let _ = producer.await;
    for w in workers {
        let _ = w.await;
    }
    reporter.abort();

    let elapsed_s = t0.elapsed().as_secs_f64();
    let succ = metrics.success.load(Ordering::Relaxed);
    let valid = metrics.valid_content.load(Ordering::Relaxed);
    let dup = metrics.duplicate.load(Ordering::Relaxed);
    let timeouts = metrics.timeout.load(Ordering::Relaxed);
    let h4xx = metrics.http_4xx.load(Ordering::Relaxed);
    let h5xx = metrics.http_5xx.load(Ordering::Relaxed);
    let dns_conn = metrics.dns_or_connect_err.load(Ordering::Relaxed);
    let failed = timeouts + h4xx + h5xx + dns_conn;
    let bytes = metrics.total_bytes.load(Ordering::Relaxed);
    let struct_count = metrics.structured_data_count.load(Ordering::Relaxed);
    let qps = (succ + failed) as f64 / elapsed_s.max(0.001);

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
        "engine": "PageSift 0.50",
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
        "metrics": {
            "success_200": succ,
            "valid_content": valid,
            "duplicate_pages": dup,
            "failed_total": failed,
            "fail_details": {
                "timeout": timeouts,
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
