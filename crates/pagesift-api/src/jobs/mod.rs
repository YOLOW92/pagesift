//! 异步任务（批量抓取和整站爬取）：进度、结果、错误，过期后清理。
//!
//! 给了存储（F-6.4）时任务落盘，服务重启后：已结束的任务照样能查（直到过期），
//! 进行中的任务从断点继续。存储布局（都在 [`pagesift_store::Tasks`] 里）：
//!
//! - KV `job/{id}`：任务元数据（类型、状态、创建时间、原始请求、webhook、调用方）；
//! - 列表 `job/{id}/pages`：完成的文档，按完成顺序，序号就是分页下标；
//! - 列表 `job/{id}/errors`：页面错误；
//! - 列表 `job/{id}/frontier`：爬取入过队的 URL 和深度，按入队顺序；
//! - 集合 `job/{id}/done`：已完成的 URL（爬取）或下标（批量抓取）。
//!
//! 每个页面先写文档再记完成：崩溃时最多重复抓一个页面，不会丢页面。
//! 不给存储时一切留在内存里，行为和以前一样。

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::SystemTime;

use pagesift_store::Store;
use tokio::sync::{Mutex, Semaphore};

use crate::engine::{CrawlResume, Engine};
use crate::error::{ApiError, ErrorCode};
use crate::limits::Limiter;
use crate::metrics::Metrics;
use crate::types::{CrawlRequest, JobError, ScrapeOptions};
use crate::webhook::{Event, Hook, Webhooks};

mod sink;
pub mod state;
mod store;
pub mod time;

pub use state::{JOB_TTL, Job, JobKind, JobState, Start, StatusPage};
pub use time::rfc3339;

use sink::JobSink;
use store::{BatchSpec, Meta, delete_job, list_key, meta_key, to_ms};

struct Inner {
    map: Mutex<HashMap<String, Job>>,
    store: Option<Store>,
    limiter: Arc<Limiter>,
    webhooks: Option<Webhooks>,
    metrics: Arc<Metrics>,
}

#[derive(Clone)]
pub struct Jobs {
    inner: Arc<Inner>,
}

impl Default for Jobs {
    /// 只在内存里的任务表：不落盘、不限额、不发 webhook。
    fn default() -> Self {
        Self::new(None, Arc::default(), None, Arc::default())
    }
}

