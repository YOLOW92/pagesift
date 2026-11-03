//! 多服务器部署的接线：把 `pagesift-cluster` 的协调器接到 HTTP 服务和 [`crate::Scraper`] 上。
//!
//! 单主机（`role = "standalone"`，默认）完全不经过这里：页面照旧在进程内直接抓，不走帧协议、不序列化。

pub use pagesift_engine::PageRunner;

mod gateway;
pub mod net;
mod replica;
mod sites;
pub mod standby;
mod stats;
mod task;
pub mod worker;

pub use gateway::{ClusterEngine, Dispatch, serve_workers};
pub use net::{ClientTls, Link};
pub use replica::replicate_jobs;
pub use standby::{StandbyOptions, follow_until_lost};
pub use stats::{PLANNER, register_stats};
pub use worker::WorkerOptions;
