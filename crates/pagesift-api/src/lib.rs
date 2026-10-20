//! pagesift 的 HTTP 接口：Firecrawl v2 兼容层、pagesift 扩展、健康检查和监控指标。
//!
//! 协议、校验和任务管理在这里；真正的抓取由实现 [`Engine`] 的引擎完成。

#![forbid(unsafe_code)]

mod engine;
mod error;
mod jobs;
mod server;
pub mod types;
mod validate;

pub use engine::{BoxFuture, Engine};
pub use error::{ApiError, ErrorCode};
pub use server::{Config, router};
