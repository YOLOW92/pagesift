use std::collections::{HashMap, HashSet};
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use pagesift_store::Store;

use crate::engine::{BoxFuture, CrawlResume, CrawlSink};
use crate::error::{ApiError, ErrorCode};
use crate::limits::{Caller, PageHold, quota_error};
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
    /// 停止摘要要不要存进任务状态（API 的 `pagesift` 字段）。引擎对所有爬取都回调摘要，
    /// 但 API 只在带查询的爬取里给这个字段，响应的形状和以前一样。
    pub(super) keep_summary: bool,
    /// 持久化（或复制到温备）失败的第一个原因。设上之后任务停止、以 `failed` 结束，
    /// 失败的那一页不算完成、不发 `page` 事件（0.90 只记警告，照样计为完成，重启后这一页就没了）。
    pub(super) store_failed: StdMutex<Option<String>>,
    /// Serialized per-job asset publication, initialized from persisted pages
    /// when resuming so concurrent pages never carry the same bytes twice.
    pub(super) asset_hashes: tokio::sync::Mutex<Option<HashSet<String>>>,
    /// 在途页面的配额预留，按抓取 URL 索引；重试同一 URL 不重复预留。
    pub(super) holds: tokio::sync::Mutex<HashMap<String, PageHold>>,
}

impl JobSink {
    fn store(&self) -> Option<&Store> {
        self.jobs.inner.store.as_ref()
    }

