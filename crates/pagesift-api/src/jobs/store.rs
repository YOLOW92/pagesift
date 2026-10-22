use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pagesift_store::Store;
use serde::{Deserialize, Serialize};

use crate::engine::{CrawlResume, Engine};
use crate::limits::{Caller, KeyLimits};
use crate::types::{CrawlRequest, CrawlSummary, ScrapeOptions};
use crate::webhook::WebhookConfig;

use super::{JOB_TTL, Job, JobKind, JobState, Jobs};

/// 落盘的任务元数据。
#[derive(Serialize, Deserialize)]
pub(super) struct Meta {
    pub(super) kind: JobKind,
    pub(super) state: JobState,
    pub(super) created_ms: u64,
    pub(super) total: usize,
    pub(super) completed: usize,
    pub(super) owner: Option<String>,
    pub(super) webhook: Option<WebhookConfig>,
    pub(super) crawl: Option<CrawlRequest>,
    pub(super) batch: Option<BatchSpec>,
    /// 带查询的爬取结束后的说明；旧版本存的任务没有这个字段。
    #[serde(default)]
    pub(super) summary: Option<CrawlSummary>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct BatchSpec {
    pub(super) urls: Vec<String>,
    pub(super) options: ScrapeOptions,
    pub(super) concurrency: usize,
}

pub(super) fn meta_key(id: &str) -> String {
    format!("job/{id}")
}

pub(super) fn list_key(id: &str, what: &str) -> String {
    format!("job/{id}/{what}")
}

// Store identity with the result in ONE CRC-protected log record. The done
// set is a derived index: startup repairs it before selecting pending work.
// Public Document/JobError decoding deliberately omits this private field.
#[derive(Serialize)]
pub(super) struct Completion<'a, T> {
    #[serde(flatten)]
    pub(super) value: &'a T,
    #[serde(rename = "_pagesiftDone")]
    pub(super) done: &'a str,
}

#[derive(Deserialize)]
struct Receipt {
    #[serde(default, rename = "_pagesiftDone")]
    done: Option<String>,
}

fn repair_done(store: &Store, id: &str) -> pagesift_store::Result<()> {
    let t = store.tasks();
    let done = list_key(id, "done");
    for kind in ["pages", "errors"] {
        let key = list_key(id, kind);
        for start in (0..t.list_len(&key)).step_by(256) {
            for raw in t.list_range(&key, start, 256)? {
                if let Ok(Receipt { done: Some(member) }) = serde_json::from_slice(&raw) {
                    t.set_insert(&done, member)?;
                }
            }
        }
    }
    Ok(())
}

pub(super) fn to_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Jobs {
    /// 把任务的当前状态和计数写回存储。
    pub(super) async fn save_state(&self, id: &str) {
        let Some(store) = &self.inner.store else {
            return;
        };
        let snapshot = {
            let map = self.inner.map.lock().await;
            map.get(id)
                .map(|j| (j.state, j.total, j.completed, j.summary.clone()))
        };
        let Some((state, total, completed, summary)) = snapshot else {
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
                meta.summary = summary;
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
            job.summary = meta.summary;
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
            // Repair the append/done crash window before any worker resumes.
            if let Err(error) = repair_done(store, &id) {
                tracing::error!(job = %id, %error, "could not rebuild completion index; not resuming job");
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
pub(super) fn delete_job(store: &Store, id: &str) {
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
