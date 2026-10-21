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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pagesift_store::Store;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Semaphore};

use crate::engine::{BoxFuture, CrawlResume, CrawlSink, Engine};
use crate::error::{ApiError, ErrorCode};
use crate::limits::{Caller, KeyLimits, Limiter, quota_error};
use crate::metrics::{Metrics, PageSource};
use crate::types::{CrawlRequest, Document, JobError, ScrapeOptions};
use crate::webhook::{Event, Hook, WebhookConfig, Webhooks};

/// Firecrawl 的任务结果保留 24 小时。
pub const JOB_TTL: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Scraping,
    Completed,
    Cancelled,
    Failed,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Scraping => "scraping",
            JobState::Completed => "completed",
            JobState::Cancelled => "cancelled",
            JobState::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Crawl,
    BatchScrape,
}

impl JobKind {
    /// 也是 webhook 事件类型的前缀（`crawl.page`、`batch_scrape.completed`）。
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Crawl => "crawl",
            JobKind::BatchScrape => "batch_scrape",
        }
    }

    fn pages(self) -> PageSource {
        match self {
            JobKind::Crawl => PageSource::Crawl,
            JobKind::BatchScrape => PageSource::Batch,
        }
    }
}

#[derive(Debug)]
pub struct Job {
    pub kind: JobKind,
    pub state: JobState,
    pub total: usize,
    pub completed: usize,
    /// 按完成顺序追加，分页用下标。任务落盘时文档在存储里，这里为空。
    pub data: Vec<Document>,
    /// 任务落盘时错误在存储里，这里为空。
    pub errors: Vec<JobError>,
    pub robots_blocked: Vec<String>,
    pub created: SystemTime,
    /// 存储里已有的文档数。
    stored_docs: usize,
    owner: Option<Caller>,
}

impl Job {
    fn new(kind: JobKind, total: usize, owner: Option<Caller>) -> Self {
        Self {
            kind,
            state: JobState::Scraping,
            total,
            completed: 0,
            data: Vec::new(),
            errors: Vec::new(),
            robots_blocked: Vec::new(),
            created: SystemTime::now(),
            stored_docs: 0,
            owner,
        }
    }

    pub fn expires_at(&self) -> SystemTime {
        self.created + JOB_TTL
    }

    fn docs(&self) -> usize {
        self.data.len() + self.stored_docs
    }
}

/// 任务状态的一页。
pub(crate) struct StatusPage {
    pub state: JobState,
    pub total: usize,
    pub completed: usize,
    pub expires_at: SystemTime,
    /// 文档总数。
    pub docs: usize,
    pub data: Vec<Document>,
}

/// 启动任务时的调用方信息。
#[derive(Default)]
pub(crate) struct Start {
    pub owner: Option<Caller>,
    pub webhook: Option<WebhookConfig>,
}

/// 落盘的任务元数据。
#[derive(Serialize, Deserialize)]
struct Meta {
    kind: JobKind,
    state: JobState,
    created_ms: u64,
    total: usize,
    completed: usize,
    owner: Option<String>,
    webhook: Option<WebhookConfig>,
    crawl: Option<CrawlRequest>,
    batch: Option<BatchSpec>,
}

#[derive(Clone, Serialize, Deserialize)]
struct BatchSpec {
    urls: Vec<String>,
    options: ScrapeOptions,
    concurrency: usize,
}

fn meta_key(id: &str) -> String {
    format!("job/{id}")
}

fn list_key(id: &str, what: &str) -> String {
    format!("job/{id}/{what}")
}

