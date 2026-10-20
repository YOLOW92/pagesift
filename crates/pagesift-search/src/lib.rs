//! 可插拔的搜索后端：SearXNG、Brave、Bing、Google CSE、Tavily。
//!
//! 各后端的请求和响应格式见各模块顶部注释里的官方文档链接。
//! key 和端点全部由调用方配置，库内不含任何默认 key。

#![forbid(unsafe_code)]

mod backend;
mod bing;
mod brave;
mod error;
mod google;
mod http;
mod searxng;
mod tavily;

pub use backend::{SearchBackend, SearchQuery, SearchResult, TimeRange};
pub use bing::BingBackend;
pub use brave::BraveBackend;
pub use error::{Result, SearchError};
pub use google::GoogleCseBackend;
pub use http::HttpOptions;
pub use searxng::SearxngBackend;
pub use tavily::TavilyBackend;
