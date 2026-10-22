use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use pagesift_store::Store;

use crate::engine::{BoxFuture, CrawlResume, CrawlSink};
use crate::error::{ApiError, ErrorCode};
use crate::limits::{Caller, quota_error};
use crate::types::{CrawlSummary, Document, JobError};
use crate::webhook::{Event, Hook};

use super::store::{Completion, list_key};
use super::{JobKind, JobState, Jobs, rfc3339};

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
pub(super) struct JobSink {
    pub(super) jobs: Jobs,
    pub(super) id: String,
    pub(super) kind: JobKind,
    pub(super) hook: Option<Hook>,
    pub(super) resume: StdMutex<Option<CrawlResume>>,
    /// 调用方当天的页面配额用完了，任务提前结束。
    pub(super) quota_hit: AtomicBool,
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
    pub(super) async fn stopped(&self) -> bool {
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
    pub(super) async fn record_page(&self, done: String, doc: Document) {
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
                    let value = serde_json::to_vec(&Completion {
                        value: &doc,
                        done: &done,
                    })
                    .map_err(std::io::Error::other)?;
                    t.list_push(pages, &value)?;
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

    pub(super) async fn record_error(&self, done: String, url: String, err: ApiError) {
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
                    let value = serde_json::to_vec(&Completion {
                        value: &e,
                        done: &done,
                    })
                    .map_err(std::io::Error::other)?;
                    t.list_push(errors, &value)?;
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
    pub(super) async fn finish(&self, result: Result<(), (String, ApiError)>) {
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

    fn summary(&self, summary: CrawlSummary) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if let Some(job) = self.jobs.inner.map.lock().await.get_mut(&self.id) {
                job.summary = Some(summary);
            }
            // 结束时 `finish` 还会再存一次；这里先存，任务结束前进程退出也不丢
            self.jobs.save_state(&self.id).await;
        })
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
