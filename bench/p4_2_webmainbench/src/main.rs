//! WebMainBench D1 extractor runner (v2).
//!
//! Differences from v1:
//! - results are written per row as soon as they finish (unbuffered, under a
//!   mutex), so one pathological row can no longer discard the whole batch;
//! - a watchdog reports progress and, if a single row exceeds the hard
//!   timeout, prints the row index/track_id/HTML size and exits with code 2;
//! - the output file is unordered and carries an explicit `row` field; the
//!   finalizer restores dataset order and fills hung/timed-out rows.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut positional = Vec::new();
    let mut hard_timeout_secs: u64 = 120;
    let mut warn_secs: u64 = 30;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--hard-timeout-secs" => {
                hard_timeout_secs = it.next().ok_or("missing value")?.parse()?;
            }
            "--soft-warn-secs" => {
                warn_secs = it.next().ok_or("missing value")?.parse()?;
            }
            other => positional.push(other.to_string()),
        }
    }
    if positional.len() != 3 {
        return Err(
            "usage: pagesift-p4-2-webmainbench <input.jsonl> <output.parts.jsonl> <threads> \
             [--hard-timeout-secs N] [--soft-warn-secs N]"
                .into(),
        );
    }
    let threads: usize = positional[2].parse()?;
    if threads == 0 {
        return Err("threads must be positive".into());
    }

    let load_start = Instant::now();
    let lines: Vec<String> = BufReader::new(File::open(&positional[0])?)
        .lines()
        .collect::<Result<_, _>>()?;
    let n = lines.len();
    eprintln!(
        "loaded rows={n} bytes={} in {:.3}s",
        lines.iter().map(|l| l.len()).sum::<usize>(),
        load_start.elapsed().as_secs_f64()
    );

    let lines = Arc::new(lines);
    let out = Arc::new(Mutex::new(File::create(&positional[1])?));
    let in_flight: Arc<Vec<AtomicU64>> = Arc::new((0..n).map(|_| AtomicU64::new(0)).collect());
    let completed = Arc::new(AtomicUsize::new(0));
    let next = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    let monitor = {
        let lines = Arc::clone(&lines);
        let in_flight = Arc::clone(&in_flight);
        let completed = Arc::clone(&completed);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let started = Instant::now();
            loop {
                std::thread::sleep(Duration::from_secs(5));
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let now = now_ms();
                let mut oldest = (0u64, usize::MAX);
                for (i, slot) in in_flight.iter().enumerate() {
                    let s = slot.load(Ordering::Relaxed);
                    if s != 0 {
                        let age = now.saturating_sub(s);
                        if age > oldest.0 {
                            oldest = (age, i);
                        }
                    }
                }
                let done = completed.load(Ordering::Relaxed);
                eprintln!(
                    "progress done={done}/{n} elapsed={:.1}s oldest_s={:.1} row={}",
                    started.elapsed().as_secs_f64(),
                    oldest.0 as f64 / 1000.0,
                    oldest.1
                );
                if oldest.1 != usize::MAX && oldest.0 / 1000 >= warn_secs {
                    let row = &lines[oldest.1];
                    let parsed: serde_json::Value =
                        serde_json::from_str(row).unwrap_or(serde_json::Value::Null);
                    let track_id = parsed["track_id"].as_str().unwrap_or("");
                    let html = parsed["html"].as_str().unwrap_or("");
                    eprintln!(
                        "slow row={} track_id={track_id} html_bytes={} age_s={:.1}",
                        oldest.1,
                        html.len(),
                        oldest.0 as f64 / 1000.0
                    );
                    if hard_timeout_secs > 0 && oldest.0 / 1000 >= hard_timeout_secs {
                        eprintln!(
                            "HANG row={} track_id={track_id} html_bytes={} age_s={:.1}",
                            oldest.1,
                            html.len(),
                            oldest.0 as f64 / 1000.0
                        );
                        std::process::exit(2);
                    }
                }
            }
        })
    };

    let start = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..threads.min(n.max(1)) {
            let lines = Arc::clone(&lines);
            let out = Arc::clone(&out);
            let in_flight = Arc::clone(&in_flight);
            let completed = Arc::clone(&completed);
            let next = Arc::clone(&next);
            scope.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    in_flight[i].store(now_ms(), Ordering::Relaxed);
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        let row: serde_json::Value = serde_json::from_str(&lines[i])?;
                        let id = row["track_id"].as_str().unwrap_or("");
                        let html = row["html"].as_str().unwrap_or("");
                        let url = row["url"].as_str().unwrap_or("");
                        let opts = pagesift_content::Options {
                            base_url: url::Url::parse(url).ok(),
                            ..Default::default()
                        };
                        let t = Instant::now();
                        let markdown = pagesift_content::process(html, &opts).markdown;
                        Ok::<_, serde_json::Error>(serde_json::json!({
                            "row": i,
                            "track_id": id,
                            "markdown": markdown,
                            "extract_ms": t.elapsed().as_secs_f64() * 1000.0,
                            "status": "ok"
                        }))
                    }));
                    let value = match result {
                        Ok(Ok(value)) => value,
                        Ok(Err(error)) => serde_json::json!({
                            "row": i, "track_id": format!("row-{i}"),
                            "markdown": "", "status": "input_error",
                            "error": error.to_string()
                        }),
                        Err(_) => serde_json::json!({
                            "row": i, "track_id": format!("row-{i}"),
                            "markdown": "", "status": "panic"
                        }),
                    };
                    {
                        let mut file = out.lock().expect("output lock");
                        writeln!(file, "{value}").expect("write result");
                    }
                    in_flight[i].store(0, Ordering::Relaxed);
                    completed.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    stop.store(true, Ordering::Relaxed);
    // 先记实际处理墙钟，再等 watchdog 线程退出——否则 5 s 轮询会被算进吞吐。
    let wall = start.elapsed().as_secs_f64();
    let _ = monitor.join();
    eprintln!(
        "rows={n} threads={} wall_s={:.3} parts={}",
        threads,
        wall,
        completed.load(Ordering::Relaxed)
    );
    Ok(())
}