fn to_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

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

    fn spawn_batch(
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

    fn spawn_crawl(
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

    /// 把任务的当前状态和计数写回存储。
    async fn save_state(&self, id: &str) {
        let Some(store) = &self.inner.store else {
            return;
        };
        let snapshot = {
            let map = self.inner.map.lock().await;
            map.get(id).map(|j| (j.state, j.total, j.completed))
        };
        let Some((state, total, completed)) = snapshot else {
            return;
        };
        let key = meta_key(id);
        let r = store
            .blocking(move |s| -> pagesift_store::Result<()> {
                let t = s.tasks();
                let Some(raw) = t.kv_get(&key)? else {
                    return Ok(());
                };
                let Ok(mut meta) = serde_json::from_slice::<Meta>(&raw) else {
                    return Ok(());
                };
                meta.state = state;
                meta.total = total;
                meta.completed = completed;
                t.kv_put(&key, &serde_json::to_vec(&meta).unwrap_or_default())
            })
            .await;
        if let Err(e) = r {
            tracing::warn!(job = %id, error = %e, "saving job state failed");
        }
    }

    /// 服务启动时从存储恢复任务：结束的任务照样能查，进行中的从断点继续，过期的删掉。
    /// `limits` 按调用方名字找回当前配置的限额。需要在 tokio 运行时里调用。
    pub(crate) fn restore(
        &self,
        engine: &Arc<dyn Engine>,
        limits: &dyn Fn(&str) -> Arc<KeyLimits>,
    ) {
        let Some(store) = &self.inner.store else {
            return;
        };
        let t = store.tasks();
        let keys = match t.kv_keys("job/") {
            Ok(k) => k,
            Err(e) => {
                tracing::error!(error = %e, "listing saved jobs failed");
                return;
            }
        };
        let now = SystemTime::now();
        let (mut finished, mut resumed, mut expired) = (0, 0, 0);
        for key in keys {
            let Some(id) = std::str::from_utf8(&key)
                .ok()
                .and_then(|k| k.strip_prefix("job/"))
                .filter(|id| !id.contains('/'))
                .map(String::from)
            else {
                continue;
            };
            let Some(meta) = t
                .kv_get(&key)
                .ok()
                .flatten()
                .and_then(|v| serde_json::from_slice::<Meta>(&v).ok())
            else {
                tracing::warn!(job = %id, "dropping an unreadable saved job");
                delete_job(store, &id);
                continue;
            };
            let created = UNIX_EPOCH + Duration::from_millis(meta.created_ms);
            if created + JOB_TTL <= now {
                delete_job(store, &id);
                expired += 1;
                continue;
            }
            let pages = t.list_len(list_key(&id, "pages")) as usize;
            let errors = t.list_len(list_key(&id, "errors")) as usize;
            let owner = meta.owner.as_deref().map(|a| Caller::named(a, limits(a)));
            let mut job = Job::new(meta.kind, meta.total, owner);
            job.created = created;
            job.state = meta.state;
            job.stored_docs = pages;
            job.completed = pages + errors;
            job.total = job.total.max(job.completed);
            if job.state != JobState::Scraping {
                finished += 1;
                self.inner
                    .map
                    .try_lock()
                    .expect("not shared yet")
                    .insert(id, job);
                continue;
            }
            // 进行中的任务：算出还没做完的部分，接着跑
            let done = list_key(&id, "done");
            let hook = match (&self.inner.webhooks, meta.webhook) {
                (Some(w), Some(cfg)) => Some(w.start(&id, meta.kind.as_str(), cfg)),
                _ => None,
            };
            match (meta.kind, meta.crawl, meta.batch) {
                (JobKind::BatchScrape, _, Some(spec)) => {
                    let pending: Vec<(usize, String)> = spec
                        .urls
                        .into_iter()
                        .enumerate()
                        .filter(|(i, _)| !t.set_contains(&done, i.to_string()).unwrap_or(false))
                        .collect();
                    job.total = job.completed + pending.len();
                    self.inner
                        .map
                        .try_lock()
                        .expect("not shared yet")
                        .insert(id.clone(), job);
                    tracing::info!(job = %id, kind = "batch_scrape", pending = pending.len(), "resuming job");
                    self.spawn_batch(
                        id,
                        engine.clone(),
                        pending,
                        spec.options,
                        spec.concurrency,
                        hook,
                    );
                }
                (JobKind::Crawl, Some(req), _) => {
                    let frontier: Vec<(String, usize)> = t
                        .list_items(list_key(&id, "frontier"), usize::MAX)
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|v| serde_json::from_slice(v).ok())
                        .collect();
                    let resume = (!frontier.is_empty()).then(|| {
                        let pending: Vec<(String, usize)> = frontier
                            .iter()
                            .filter(|(u, _)| !t.set_contains(&done, u).unwrap_or(false))
                            .cloned()
                            .collect();
                        CrawlResume {
                            enqueued: frontier.into_iter().map(|(u, _)| u).collect(),
                            pending,
                            done: job.completed,
                        }
                    });
                    if let Some(r) = &resume {
                        job.total =
                            (job.completed + r.pending.len()).min(req.limit.max(job.completed));
                    }
                    self.inner
                        .map
                        .try_lock()
                        .expect("not shared yet")
                        .insert(id.clone(), job);
                    tracing::info!(
                        job = %id,
                        kind = "crawl",
                        pending = resume.as_ref().map_or(0, |r| r.pending.len()),
                        "resuming job"
                    );
                    self.spawn_crawl(id, engine.clone(), req, hook, resume);
                }
                _ => {
                    tracing::warn!(job = %id, "saved job has no request to resume from");
                    job.state = JobState::Failed;
                    self.inner
                        .map
                        .try_lock()
                        .expect("not shared yet")
                        .insert(id, job);
                }
            }
            resumed += 1;
        }
        if finished + resumed + expired > 0 {
            tracing::info!(finished, resumed, expired, "restored saved jobs");
        }
    }
}

