//! pagesift：把网页变成给大模型用的干净 Markdown。
//!
//! 这个 crate 既是库也是命令行工具。库的入口是 [`Scraper`]：
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let scraper = pagesift::Scraper::new(pagesift::ScraperConfig::default())?;
//! let doc = scraper.scrape("https://example.com", &Default::default()).await?;
//! println!("{}", doc.markdown.unwrap_or_default());
//! # Ok(()) }
//! ```

#![forbid(unsafe_code)]

pub mod cluster;
pub mod collect;
pub mod config;
pub mod mcp;
mod mcp_graph;
pub mod models;
pub mod session;

pub use pagesift_api as api;
pub use pagesift_engine::{
    AUTO_RENDER_ARTICLE_MIN_CHARS, AUTO_RENDER_MIN_CHARS, EngineProbe, InteractLimits, PageRunner,
    Scraper, ScraperBuilder, ScraperConfig, ScraperError, audit, needs_render, normalize_url,
    notice, office_markdown, office_markdown_from, scripts, set_cpu_capacity, set_cpu_groups,
    text_weight,
};
#[doc(hidden)]
pub use pagesift_engine::{change, crawl, discovery, engine, focus, frontier, learn};
pub use pagesift_hooks as hooks;
pub use pagesift_media::{download, media, postprocess, stream};
pub use pagesift_store as store;
