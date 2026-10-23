//! Tokio 多线程高并发工作池模型。
//! 协调无锁调度队列与域名限速器，最大化压榨网络 IO 与系统吞吐。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use url::Url;

use super::domain_limiter::DomainLimiter;
use super::ring_queue::RingQueue;

/// 爬取任务单元
#[derive(Debug, Clone)]
pub struct CrawlTask {
    pub url: String,
    pub domain: String,
    pub depth: u32,
    pub retries: u32,
}

impl CrawlTask {
    pub fn new(url_str: &str, depth: u32) -> Option<Self> {
        let u = Url::parse(url_str).ok()?;
        let domain = u.host_str()?.to_string();
        Some(Self {
            url: url_str.to_string(),
            domain,
            depth,
            retries: 0,
        })
    }
}

/// 爬取任务响应结果
#[derive(Debug, Clone)]
pub struct CrawlResult {
    pub url: String,
    pub status: u16,
    pub bytes: usize,
    pub duration_ms: u64,
    pub error: Option<String>,
}

/// 高并发工作池
pub struct WorkerPool {
    queue: RingQueue<CrawlTask>,
    limiter: Arc<DomainLimiter>,
    concurrency: usize,
    active_workers: Arc<AtomicUsize>,
    shutdown: Arc<AtomicBool>,
    completed_count: Arc<AtomicUsize>,
    error_count: Arc<AtomicUsize>,
}

impl WorkerPool {
    pub fn new(
        queue_capacity: usize,
        default_rate_per_domain: f64,
        max_inflight_per_domain: usize,
        concurrency: usize,
    ) -> Self {
        Self {
            queue: RingQueue::new(queue_capacity),
            limiter: Arc::new(DomainLimiter::new(
                default_rate_per_domain,
                max_inflight_per_domain,
            )),
            concurrency: concurrency.max(1),
            active_workers: Arc::new(AtomicUsize::new(0)),
            shutdown: Arc::new(AtomicBool::new(false)),
            completed_count: Arc::new(AtomicUsize::new(0)),
            error_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// 向调度队列投递新任务。
    pub fn submit(&self, task: CrawlTask) -> Result<(), CrawlTask> {
        self.queue.push(task).map_err(|e| match e {
            super::ring_queue::PushError::Full(t) => t,
        })
    }

    /// 获取任务队列引用。
    pub fn queue(&self) -> &RingQueue<CrawlTask> {
        &self.queue
    }

    /// 获取域名限速器引用。
    pub fn limiter(&self) -> &DomainLimiter {
        &self.limiter
    }

    /// 获取域名限速器 Arc 克隆。
    pub fn limiter_arc(&self) -> Arc<DomainLimiter> {
        Arc::clone(&self.limiter)
    }

    /// 已完成的任务总数。
    pub fn completed(&self) -> usize {
        self.completed_count.load(Ordering::Relaxed)
    }

    /// 发生错误的任务总数。
    pub fn errors(&self) -> usize {
        self.error_count.load(Ordering::Relaxed)
    }

    /// 触发优雅停止信号。
    pub fn stop(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    /// 是否已接收到停止信号。
    pub fn is_stopped(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    /// 启动后台工作线程池，执行异步请求与结果分发。
    pub async fn run_with_handler<F, Fut>(&self, handler: F)
    where
        F: Fn(CrawlTask) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<CrawlResult, String>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        let mut handles = Vec::with_capacity(self.concurrency);

        for _ in 0..self.concurrency {
            let queue = self.queue.clone();
            let limiter = Arc::clone(&self.limiter);
            let shutdown = Arc::clone(&self.shutdown);
            let handler = Arc::clone(&handler);
            let completed = Arc::clone(&self.completed_count);
            let errors = Arc::clone(&self.error_count);
            let active = Arc::clone(&self.active_workers);

            let h = tokio::spawn(async move {
                active.fetch_add(1, Ordering::Relaxed);
                while !shutdown.load(Ordering::Relaxed) {
                    let Some(task) = queue.pop() else {
                        tokio::time::sleep(Duration::from_millis(2)).await;
                        continue;
                    };

                    if !limiter.acquire(&task.domain) {
                        // 该域名限速或当前并发已满，稍后重新放回队列
                        let _ = queue.push(task);
                        tokio::time::sleep(Duration::from_millis(1)).await;
                        continue;
                    }

                    let domain = task.domain.clone();
                    let res = handler(task).await;
                    match res {
                        Ok(r) => {
                            limiter.release(&domain, Some(r.status));
                            completed.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(_) => {
                            limiter.release(&domain, Some(500));
                            errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                active.fetch_sub(1, Ordering::Relaxed);
            });
            handles.push(h);
        }

        for h in handles {
            let _ = h.await;
        }
    }
}