impl Jobs {
    pub(crate) fn new(
        store: Option<Store>,
        limiter: Arc<Limiter>,
        webhooks: Option<Webhooks>,
        metrics: Arc<Metrics>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                map: Mutex::new(HashMap::new()),
                store,
                limiter,
                webhooks,
                metrics,
            }),
        }
    }

    /// 清掉过期任务（连同存储里的数据）。
    async fn sweep(&self) {
        let now = SystemTime::now();
        let expired: Vec<String> = {
            let mut map = self.inner.map.lock().await;
            let ids: Vec<String> = map
                .iter()
                .filter(|(_, j)| j.expires_at() <= now)
                .map(|(id, _)| id.clone())
                .collect();
            for id in &ids {
                map.remove(id);
            }
            ids
        };
        if let (Some(store), false) = (&self.inner.store, expired.is_empty()) {
            store
                .blocking(move |s| {
                    for id in expired {
                        delete_job(s, &id);
                    }
                })
                .await;
        }
    }

    pub async fn get<R>(&self, id: &str, f: impl FnOnce(&Job) -> R) -> Option<R> {
        self.sweep().await;
        self.inner.map.lock().await.get(id).map(f)
    }

    /// 状态和从 `skip` 开始最多 `limit` 个文档。
    pub(crate) async fn status(&self, id: &str, skip: usize, limit: usize) -> Option<StatusPage> {
        self.sweep().await;
        let mut page = {
            let map = self.inner.map.lock().await;
            let job = map.get(id)?;
            let end = (skip + limit).min(job.data.len());
            StatusPage {
                state: job.state,
                total: job.total,
                completed: job.completed,
                expires_at: job.expires_at(),
                docs: job.docs(),
                data: job
                    .data
                    .get(skip.min(end)..end)
                    .unwrap_or_default()
                    .to_vec(),
                summary: job.summary.clone(),
            }
        };
        if let Some(store) = &self.inner.store
            && page.data.is_empty()
            && skip < page.docs
        {
            let key = list_key(id, "pages");
            page.data = store
                .blocking(move |s| s.tasks().list_range(key, skip as u64, limit))
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "reading job pages failed");
                    Vec::new()
                })
                .iter()
                .filter_map(|v| serde_json::from_slice(v).ok())
                .collect();
        }
        Some(page)
    }

    /// 任务的页面错误和被 robots.txt 拦下的 URL。
    pub(crate) async fn errors(&self, id: &str) -> Option<(Vec<JobError>, Vec<String>)> {
        self.sweep().await;
        let (mut errors, mut robots) = {
            let map = self.inner.map.lock().await;
            let job = map.get(id)?;
            (job.errors.clone(), job.robots_blocked.clone())
        };
        if let Some(store) = &self.inner.store {
            let key = list_key(id, "errors");
            let stored = store
                .blocking(move |s| s.tasks().list_items(key, usize::MAX))
                .await
                .unwrap_or_default();
            for v in stored {
                if let Ok(e) = serde_json::from_slice::<JobError>(&v) {
                    if e.code == ErrorCode::RobotsDisallowed.as_str() {
                        robots.push(e.url.clone());
                    }
                    errors.push(e);
                }
            }
        }
        Some((errors, robots))
    }

    pub async fn cancel(&self, id: &str) -> bool {
        let found = {
            let mut map = self.inner.map.lock().await;
            match map.get_mut(id) {
                Some(j) => {
                    if j.state == JobState::Scraping {
                        j.state = JobState::Cancelled;
                    }
                    true
                }
                None => false,
            }
        };
        if found {
            self.save_state(id).await;
        }
        found
    }

    pub async fn active(&self) -> Vec<String> {
        let map = self.inner.map.lock().await;
        map.iter()
            .filter(|(_, j)| j.state == JobState::Scraping)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// 按 (类型, 状态) 统计的任务数，给 `/metrics`。
    pub(crate) async fn counts(&self) -> BTreeMap<(&'static str, &'static str), u64> {
        let mut out = BTreeMap::new();
        for j in self.inner.map.lock().await.values() {
            *out.entry((j.kind.as_str(), j.state.as_str())).or_default() += 1;
        }
        out
    }

    /// 登记新任务：检查调用方的并发任务数，落盘元数据，发 `started` 事件。
    async fn register(
        &self,
        kind: JobKind,
        total: usize,
        start: Start,
        crawl: Option<CrawlRequest>,
        batch: Option<BatchSpec>,
    ) -> Result<(String, Option<Hook>), ApiError> {
        let id = uuid::Uuid::new_v4().to_string();
        let job = Job::new(kind, total, start.owner.clone());
        let meta = Meta {
            kind,
            state: JobState::Scraping,
            created_ms: to_ms(job.created),
            total,
            completed: 0,
            owner: start.owner.as_ref().map(|c| c.account.to_string()),
            webhook: start.webhook.clone(),
            crawl,
            batch,
            summary: None,
        };
        {
            let mut map = self.inner.map.lock().await;
            if let Some(c) = &start.owner {
                if c.limits.max_concurrent_jobs.is_some() {
                    let active = map
                        .values()
                        .filter(|j| {
                            j.state == JobState::Scraping
                                && j.owner.as_ref().is_some_and(|o| o.account == c.account)
                        })
                        .count();
                    self.inner.limiter.check_jobs(c, active)?;
                }
                self.inner.limiter.check_quota(c)?;
            }
            map.insert(id.clone(), job);
        }
        if let Some(store) = &self.inner.store {
            let (key, value) = (meta_key(&id), serde_json::to_vec(&meta).unwrap_or_default());
            if let Err(e) = store.blocking(move |s| s.tasks().kv_put(key, &value)).await {
                tracing::warn!(job = %id, error = %e, "saving job failed; it will not survive a restart");
            }
        }
        self.inner.metrics.job_started(kind.as_str());
        tracing::info!(
            job = %id,
            kind = kind.as_str(),
            account = start.owner.as_ref().map(|c| &*c.account),
            "job started"
        );
        let hook = match (&self.inner.webhooks, start.webhook) {
            (Some(w), Some(cfg)) => {
                let hook = w.start(&id, kind.as_str(), cfg);
                hook.send(Event::Started, Vec::new(), None);
                Some(hook)
            }
            _ => None,
        };
        Ok((id, hook))
    }

    /// 启动批量抓取，立即返回任务 id；每个 URL 的结果和错误陆续写入任务。
    pub async fn start_batch(
        &self,
        engine: Arc<dyn Engine>,
        urls: Vec<String>,
        opts: ScrapeOptions,
        concurrency: usize,
    ) -> String {
        self.start_batch_with(engine, urls, opts, concurrency, Start::default())
            .await
            .expect("no limits without a caller")
    }

    pub(crate) async fn start_batch_with(
        &self,
        engine: Arc<dyn Engine>,
        urls: Vec<String>,
        opts: ScrapeOptions,
        concurrency: usize,
        start: Start,
    ) -> Result<String, ApiError> {
        let spec = BatchSpec {
            urls,
            options: opts,
            concurrency,
        };
        let total = spec.urls.len();
        let (id, hook) = self
            .register(
                JobKind::BatchScrape,
                total,
                start,
                None,
                self.inner.store.is_some().then(|| spec.clone()),
            )
            .await?;
        let pending = spec.urls.into_iter().enumerate().collect();
        self.spawn_batch(id.clone(), engine, pending, spec.options, concurrency, hook);
        Ok(id)
    }

    fn sink(
        &self,
        id: &str,
        kind: JobKind,
        hook: Option<Hook>,
        resume: Option<CrawlResume>,
    ) -> Arc<JobSink> {
        Arc::new(JobSink {
            jobs: self.clone(),
            id: id.to_string(),
            kind,
            hook,
            resume: StdMutex::new(resume),
            quota_hit: AtomicBool::new(false),
        })
    }

    pub(super) fn spawn_batch(
        &self,
        id: String,
        engine: Arc<dyn Engine>,
        pending: Vec<(usize, String)>,
        opts: ScrapeOptions,
        concurrency: usize,
        hook: Option<Hook>,
    ) {
        let sink = self.sink(&id, JobKind::BatchScrape, hook, None);
        let opts = Arc::new(opts);
        tokio::spawn(async move {
            let sem = Arc::new(Semaphore::new(concurrency.max(1)));
            let mut tasks = tokio::task::JoinSet::new();
            for (i, url) in pending {
                let (sink, engine, opts, sem) =
                    (sink.clone(), engine.clone(), opts.clone(), sem.clone());
                tasks.spawn(async move {
                    let _permit = sem.acquire_owned().await.expect("semaphore open");
                    if sink.stopped().await {
                        return;
                    }
                    let done = i.to_string();
                    match engine.scrape(&url, &opts).await {
                        Ok(doc) => sink.record_page(done, doc).await,
                        Err(e) => sink.record_error(done, url, e).await,
                    }
                });
            }
            while tasks.join_next().await.is_some() {}
            sink.finish(Ok(())).await;
        });
    }

    /// 启动整站爬取，立即返回任务 id。
    pub async fn start_crawl(&self, engine: Arc<dyn Engine>, req: CrawlRequest) -> String {
        self.start_crawl_with(engine, req, Start::default())
            .await
            .expect("no limits without a caller")
    }

    pub(crate) async fn start_crawl_with(
        &self,
        engine: Arc<dyn Engine>,
        req: CrawlRequest,
        start: Start,
    ) -> Result<String, ApiError> {
        let (id, hook) = self
            .register(
                JobKind::Crawl,
                0,
                start,
                self.inner.store.is_some().then(|| req.clone()),
                None,
            )
            .await?;
        self.spawn_crawl(id.clone(), engine, req, hook, None);
        Ok(id)
    }

    pub(super) fn spawn_crawl(
        &self,
        id: String,
        engine: Arc<dyn Engine>,
        req: CrawlRequest,
        hook: Option<Hook>,
        resume: Option<CrawlResume>,
    ) {
        let sink = self.sink(&id, JobKind::Crawl, hook, resume);
        tokio::spawn(async move {
            let result = engine.crawl(&req, sink.clone()).await;
            sink.finish(result.map_err(|e| (req.url.clone(), e))).await;
        });
    }
}