/// 删除一个任务在存储里的全部数据。
fn delete_job(store: &Store, id: &str) {
    let t = store.tasks();
    let r = (|| -> pagesift_store::Result<()> {
        for what in ["pages", "errors", "frontier"] {
            t.list_clear(list_key(id, what))?;
        }
        t.set_clear(list_key(id, "done"))?;
        t.kv_delete(meta_key(id))?;
        Ok(())
    })();
    if let Err(e) = r {
        tracing::warn!(job = %id, error = %e, "deleting an expired job failed");
    }
}

fn job_error(url: String, e: &ApiError) -> JobError {
    JobError {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: rfc3339(SystemTime::now()),
        url,
        code: e.code.as_str().to_string(),
        error: e.message.clone(),
    }
}

/// 一个任务的进度回报：批量抓取和整站爬取共用。
struct JobSink {
    jobs: Jobs,
    id: String,
    kind: JobKind,
    hook: Option<Hook>,
    resume: StdMutex<Option<CrawlResume>>,
    /// 调用方当天的页面配额用完了，任务提前结束。
    quota_hit: AtomicBool,
}

impl JobSink {
    fn store(&self) -> Option<&Store> {
        self.jobs.inner.store.as_ref()
    }

    async fn owner(&self) -> Option<Caller> {
        let map = self.jobs.inner.map.lock().await;
        map.get(&self.id).and_then(|j| j.owner.clone())
    }

    async fn running(&self) -> bool {
        let map = self.jobs.inner.map.lock().await;
        map.get(&self.id)
            .is_some_and(|j| j.state == JobState::Scraping)
    }

    /// 任务被取消，或者调用方的配额用完了。
    async fn stopped(&self) -> bool {
        let owner = {
            let map = self.jobs.inner.map.lock().await;
            match map.get(&self.id) {
                Some(j) if j.state == JobState::Scraping => j.owner.clone(),
                _ => return true,
            }
        };
        if let Some(c) = owner
            && self.jobs.inner.limiter.remaining_pages(&c) == Some(0)
        {
            self.quota_hit.store(true, Ordering::Relaxed);
            return true;
        }
        false
    }

    /// 记一个完成的页面；`done` 是恢复时用来认出它的键（URL 或批量里的下标）。
    async fn record_page(&self, done: String, doc: Document) {
        let inner = &self.jobs.inner;
        if !self.running().await {
            return;
        }
        let page_doc = self.hook.as_ref().map(|_| doc.clone());
        let mut keep = Some(doc);
        if let Some(store) = self.store() {
            let doc = keep.take().expect("set above");
            let (pages, done_set) = (list_key(&self.id, "pages"), list_key(&self.id, "done"));
            let r = store
                .blocking(move |s| -> pagesift_store::Result<()> {
                    let t = s.tasks();
                    t.list_push(pages, &serde_json::to_vec(&doc).unwrap_or_default())?;
                    t.set_insert(done_set, done)?;
                    Ok(())
                })
                .await;
            if let Err(e) = r {
                tracing::warn!(job = %self.id, error = %e, "saving a job page failed");
            }
        }
        let owner = {
            let mut map = inner.map.lock().await;
            let Some(job) = map.get_mut(&self.id) else {
                return;
            };
            if job.state == JobState::Cancelled {
                return;
            }
            job.completed += 1;
            job.total = job.total.max(job.completed);
            match keep {
                Some(doc) => job.data.push(doc),
                None => job.stored_docs += 1,
            }
            job.owner.clone()
        };
        inner.metrics.page(self.kind.pages());
        if let Some(c) = owner {
            inner.limiter.consume_pages(&c, 1).await;
        }
        if let (Some(hook), Some(doc)) = (&self.hook, page_doc) {
            hook.send(Event::Page, vec![doc], None);
        }
    }

