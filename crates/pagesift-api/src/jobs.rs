//! 异步任务（批量抓取；整站爬取以后复用）：内存里保存进度和结果，过期后清理。
//! 持久化（F-6.4）由存储层接入后补上。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::{Mutex, Semaphore};

use crate::engine::Engine;
use crate::types::{Document, JobError, ScrapeOptions};

/// Firecrawl 的任务结果保留 24 小时。
pub const JOB_TTL: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Scraping,
    Completed,
    Cancelled,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Scraping => "scraping",
            JobState::Completed => "completed",
            JobState::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug)]
pub struct Job {
    pub state: JobState,
    pub total: usize,
    pub completed: usize,
    /// 按完成顺序追加，分页用下标。
    pub data: Vec<Document>,
    pub errors: Vec<JobError>,
    pub robots_blocked: Vec<String>,
    pub created: SystemTime,
}

impl Job {
    pub fn expires_at(&self) -> SystemTime {
        self.created + JOB_TTL
    }
}

#[derive(Clone, Default)]
pub struct Jobs {
    inner: Arc<Mutex<HashMap<String, Job>>>,
}

impl Jobs {
    pub async fn get<R>(&self, id: &str, f: impl FnOnce(&Job) -> R) -> Option<R> {
        let mut map = self.inner.lock().await;
        let now = SystemTime::now();
        map.retain(|_, j| j.expires_at() > now);
        map.get(id).map(f)
    }

    pub async fn cancel(&self, id: &str) -> bool {
        let mut map = self.inner.lock().await;
        match map.get_mut(id) {
            Some(j) => {
                if j.state == JobState::Scraping {
                    j.state = JobState::Cancelled;
                }
                true
            }
            None => false,
        }
    }

    pub async fn active(&self) -> Vec<String> {
        let map = self.inner.lock().await;
        map.iter()
            .filter(|(_, j)| j.state == JobState::Scraping)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// 启动批量抓取，立即返回任务 id；每个 URL 的结果和错误陆续写入任务。
    pub async fn start_batch(
        &self,
        engine: Arc<dyn Engine>,
        urls: Vec<String>,
        opts: ScrapeOptions,
        concurrency: usize,
    ) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.inner.lock().await.insert(
            id.clone(),
            Job {
                state: JobState::Scraping,
                total: urls.len(),
                completed: 0,
                data: Vec::new(),
                errors: Vec::new(),
                robots_blocked: Vec::new(),
                created: SystemTime::now(),
            },
        );
        let jobs = self.clone();
        let job_id = id.clone();
        let opts = Arc::new(opts);
        tokio::spawn(async move {
            let sem = Arc::new(Semaphore::new(concurrency.max(1)));
            let mut tasks = tokio::task::JoinSet::new();
            for url in urls {
                let (jobs, job_id, engine, opts, sem) = (
                    jobs.clone(),
                    job_id.clone(),
                    engine.clone(),
                    opts.clone(),
                    sem.clone(),
                );
                tasks.spawn(async move {
                    let _permit = sem.acquire_owned().await.expect("semaphore open");
                    if jobs.is_cancelled(&job_id).await {
                        return;
                    }
                    let result = engine.scrape(&url, &opts).await;
                    let mut map = jobs.inner.lock().await;
                    let Some(job) = map.get_mut(&job_id) else {
                        return;
                    };
                    if job.state == JobState::Cancelled {
                        return;
                    }
                    job.completed += 1;
                    match result {
                        Ok(doc) => job.data.push(doc),
                        Err(e) => {
                            if e.code == crate::error::ErrorCode::RobotsDisallowed {
                                job.robots_blocked.push(url.clone());
                            }
                            job.errors.push(JobError {
                                id: uuid::Uuid::new_v4().to_string(),
                                timestamp: rfc3339(SystemTime::now()),
                                url,
                                code: e.code.as_str().to_string(),
                                error: e.message,
                            });
                        }
                    }
                });
            }
            while tasks.join_next().await.is_some() {}
            if let Some(job) = jobs.inner.lock().await.get_mut(&job_id)
                && job.state == JobState::Scraping
            {
                job.state = JobState::Completed;
            }
        });
        id
    }

    async fn is_cancelled(&self, id: &str) -> bool {
        self.inner
            .lock()
            .await
            .get(id)
            .is_none_or(|j| j.state == JobState::Cancelled)
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
