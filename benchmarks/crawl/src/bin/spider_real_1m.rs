//! Spider 2.53.9: Real-World Web Processing Benchmark against Tranco Top 1M
//! Matching exact hardware concurrency (768), connect timeout (3.5s), total timeout (10s),
//! streaming MPSC channel, Blake3 content deduplication, and periodic checkpoints.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use spider::Client;
use spider::page::Page;
use spider_transformations::transformation::content::{
    ReturnFormat, TransformConfig, transform_content,
};
use tokio::sync::{mpsc, Mutex};

#[derive(Default)]
struct Metrics {
    processed: AtomicUsize,
    success: AtomicUsize,
    valid_content: AtomicUsize,
    duplicate: AtomicUsize,
    failed: AtomicUsize,
    total_bytes: AtomicUsize,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let url_file = args.get(1).map(|s| s.as_str()).unwrap_or("benchmarks/data/top1m.txt");
    let concurrency: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(768);
    let limit: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1_000_000);
    let out_json = args.get(4).map(|s| s.as_str()).unwrap_or("benchmarks/history/0.50/results/spider_real_1m.json");

    eprintln!("================================================================================");
    eprintln!("[Spider 2.53.9] Real-World 1-Million Benchmark Initialized");
    eprintln!("Concurrency: {} | Connect Timeout: 3.5s | Total Timeout: 10.0s", concurrency);
    eprintln!("Target Dataset: {} (Limit: {} URLs)", url_file, limit);
    eprintln!("================================================================================");

    let client = Client::builder()
        .connect_timeout(Duration::from_millis(3500))
        .timeout(Duration::from_secs(10))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) PageSiftBench/Spider2.53")
        .build()?;

    let client = Arc::new(client);
    let metrics = Arc::new(Metrics::default());
    let seen_hashes = Arc::new(Mutex::new(HashSet::<[u8; 16]>::new()));

    let (tx, rx) = mpsc::channel::<String>(concurrency * 2);
    let rx = Arc::new(Mutex::new(rx));

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

    // Live reporter
    let metrics_rep = Arc::clone(&metrics);
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
            let fail = metrics_rep.failed.load(Ordering::Relaxed);

            eprintln!(
                "[Spider Monitor] Progress: {}/{} ({:.1}%) | Inst QPS: {:.1} | Sustained QPS: {:.1} | Succ: {} (Valid: {}, Dup: {}) | Fail: {} | Elapsed: {:.1}s",
                current, limit, (current as f64 / limit as f64) * 100.0, inst_qps, overall_qps, succ, valid, dup, fail, total_el
            );

            // Periodic checkpoint dump
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

            last_processed = current;
            last_t = Instant::now();
            if current >= limit {
                break;
            }
        }
    });

    let transform_cfg = TransformConfig {
        readability: false,
        return_format: ReturnFormat::Markdown,
        filter_images: false,
        clean_html: false,
        filter_svg: true,
        main_content: true,
    };

    for _ in 0..concurrency {
        let client = Arc::clone(&client);
        let metrics = Arc::clone(&metrics);
        let seen = Arc::clone(&seen_hashes);
        let rx = Arc::clone(&rx);
        let t_cfg = transform_cfg.clone();

        workers.push(tokio::spawn(async move {
            loop {
                let url = {
                    let mut lock = rx.lock().await;
                    lock.recv().await
                };
                let Some(raw_url) = url else {
                    break;
                };

                let target_url = if raw_url.starts_with("http://") || raw_url.starts_with("https://") {
                    raw_url
                } else {
                    format!("https://{}", raw_url)
                };

                let page = Page::new(&target_url, &client).await;
                let html = page.get_html();
                if !html.is_empty() && html.len() > 100 {
                    metrics.success.fetch_add(1, Ordering::Relaxed);
                    let md = transform_content(&page, &t_cfg, &None, &None, &None);
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
                } else {
                    metrics.failed.fetch_add(1, Ordering::Relaxed);
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
    let failed = metrics.failed.load(Ordering::Relaxed);
    let bytes = metrics.total_bytes.load(Ordering::Relaxed);
    let qps = (succ + failed) as f64 / elapsed_s.max(0.001);

    eprintln!(
        "[Spider Complete] Processed {} URLs in {:.2}s ({:.1} sustained QPS) | Succ: {}, Valid: {}, Dup: {}, Fail: {} | Bytes: {} MB",
        succ + failed, elapsed_s, qps, succ, valid, dup, failed, bytes / (1024 * 1024)
    );

    let summary = serde_json::json!({
        "engine": "Spider 2.53.9",
        "urls_processed": succ + failed,
        "elapsed_s": elapsed_s,
        "sustained_qps": qps,
        "metrics": {
            "success_200": succ,
            "valid_content": valid,
            "duplicate_pages": dup,
            "failed_total": failed,
            "total_bytes_extracted": bytes,
            "valid_content_ratio": valid as f64 / succ.max(1) as f64,
        }
    });

    if let Some(parent) = std::path::Path::new(out_json).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out_f = File::create(out_json)?;
    out_f.write_all(serde_json::to_string_pretty(&summary)?.as_bytes())?;
    eprintln!("[Spider] Final metrics written to {}", out_json);

    Ok(())
}
