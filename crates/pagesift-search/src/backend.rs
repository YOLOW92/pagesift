use std::future::Future;
use std::pin::Pin;

use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeRange {
    Day,
    Week,
    Month,
    Year,
}

#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub query: String,
    pub limit: usize,
    /// 语言提示，如 "zh"、"zh-CN"、"en"。
    pub lang: Option<String>,
    /// ISO 3166-1 两位国家码，如 "CN"。
    pub country: Option<String>,
    pub time_range: Option<TimeRange>,
}

impl SearchQuery {
    pub fn new(query: impl Into<String>, limit: usize) -> Self {
        Self {
            query: query.into(),
            limit,
            lang: None,
            country: None,
            time_range: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub url: String,
    pub title: String,
    pub description: String,
    /// 从 1 开始的名次。
    pub position: usize,
}

/// 搜索后端。`lang` 和 `country` 是尽力而为的提示，后端不支持时忽略；
/// 返回条数不超过 `limit`，也可能因后端上限而更少。
pub trait SearchBackend: Send + Sync {
    fn name(&self) -> &'static str;

    fn search<'a>(
        &'a self,
        query: &'a SearchQuery,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SearchResult>>> + Send + 'a>>;
}

/// 把后端返回的条目转成带名次的统一结果，丢弃没有 URL 的条目。
pub(crate) fn finish(
    items: impl IntoIterator<Item = (String, String, String)>,
    limit: usize,
) -> Vec<SearchResult> {
    items
        .into_iter()
        .filter(|(url, _, _)| !url.is_empty())
        .take(limit)
        .enumerate()
        .map(|(i, (url, title, description))| SearchResult {
            url,
            title,
            description,
            position: i + 1,
        })
        .collect()
}