    async fn record_error(&self, done: String, url: String, err: ApiError) {
        let inner = &self.jobs.inner;
        if !self.running().await {
            return;
        }
        inner.metrics.error(err.code.as_str(), "job");
        let e = job_error(url, &err);
        let mut keep = Some(e);
        if let Some(store) = self.store() {
            let e = keep.take().expect("set above");
            let (errors, done_set) = (list_key(&self.id, "errors"), list_key(&self.id, "done"));
            let r = store
                .blocking(move |s| -> pagesift_store::Result<()> {
                    let t = s.tasks();
                    t.list_push(errors, &serde_json::to_vec(&e).unwrap_or_default())?;
                    t.set_insert(done_set, done)?;
                    Ok(())
                })
                .await;
            if let Err(e) = r {
                tracing::warn!(job = %self.id, error = %e, "saving a job error failed");
            }
        }
        let mut map = inner.map.lock().await;
        let Some(job) = map.get_mut(&self.id) else {
            return;
        };
        if job.state == JobState::Cancelled {
            return;
        }
        job.completed += 1;
        job.total = job.total.max(job.completed);
        if let Some(e) = keep {
            if err.code == ErrorCode::RobotsDisallowed {
                job.robots_blocked.push(e.url.clone());
            }
            job.errors.push(e);
        }
    }

    /// 任务跑完：定下最终状态、落盘、发结束事件。
    async fn finish(&self, result: Result<(), (String, ApiError)>) {
        let inner = &self.jobs.inner;
        let quota = self.quota_hit.load(Ordering::Relaxed);
        let mut extra = Vec::new();
        if quota && let Some(c) = self.owner().await {
            extra.push(job_error(String::new(), &quota_error(&c)));
        }
        if let Err((url, e)) = &result {
            extra.push(job_error(url.clone(), e));
        }
        if let (Some(store), false) = (self.store(), extra.is_empty()) {
            let (key, items) = (list_key(&self.id, "errors"), extra.clone());
            store
                .blocking(move |s| {
                    for e in items {
                        let _ = s
                            .tasks()
                            .list_push(&key, &serde_json::to_vec(&e).unwrap_or_default());
                    }
                })
                .await;
        }
        let final_state = {
            let mut map = inner.map.lock().await;
            let Some(job) = map.get_mut(&self.id) else {
                return;
            };
            if job.state == JobState::Scraping {
                job.state = if result.is_ok() {
                    JobState::Completed
                } else {
                    JobState::Failed
                };
                if self.store().is_none() {
                    job.errors.extend(extra);
                }
            }
            (job.state, job.completed)
        };
        self.jobs.save_state(&self.id).await;
        let (state, completed) = final_state;
        tracing::info!(
            job = %self.id,
            kind = self.kind.as_str(),
            status = state.as_str(),
            pages = completed,
            quota_exceeded = quota,
            "job finished"
        );
        if let Some(hook) = &self.hook {
            match (state, &result) {
                (JobState::Failed, Err((_, e))) => {
                    hook.send(Event::Failed, Vec::new(), Some(e.to_string()))
                }
                (JobState::Completed, _) => hook.send(Event::Completed, Vec::new(), None),
                _ => {}
            }
        }
    }
}

impl CrawlSink for JobSink {
    fn page(&self, doc: Document) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let done = doc.metadata.source_url.clone().unwrap_or_default();
            self.record_page(done, doc).await
        })
    }

    fn error<'a>(&'a self, url: &'a str, err: ApiError) -> BoxFuture<'a, ()> {
        Box::pin(self.record_error(url.to_string(), url.to_string(), err))
    }

    fn total(&self, total: usize) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if let Some(job) = self.jobs.inner.map.lock().await.get_mut(&self.id) {
                job.total = total.max(job.completed);
            }
        })
    }

    fn cancelled(&self) -> BoxFuture<'_, bool> {
        Box::pin(self.stopped())
    }

    fn resume(&self) -> Option<CrawlResume> {
        self.resume.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    fn tracks_frontier(&self) -> bool {
        self.store().is_some()
    }

    fn enqueued(&self, urls: Vec<(String, usize)>) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let (Some(store), false) = (self.store(), urls.is_empty()) else {
                return;
            };
            let key = list_key(&self.id, "frontier");
            let r = store
                .blocking(move |s| -> pagesift_store::Result<()> {
                    let t = s.tasks();
                    for u in urls {
                        t.list_push(&key, &serde_json::to_vec(&u).unwrap_or_default())?;
                    }
                    Ok(())
                })
                .await;
            if let Err(e) = r {
                tracing::warn!(job = %self.id, error = %e, "saving the crawl queue failed");
            }
        })
    }
}

/// UTC 时间格式化成 RFC 3339（秒级精度后带毫秒），不引入日期库。
pub fn rfc3339(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant 的 civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        d.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc_dates() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        let t = UNIX_EPOCH + Duration::from_millis(1_792_540_800_123);
        assert_eq!(rfc3339(t), "2026-10-21T00:00:00.123Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400); // 2000-02-29
        assert_eq!(rfc3339(leap), "2000-02-29T00:00:00.000Z");
    }
}