    fn failed_store(&self) -> Option<String> {
        self.store_failed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 记下持久化失败（只留第一个原因）。
    fn persist_failed(&self, what: &str, e: &pagesift_store::Error) {
        tracing::error!(job = %self.id, error = %e, "saving {what} failed; stopping the job");
        let mut f = self.store_failed.lock().unwrap_or_else(|e| e.into_inner());
        f.get_or_insert_with(|| format!("saving {what} failed: {e}"));
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

    /// 任务被取消、调用方的配额用完了，或者结果存不进去了。
    pub(super) async fn stopped(&self) -> bool {
        if self.failed_store().is_some() {
            return true;
        }
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
    pub(super) async fn record_page(&self, done: String, mut doc: Document) {
        let inner = &self.jobs.inner;
        if !self.running().await || self.failed_store().is_some() {
            return;
        }
        let needs_assets = doc
            .attachments
            .iter()
            .any(|a| a.inline_data && a.data.is_some());
        let mut guard = if needs_assets {
            Some(self.asset_hashes.lock().await)
        } else {
            None
        };
        // Another page can fail persistence or cancel the job while this
        // page waits for the publication lock.
        if guard.is_some() && (!self.running().await || self.failed_store().is_some()) {
            return;
        }
        if let Some(state) = guard.as_mut() {
            if state.is_none() {
                let hashes = if let Some(store) = self.store() {
                    let key = list_key(&self.id, "pages");
                    match store
                        .blocking(move |s| -> pagesift_store::Result<HashSet<String>> {
                            let tasks = s.tasks();
                            let mut hashes = HashSet::new();
                            for start in (0..tasks.list_len(&key)).step_by(256) {
                                for raw in tasks.list_range(&key, start, 256)? {
                                    let value: serde_json::Value = serde_json::from_slice(&raw)
                                        .map_err(std::io::Error::other)?;
                                    if let Some(attachments) =
                                        value.get("attachments").and_then(|v| v.as_array())
                                    {
                                        for a in attachments {
                                            if (a.get("data").is_some()
                                                || a.get("inlineData").and_then(|v| v.as_bool())
                                                    == Some(true))
                                                && let Some(hash) =
                                                    a.get("hash").and_then(|v| v.as_str())
                                            {
                                                hashes.insert(hash.to_owned());
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(hashes)
                        })
                        .await
                    {
                        Ok(hashes) => hashes,
                        Err(e) => {
                            self.persist_failed("asset index", &e);
                            return;
                        }
                    }
                } else {
                    let jobs = inner.map.lock().await;
                    jobs.get(&self.id)
                        .map(|job| {
                            job.data
                                .iter()
                                .flat_map(|d| &d.attachments)
                                .filter(|a| a.inline_data && a.data.is_some())
                                .map(|a| a.hash.clone())
                                .collect()
                        })
                        .unwrap_or_default()
                };
                **state = Some(hashes);
            }
            let seen = state.as_mut().expect("initialized above");
            let mut local = HashSet::new();
            for attachment in &mut doc.attachments {
                if attachment.inline_data
                    && attachment.data.is_some()
                    && (seen.contains(&attachment.hash) || !local.insert(attachment.hash.clone()))
                {
                    attachment.data = None;
                    attachment.inline_data = false;
                }
            }
        }
        let published: Vec<String> = doc
            .attachments
            .iter()
            .filter(|a| a.inline_data && a.data.is_some())
            .map(|a| a.hash.clone())
            .collect();
        let owner_caller = self.owner().await;
        let owner_account = owner_caller
            .as_ref()
            .map(|c| c.account.to_string())
            .unwrap_or_else(|| "anonymous".to_string());
        crate::attachments::keep_job_in_for(&doc, self.jobs.disk().as_ref(), &owner_account);
        let page_doc = self.hook.as_ref().map(|_| doc.clone());
        let mut keep = Some(doc);
        if let Some(store) = self.store() {
            let mut doc = keep.take().expect("set above");
            let assets: Vec<_> = doc
                .attachments
                .iter_mut()
                .filter_map(|a| {
                    if a.inline_data {
                        a.data.take().map(|data| (a.id.clone(), data))
                    } else {
                        None
                    }
                })
                .collect();
            let attachment_ids: Vec<String> =
                doc.attachments.iter().map(|a| a.id.clone()).collect();
            let (pages, done_set) = (list_key(&self.id, "pages"), list_key(&self.id, "done"));
            let m = inner.mirror.clone();
            let owner_for_store = owner_account.clone();
            let r = store
                .blocking(move |s| -> pagesift_store::Result<()> {
                    let value = serde_json::to_vec(&Completion {
                        value: &doc,
                        done: &done,
                    })
                    .map_err(std::io::Error::other)?;
                    super::mirror::write(s, m.as_deref(), |t| {
                        for (id, data) in &assets {
                            crate::attachments::persist_job_asset(t, id, data)?;
                        }
                        for id in &attachment_ids {
                            crate::attachments::persist_access(t, &owner_for_store, id)?;
                        }
                        t.list_push(&pages, &value)?;
                        t.set_insert(&done_set, done.as_bytes())
                    })
                })
                .await;
            if let Err(e) = r {
                self.persist_failed("a job page", &e);
                // 本地已经存下（只是没复制到温备）的照常算完成，轮询和重启后看到的一致
                if !super::mirror::unreplicated(&e) {
                    return;
                }
            }
        }
        {
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
            job.touch();
        }
        if let Some(state) = guard.as_mut() {
            state.as_mut().expect("initialized above").extend(published);
        }
        inner.metrics.page(self.kind.pages());
        if let (Some(hook), Some(doc)) = (&self.hook, page_doc) {
            hook.send(Event::Page, vec![doc], None).await;
        }
    }

    pub(super) async fn record_error(&self, done: String, url: String, err: ApiError) {
        let inner = &self.jobs.inner;
        if !self.running().await || self.failed_store().is_some() {
            return;
        }
        inner.metrics.error(err.code.as_str(), "job");
        let e = job_error(url, &err);
        let mut keep = Some(e);
        if let Some(store) = self.store() {
            let e = keep.take().expect("set above");
            let (errors, done_set) = (list_key(&self.id, "errors"), list_key(&self.id, "done"));
            let m = inner.mirror.clone();
            let r = store
                .blocking(move |s| -> pagesift_store::Result<()> {
                    let value = serde_json::to_vec(&Completion {
                        value: &e,
                        done: &done,
                    })
                    .map_err(std::io::Error::other)?;
                    super::mirror::write(s, m.as_deref(), |t| {
                        t.list_push(&errors, &value)?;
                        t.set_insert(&done_set, done.as_bytes())
                    })
                })
                .await;
            if let Err(e) = r {
                self.persist_failed("a job error", &e);
                if !super::mirror::unreplicated(&e) {
                    return;
                }
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
        let result = match (result, self.failed_store()) {
            (Ok(()), Some(why)) => Err((String::new(), ApiError::new(ErrorCode::Internal, why))),
            (r, _) => r,
        };
        let inner = &self.jobs.inner;
        let quota = self.quota_hit.load(Ordering::Relaxed);
        let mut extra = Vec::new();
        if quota && let Some(c) = self.owner().await {
            extra.push(job_error(String::new(), &quota_error(&c)));
        }
        if let Err((url, e)) = &result {
            extra.push(job_error(url.clone(), e));
        }
        // 任务级的错误（配额、失败原因）进单独的列表：0.90 放在页面错误里，重启后被算进完成数；
        // 也经过镜像复制（0.90 直接写本地存储，温备上没有）
        let mut kept = self.store().is_none();
        if let (Some(store), false) = (self.store(), extra.is_empty()) {
            let (key, items) = (list_key(&self.id, "notes"), extra.clone());
            let m = inner.mirror.clone();
            let r = store
                .blocking(move |s| {
                    super::mirror::write(s, m.as_deref(), |t| {
                        for e in items {
                            t.list_push(&key, &serde_json::to_vec(&e).unwrap_or_default())?;
                        }
                        Ok(())
                    })
                })
                .await;
            if let Err(e) = r {
                // 存不进去（多半就是让任务失败的那个存储错误）：留在内存里，至少进程还在时查得到原因
                tracing::error!(job = %self.id, error = %e, "saving the job's final errors failed");
                kept = true;
            }
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
                if kept {
                    job.errors.extend(extra);
                }
                job.touch();
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
                        .await
                }
                (JobState::Completed, _) => hook.send(Event::Completed, Vec::new(), None).await,
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

    fn reserve_page<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<(), ApiError>> {
        Box::pin(async move {
            let mut holds = self.holds.lock().await;
            if holds.contains_key(url) {
                return Ok(());
            }
            let Some(caller) = self.owner().await else {
                return Ok(());
            };
            let limiter = &self.jobs.inner.limiter;
            match limiter.reserve_page(&caller).await {
                Ok(hold) => {
                    holds.insert(url.to_string(), hold);
                    Ok(())
                }
                Err(e) => {
                    self.quota_hit.store(true, Ordering::Relaxed);
                    Err(e)
                }
            }
        })
    }

    fn settle_page<'a>(&'a self, url: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let hold = { self.holds.lock().await.remove(url) };
            if let Some(hold) = hold {
                hold.commit().await;
            }
        })
    }

    fn release_page<'a>(&'a self, url: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            drop(self.holds.lock().await.remove(url));
        })
    }

    fn release_pending(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.holds.lock().await.clear();
        })
    }

    fn resume(&self) -> Option<CrawlResume> {
        self.resume.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    fn tracks_frontier(&self) -> bool {
        self.store().is_some()
    }

    fn summary(&self, summary: CrawlSummary) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if !self.keep_summary {
                return;
            }
            if let Some(job) = self.jobs.inner.map.lock().await.get_mut(&self.id) {
                job.summary = Some(summary);
            }
            // 结束时 `finish` 还会再存一次；这里先存，任务结束前进程退出也不丢
            self.jobs.save_state(&self.id).await;
        })
    }

    fn graph_delta(&self, delta: pagesift_graph::PageGraphDelta) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            super::graph::record_delta(&self.jobs, &self.id, delta).await;
        })
    }

    fn graph_summary(&self, summary: pagesift_graph::GraphSummary) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            super::graph::record_summary(&self.jobs, &self.id, summary).await;
        })
    }

    fn enqueued(&self, urls: Vec<(String, usize)>) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let (Some(store), false) = (self.store(), urls.is_empty()) else {
                return;
            };
            let key = list_key(&self.id, "frontier");
            let m = self.jobs.inner.mirror.clone();
            let r = store
                .blocking(move |s| -> pagesift_store::Result<()> {
                    super::mirror::write(s, m.as_deref(), |t| {
                        for u in urls {
                            t.list_push(&key, &serde_json::to_vec(&u).unwrap_or_default())?;
                        }
                        Ok(())
                    })
                })
                .await;
            if let Err(e) = r {
                // 恢复时的队列不完整了：和页面存不进去一样处理
                self.persist_failed("the crawl queue", &e);
            }
        })
    }
}
