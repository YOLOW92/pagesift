//! Job state definitions and lifecycle structures.

use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};
use tokio::sync::watch;

use crate::limits::Caller;
use crate::metrics::PageSource;
use crate::types::{CrawlSummary, Document, JobError};
use crate::webhook::WebhookConfig;

/// Task results retention period (24 hours).
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
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Crawl => "crawl",
            JobKind::BatchScrape => "batch_scrape",
        }
    }

    pub(crate) fn pages(self) -> PageSource {
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
    pub data: Vec<Document>,
    pub errors: Vec<JobError>,
    pub robots_blocked: Vec<String>,
    pub created: SystemTime,
    pub summary: Option<CrawlSummary>,
    pub(crate) stored_docs: usize,
    pub(crate) owner: Option<Caller>,
    /// 新完成了文档或者状态变了就响一下，叫醒在等这个任务的流（`/stream`）。任务删掉时随之关闭。
    pub(crate) changed: watch::Sender<()>,
    pub(crate) previews: tokio::sync::broadcast::Sender<crate::types::MarkdownPreview>,
}

impl Job {
    pub(crate) fn new(kind: JobKind, total: usize, owner: Option<Caller>) -> Self {
        Self {
            kind,
            state: JobState::Scraping,
            total,
            completed: 0,
            data: Vec::new(),
            errors: Vec::new(),
            robots_blocked: Vec::new(),
            created: SystemTime::now(),
            summary: None,
            stored_docs: 0,
            owner,
            changed: watch::channel(()).0,
            previews: tokio::sync::broadcast::channel(4).0,
        }
    }

    /// 通知在等这个任务的流：有新文档或状态变了。没有人在等也没关系。
    pub(crate) fn touch(&self) {
        self.changed.send_replace(());
    }

    pub fn expires_at(&self) -> SystemTime {
        self.created + JOB_TTL
    }

    pub(crate) fn docs(&self) -> usize {
        self.data.len() + self.stored_docs
    }
}

pub struct StatusPage {
    pub state: JobState,
    pub total: usize,
    pub completed: usize,
    pub expires_at: SystemTime,
    pub docs: usize,
    pub data: Vec<Document>,
    pub summary: Option<CrawlSummary>,
}

#[derive(Default)]
pub struct Start {
    pub owner: Option<Caller>,
    pub webhook: Option<WebhookConfig>,
}
