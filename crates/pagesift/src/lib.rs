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

pub mod config;
mod crawl;
mod engine;
pub mod mcp;

pub use crawl::normalize as normalize_url;
pub use engine::{AUTO_RENDER_MIN_CHARS, Scraper, ScraperConfig, ScraperError};
pub use pagesift_api as api;
