//! Bing Web Search API v7
//! 文档: https://learn.microsoft.com/en-us/bing/search-apis/bing-web-search/reference/query-parameters
//! （该页已被微软标记为 retired/archived，更新于 2025-08；Bing Search API 已于 2025-08-11 退役，
//! 新 key 无法再申请。保留此后端是为了兼容仍持有旧资源的用户和兼容 Bing 协议的网关，
//! endpoint 可配置。）
//!
//! `GET https://api.bing.microsoft.com/v7.0/search`，鉴权头 `Ocp-Apim-Subscription-Key`。
//! 参数: q、count（最大 50）、mkt、cc（与 mkt 互斥）、setLang、
//! freshness（Day / Week / Month）。响应: `webPages.value[].name/url/snippet`。
//! 响应字段名取自 Bing 的 response-objects 文档，本次未能重新抓取核实。

use std::future::Future;
use std::pin::Pin;

use crate::backend::{SearchBackend, SearchQuery, SearchResult, TimeRange, finish};
use crate::error::{Result, SearchError};
use crate::http::{HttpOptions, build_client, require_nonempty, s, send_json};

const DEFAULT_ENDPOINT: &str = "https://api.bing.microsoft.com/v7.0/search";
const MAX_COUNT: usize = 50;

pub struct BingBackend {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl BingBackend {
    pub fn new(api_key: impl Into<String>, opts: HttpOptions) -> Result<Self> {
        Self::with_endpoint(DEFAULT_ENDPOINT, api_key, opts)
    }

    pub fn with_endpoint(
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
        opts: HttpOptions,
    ) -> Result<Self> {
        let api_key = api_key.into();
        require_nonempty("api_key", &api_key)?;
        Ok(Self {
            http: build_client(&opts)?,
            endpoint: endpoint.into(),
            api_key,
        })
    }

    async fn run(&self, q: &SearchQuery) -> Result<Vec<SearchResult>> {
        let mut params = vec![
            ("q", q.query.clone()),
            ("count", q.limit.clamp(1, MAX_COUNT).to_string()),
        ];
        match (&q.lang, &q.country) {
            // mkt 需要 语言-国家 形式；mkt 与 cc 互斥
            (Some(l), Some(c)) => {
                let lang = l.split('-').next().unwrap_or(l).to_lowercase();
                params.push(("mkt", format!("{lang}-{}", c.to_uppercase())));
            }
            (None, Some(c)) => params.push(("cc", c.to_uppercase())),
            (Some(l), None) => params.push(("setLang", l.clone())),
            (None, None) => {}
        }
        if let Some(t) = q.time_range {
            params.push((
                "freshness",
                match t {
                    TimeRange::Day => "Day",
                    TimeRange::Week => "Week",
                    TimeRange::Month => "Month",
                    TimeRange::Year => {
                        return Err(SearchError::Unsupported(
                            "Bing freshness 只支持 Day / Week / Month".into(),
                        ));
                    }
                }
                .to_owned(),
            ));
        }
        let rb = self
            .http
            .get(&self.endpoint)
            .header("Ocp-Apim-Subscription-Key", &self.api_key)
            .query(&params);
        let v = send_json(rb, false).await?;
        // 无结果时没有 webPages 字段
        let items = v
            .pointer("/webPages/value")
            .and_then(|r| r.as_array())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        Ok(finish(
            items
                .iter()
                .map(|r| (s(r, "url"), s(r, "name"), s(r, "snippet"))),
            q.limit,
        ))
    }
}

impl SearchBackend for BingBackend {
    fn name(&self) -> &'static str {
        "bing"
    }
    fn search<'a>(
        &'a self,
        query: &'a SearchQuery,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SearchResult>>> + Send + 'a>> {
        Box::pin(self.run(query))
    }
}
