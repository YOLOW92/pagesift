//! pagesift 的无 LLM 提取工具：按 CSS/XPath schema 抽取 JSON、正则实体提取、
//! Markdown 分块和 BM25 相关性过滤。schema 格式兼容 Crawl4AI 的
//! `JsonCssExtractionStrategy` / `JsonXPathExtractionStrategy`。
//!
//! ```
//! use pagesift_extract::{JsonCssExtractor, Schema};
//!
//! let schema: Schema = r#"{
//!     "baseSelector": "li.item",
//!     "fields": [
//!         {"name": "title", "selector": "a", "type": "text"},
//!         {"name": "url", "selector": "a", "type": "attribute", "attribute": "href"}
//!     ]
//! }"#.parse().unwrap();
//! let html = r#"<ul><li class="item"><a href="/a">First</a></li><li class="item"><a href="/b">Second</a></li></ul>"#;
//! let items = JsonCssExtractor::new(&schema).unwrap().extract(html);
//! assert_eq!(items[1]["title"], "Second");
//! assert_eq!(items[1]["url"], "/b");
//! ```

#![forbid(unsafe_code)]

mod bm25;
mod chunk;
mod css;
mod engine;
mod error;
mod patterns;
mod recursive;
mod relevance;
mod schema;
mod text;
mod xpath;

pub use bm25::{Bm25Options, ScoredBlock, bm25_filter, bm25_scores, filter_markdown, tokenize};
pub use chunk::{Chunk, ChunkStrategy, chunk};
pub use css::JsonCssExtractor;
pub use error::Error;
pub use patterns::{BuiltinPattern, RegexExtractor, RegexMatch, RegexOutput, UniqueMatch};
pub use relevance::{
    FilteredMarkdown, MAX_QUERY_TERMS, Query, QueryFilterOptions, filter_markdown_by_query,
};
pub use schema::{Field, FieldKind, Schema, Step, Transform};
pub use xpath::JsonXPathExtractor;
