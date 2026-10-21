//! 抓取引擎接口。API 层只做协议、校验和任务管理，真正的抓取由实现这个 trait 的引擎完成。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::error::ApiError;
use crate::types::{
    CrawlRequest, Document, ExtractRequest, MapLink, MapRequest, ScrapeOptions, SearchRequest,
};
use serde_json::Value;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait Engine: Send + Sync + 'static {
    fn scrape<'a>(
        &'a self,
        url: &'a str,
        opts: &'a ScrapeOptions,
    ) -> BoxFuture<'a, Result<Document, ApiError>>;

    fn map<'a>(&'a self, _req: &'a MapRequest) -> BoxFuture<'a, Result<Vec<MapLink>, ApiError>> {
        Box::pin(async { Err(ApiError::unsupported("map is not available in this build")) })
    }

    /// 整站爬取：抓到一页、出错一页、发现新 URL 时都通过 `sink` 回报，直到爬完或被取消。
    fn crawl<'a>(
        &'a self,
        _req: &'a CrawlRequest,
        _sink: Arc<dyn CrawlSink>,
    ) -> BoxFuture<'a, Result<(), ApiError>> {
        Box::pin(async {
            Err(ApiError::unsupported(
                "crawl is not available in this build",
            ))
        })
    }

    /// `/v2/search`：返回 Firecrawl 的 `data`（`{ "web": [...] }`）。
    fn search<'a>(&'a self, _req: &'a SearchRequest) -> BoxFuture<'a, Result<Value, ApiError>> {
        Box::pin(async {
            Err(ApiError::unsupported(
                "search is not configured: set [search] in pagesift.toml",
            ))
        })
    }

    /// `/v2/extract`：返回提取出的 JSON。
    fn extract<'a>(&'a self, _req: &'a ExtractRequest) -> BoxFuture<'a, Result<Value, ApiError>> {
        Box::pin(async {
            Err(ApiError::unsupported(
                "extract needs an LLM: set [llm] in pagesift.toml",
            ))
        })
    }

    /// 引擎支持的 `formats`；请求了不支持的格式时返回 UNSUPPORTED_OPTION，不悄悄忽略。
    fn supported_formats(&self) -> &'static [&'static str] {
        &["markdown", "html", "rawHtml", "links", "images"]
    }

    /// 是否支持 `actions`（需要浏览器渲染）。
    fn supports_actions(&self) -> bool {
        false
    }
}

/// 整站爬取的进度回报，由 API 层的任务管理实现。
pub trait CrawlSink: Send + Sync {
    fn page(&self, doc: Document) -> BoxFuture<'_, ()>;
    fn error<'a>(&'a self, url: &'a str, err: ApiError) -> BoxFuture<'a, ()>;
    /// 已经确定要抓的页面总数（随着发现新链接增长）。
    fn total(&self, total: usize) -> BoxFuture<'_, ()>;
    fn cancelled(&self) -> BoxFuture<'_, bool>;

    /// 断点续爬：任务是从中断处恢复的时候返回上次的进度，引擎据此重建队列和去重表，
    /// 不再抓已经完成的页面。新任务返回 `None`。
    fn resume(&self) -> Option<CrawlResume> {
        None
    }

    /// 是否需要 [`CrawlSink::enqueued`] 回报（持久化的任务需要）。不需要时引擎不必收集。
    fn tracks_frontier(&self) -> bool {
        false
    }

    /// 新进入队列的 URL 和发现深度，按入队顺序。只在 [`CrawlSink::tracks_frontier`] 为真时调用。
    fn enqueued(&self, _urls: Vec<(String, usize)>) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// 中断的爬取任务恢复时的进度。完成的页面以文档的 `metadata.sourceURL`（失败的以出错的 URL）为准。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CrawlResume {
    /// 入过队的全部 URL，按入队顺序（去重表）。
    pub enqueued: Vec<String>,
    /// 入了队但还没完成的 URL 和发现深度，按入队顺序。中断时正在抓的页面也在这里。
    pub pending: Vec<(String, usize)>,
    /// 已经完成的页数（成功和失败都算），计入 `limit`。
    pub done: usize,
}
