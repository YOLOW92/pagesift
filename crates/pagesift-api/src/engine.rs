//! 抓取引擎接口。API 层只做协议、校验和任务管理，真正的抓取由实现这个 trait 的引擎完成。

use std::future::Future;
use std::pin::Pin;

use crate::error::ApiError;
use crate::types::{Document, MapLink, MapRequest, ScrapeOptions};

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

    /// 引擎支持的 `formats`；请求了不支持的格式时返回 UNSUPPORTED_OPTION，不悄悄忽略。
    fn supported_formats(&self) -> &'static [&'static str] {
        &["markdown", "html", "rawHtml", "links", "images"]
    }

    /// 是否支持 `actions`（需要浏览器渲染）。
    fn supports_actions(&self) -> bool {
        false
    }
}
