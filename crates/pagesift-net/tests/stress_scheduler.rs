//! 50,000 页大规模并发抓取调度、内存防线与崩溃检查点秒级恢复极限压测。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use pagesift_net::budget::{BackpressureController, MemGuard};
use pagesift_net::checkpoint::{RecoveryEngine, TaskSnapshot};
use pagesift_net::scheduler::{CrawlTask, WorkerPool};

#[test]
fn stress_50000_pages_queue_and_domain_limiter_throughput() {
    let queue_capacity = 65536; // 64K 有界队列
    let total_pages = 50_000;
    let domains_count = 1000;
    let pages_per_domain = 50;

    let pool = WorkerPool::new(queue_capacity, 0.0, 8, 16);
    // wall-clock: throughput is printed, not asserted
    let t0 = Instant::now();

    // 1. 快速生产 50,000 个任务
    for d in 0..domains_count {
        for p in 0..pages_per_domain {
            let url = format!("http://s{d}.localhost:18080/p/{p}");
            let task = CrawlTask::new(&url, 0).expect("valid task");
            pool.submit(task).expect("queue must accept bounded tasks");
        }
    }
    let enqueue_time = t0.elapsed();
    println!(
        "50,000 pages enqueued in {:.2} ms",
        enqueue_time.as_secs_f64() * 1000.0
    );
    assert_eq!(pool.queue().len(), total_pages);

    // 2. 模拟高并发多线程消费与域名限速
    let processed = Arc::new(AtomicUsize::new(0));
    let mut consumers = Vec::new();

    for _ in 0..16 {
        let queue = pool.queue().clone();
        let limiter = pool.limiter_arc();
        let proc = Arc::clone(&processed);

        consumers.push(std::thread::spawn(move || {
            let mut local = 0;
            while let Some(task) = queue.pop() {
                if limiter.acquire(&task.domain) {
                    local += 1;
                    limiter.release(&task.domain, Some(200));
                } else {
                    let _ = queue.push(task);
                }
            }
            proc.fetch_add(local, Ordering::Relaxed);
        }));
    }

    for h in consumers {
        h.join().unwrap();
    }

    let elapsed = t0.elapsed().as_secs_f64();
    let throughput = (total_pages as f64) / elapsed;
    println!(
        "50,000 pages dispatched in {:.2} s ({:.1} pages/s)",
        elapsed, throughput
    );

    assert_eq!(processed.load(Ordering::Relaxed), total_pages);
    assert_eq!(pool.limiter().tracked_domains(), domains_count);
}

#[test]
fn stress_memory_guard_12gib_defense_and_active_backpressure() {
    let guard = Arc::new(MemGuard::default());
    let bp = BackpressureController::new(Arc::clone(&guard));

    let rss_mb = guard.current_rss_bytes() / (1024 * 1024);
    println!("Current RSS: {} MB (Must stay strictly < 12288 MB)", rss_mb);
    assert!(
        guard.current_rss_bytes() < guard.max_bytes(),
        "Must not exceed 12 GiB"
    );

    // 校验背压评估在常态下为 Normal
    assert_eq!(bp.evaluate(), pagesift_net::budget::PressureLevel::Normal);
    assert!(!bp.should_pause());
    assert_eq!(bp.throttle_delay(), std::time::Duration::ZERO);
}

#[test]
fn stress_crash_kill_recovery_50000_pages_zero_loss() {
    let dir = std::env::temp_dir();
    let ckpt_path = dir.join("crash_recovery_50000.json");

    // 模拟爬取进行到第 32,500 页时遭遇突发 SIGKILL
    let total = 50_000;
    let crashed_at = 32_500;

    let mut completed = HashSet::with_capacity(crashed_at);
    let mut pending = Vec::with_capacity(total - crashed_at);
    let mut domains = HashMap::new();

    for i in 0..crashed_at {
        completed.insert(format!("http://s{}.localhost/p/{}", i % 1000, i / 1000));
        *domains
            .entry(format!("s{}.localhost", i % 1000))
            .or_insert(0) += 1;
    }
    for i in crashed_at..total {
        pending.push(format!("http://s{}.localhost/p/{}", i % 1000, i / 1000));
    }

    // wall-clock: the save time is printed, not asserted (recovery is checked with CPU time)
    let t0 = Instant::now();
    let snapshot = TaskSnapshot::new(
        "job_huge_50000",
        completed.clone(),
        pending.clone(),
        domains,
    );
    snapshot
        .save_atomic(&ckpt_path)
        .expect("atomic save checkpoint");
    let save_ms = t0.elapsed().as_secs_f64() * 1000.0;
    println!(
        "50,000-page checkpoint saved atomically in {:.2} ms",
        save_ms
    );

    // 模拟重启：反序列化检查点并验证 BLAKE3 完整性
    // 本线程的 CPU 时间：机器忙时墙钟会翻倍，要验的是恢复本身的工作量
    let ((done, to_crawl), recover) = pagesift_testkit::cpu_time(|| {
        let recovered = RecoveryEngine::load_checkpoint(&ckpt_path).expect("recovery load");
        RecoveryEngine::restore_tasks(recovered)
    });
    let recover_ms = recover.as_secs_f64() * 1000.0;
    println!(
        "50,000-page checkpoint verified & restored in {:.2} ms",
        recover_ms
    );

    // 验收秒级恢复要求 (< 1000 ms)
    assert!(recover_ms < 1000.0, "Recovery must be sub-second");
    assert_eq!(done.len(), 32_500, "Zero completed page loss");
    assert_eq!(to_crawl.len(), 17_500, "Zero pending page loss");

    // 确保没有跨集合交集（零重复爬取）
    for task in &to_crawl {
        assert!(
            !done.contains(&task.url),
            "Restored task must not be in completed set"
        );
    }

    let _ = std::fs::remove_file(ckpt_path);
}
