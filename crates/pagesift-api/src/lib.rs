//! pagesift 的 HTTP 接口：Firecrawl v2 兼容层、pagesift 扩展、健康检查和监控指标，
//! 以及部署成服务要用的任务持久化、webhook、按 API key 的限流和配额。
//!
//! 协议、校验和任务管理在这里；真正的抓取由实现 [`Engine`] 的引擎完成。

#![forbid(unsafe_code)]

mod engine;
mod error;
pub(crate) mod firecrawl;
pub mod jina;
mod jobs;
mod limits;
mod metrics;
pub(crate) mod server;
pub mod types;
mod validate;
mod webhook;

pub use engine::{BoxFuture, CrawlResume, CrawlSink, Engine};
pub use error::{ApiError, ErrorCode};
pub use jobs::{JOB_TTL, Job, JobKind, JobState, Jobs, rfc3339};
pub use limits::KeyLimits;
pub use server::{Config, Timeouts, router, serve, serve_with};
pub use validate::{
    MAX_CRAWL_DEPTH, MAX_CRAWL_LIMIT, MAX_MAP_LIMIT, MAX_PARSE_FILE_BYTES, MAX_SCRAPE_TIMEOUT_MS,
    MAX_URL_LEN, MAX_URLS, check_parse_options, check_scrape_options, check_url,
};
pub use webhook::{WebhookConfig, WebhookSettings};
