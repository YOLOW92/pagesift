//! 高并发无锁任务调度与并发池。

pub mod domain_limiter;
pub mod ring_queue;
pub mod worker_pool;

pub use domain_limiter::DomainLimiter;
pub use ring_queue::{PushError, RingQueue};
pub use worker_pool::{CrawlResult, CrawlTask, WorkerPool};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_lock_free_ring_queue() {
        let q = RingQueue::new(16);
        assert_eq!(q.capacity(), 16);
        assert!(q.is_empty());

        for i in 0..16 {
            assert!(q.push(i).is_ok());
        }
        // 满了
        assert!(q.push(999).is_err());
        assert_eq!(q.len(), 16);

        for i in 0..16 {
            assert_eq!(q.pop(), Some(i));
        }
        assert!(q.pop().is_none());
        assert!(q.is_empty());
    }

    #[test]
    fn test_concurrent_mpmc_queue() {
        let q = Arc::new(RingQueue::new(1024));
        let mut producers = Vec::new();
        let items_per_thread = 500;

        for p in 0..4 {
            let q_clone = Arc::clone(&q);
            producers.push(thread::spawn(move || {
                for i in 0..items_per_thread {
                    let mut val = p * 1000 + i;
                    while let Err(PushError::Full(v)) = q_clone.push(val) {
                        val = v;
                        thread::yield_now();
                    }
                }
            }));
        }

        let mut consumers = Vec::new();
        let received = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let total_items = 4 * items_per_thread;

        for _ in 0..4 {
            let q_clone = Arc::clone(&q);
            let rec = Arc::clone(&received);
            consumers.push(thread::spawn(move || {
                while rec.load(std::sync::atomic::Ordering::SeqCst) < total_items {
                    if q_clone.pop().is_some() {
                        rec.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    } else {
                        thread::yield_now();
                    }
                }
            }));
        }

        for h in producers {
            h.join().unwrap();
        }
        for h in consumers {
            h.join().unwrap();
        }
        assert!(received.load(std::sync::atomic::Ordering::SeqCst) >= total_items);
    }

    #[test]
    fn test_domain_limiter_inflight_and_penalty() {
        let limiter = DomainLimiter::new(100.0, 2);
        let domain = "example.com";

        assert!(limiter.acquire(domain));
        assert!(limiter.acquire(domain));
        // 已达 max_inflight 2，无法再获取
        assert!(!limiter.acquire(domain));

        // 释放一个，恢复可获取
        limiter.release(domain, Some(200));
        assert!(limiter.acquire(domain));

        // 遇到 429 处罚，进入退避
        limiter.release(domain, Some(429));
        assert!(!limiter.acquire(domain));
    }
}
