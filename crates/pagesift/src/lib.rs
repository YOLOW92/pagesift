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

pub mod adaptive;
pub mod audit;
mod change;
pub mod collect;
pub mod config;
mod crawl;
mod discovery;
pub mod download;
mod engine;
mod focus;
mod frontier;
mod learn;
pub mod mcp;
pub mod media;
pub mod notice;
pub mod postprocess;
pub mod scripts;
pub mod session;
pub mod stream;

pub use crawl::normalize as normalize_url;
pub use engine::{
    AUTO_RENDER_ARTICLE_MIN_CHARS, AUTO_RENDER_MIN_CHARS, EngineProbe, Scraper, ScraperConfig,
    ScraperError, needs_render, office_markdown, office_markdown_from, set_cpu_capacity,
    text_weight,
};
pub use pagesift_api as api;
