//! pagesift 的浏览器渲染：自研的 CDP 客户端、有上限的浏览器池和子请求拦截。
//!
//! ```no_run
//! # async fn demo() -> pagesift_cdp::Result<()> {
//! use std::sync::Arc;
//! use pagesift_cdp::{BrowserPool, PoolConfig, PrivateNetworkGuard, RenderOptions};
//!
//! let pool = BrowserPool::new(PoolConfig::default(), Arc::new(PrivateNetworkGuard)).await?;
//! let page = pool.render("https://example.com/", &RenderOptions::default()).await?;
//! println!("{} {:?} {} bytes", page.final_url, page.status, page.html.len());
//! pool.shutdown().await;
//! # Ok(()) }
//! ```

#![forbid(unsafe_code)]

mod capture;
mod conn;
mod endpoint;
mod engine;
mod error;
mod guard;
#[cfg(unix)]
mod launch;
#[cfg(not(unix))]
#[path = "launch_unavailable.rs"]
mod launch;
mod options;
mod page;
mod pool;

pub use conn::{Connection, Event, EventStream, Session};
pub use engine::{EngineKind, Footprint, detect_engines, engine_chain};
pub use error::{Error, Result};
pub use guard::{
    BlockResources, BoxFuture, PrivateNetworkGuard, RequestGuard, SubRequest, Verdict,
    check_public_url, is_public_ip,
};
pub use launch::detect_browser;
pub use options::*;
pub use pool::{
    BrowserPool, BrowserSource, BrowserStats, PoolConfig, auto_browsers, auto_browsers_for,
};
