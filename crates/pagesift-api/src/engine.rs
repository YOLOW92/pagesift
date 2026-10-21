//! 抓取引擎接口。API 层只做协议、校验和任务管理，真正的抓取由实现这个 trait 的引擎完成。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::error::ApiError;
use crate::types::{
    CrawlRequest, CrawlSummary, Document, ExtractRequest, MapLink, MapRequest, ScrapeOptions,
    SearchRequest,
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
    /// 爬取结束时的说明（停止原因等）；只有带查询的爬取会调用。
    fn summary(&self, _summary: CrawlSummary) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}
