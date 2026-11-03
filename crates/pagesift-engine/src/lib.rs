//! pagesift 的引擎：单页抓取的完整流程（HTTP、渲染、渲染决策、PDF/Office、结果组装）和爬取循环。
//! 命令行、HTTP 服务、多服务器接线在上层的 `pagesift` crate；这里不依赖它们。

#![forbid(unsafe_code)]

pub mod change;
pub mod crawl;
pub mod discovery;
pub mod engine;
pub mod notice;
pub mod scripts;

pub use pagesift_frontier::{focus, frontier};
pub use pagesift_plan::learn;
pub use pagesift_sink::audit;

use pagesift_api::types::{Document, ScrapeOptions};
use pagesift_api::{ApiError, BoxFuture};
use pagesift_policy::RobotsMode;

/// 爬取里逐页抓取的执行者。网关把它装到 `Scraper` 上（[`engine::Scraper::route_pages`]），
/// 爬取前沿留在网关，页面交给工人。
pub trait PageRunner: Send + Sync {
    fn run<'a>(
        &'a self,
        kind: &'static str,
        url: &'a str,
        opts: &'a ScrapeOptions,
        mode: RobotsMode,
    ) -> BoxFuture<'a, Result<Document, ApiError>>;

    /// 执行方的准入容量（网关：在线工人报告的准入值之和）；`None` 时用本机的准入。
    fn capacity(&self) -> Option<usize> {
        None
    }
}

pub use crawl::normalize as normalize_url;
pub use engine::{
    AUTO_RENDER_ARTICLE_MIN_CHARS, AUTO_RENDER_MIN_CHARS, EngineProbe, InteractLimits, Scraper,
    ScraperBuilder, ScraperConfig, ScraperError, needs_render, office_markdown,
    office_markdown_from, set_cpu_capacity, set_cpu_groups, text_weight,
};
