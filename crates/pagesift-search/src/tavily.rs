//! Tavily Search API
//! 文档: https://docs.tavily.com/documentation/api-reference/endpoint/search
//!
//! `POST https://api.tavily.com/search`，鉴权 `Authorization: Bearer tvly-...`。
//! 请求体: query、max_results（0~20）、time_range（day / week / month / year）、
//! topic。响应: `results[].title/url/content/score`，`content` 作为 description。
//! Tavily 的 `country` 要求小写国家全名（如 "united states"），且没有语言参数，
//! 这里不做映射，lang 和 country 都忽略。

use std::future::Future;
use std::pin::Pin;

use serde_json::json;

use crate::backend::{SearchBackend, SearchQuery, SearchResult, TimeRange, finish};
use crate::error::{Result, SearchError};
use crate::http::{HttpOptions, build_client, require_nonempty, s, send_json};

const DEFAULT_ENDPOINT: &str = "https://api.tavily.com/search";
const MAX_RESULTS: usize = 20;

pub struct TavilyBackend {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl TavilyBackend {
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
        let mut body = json!({
            "query": q.query,
            "max_results": q.limit.clamp(1, MAX_RESULTS),
        });
        if let Some(t) = q.time_range {
            body["time_range"] = json!(match t {
                TimeRange::Day => "day",
                TimeRange::Week => "week",
                TimeRange::Month => "month",
                TimeRange::Year => "year",
            });
        }
        let payload =
            serde_json::to_vec(&body).map_err(|e| SearchError::InvalidResponse(e.to_string()))?;
        let rb = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .header("Content-Type", "application/json")
            .body(payload);
        let v = send_json(rb, false).await?;
        let items = v
            .get("results")
            .and_then(|r| r.as_array())
            .ok_or_else(|| SearchError::InvalidResponse("缺少 results 数组".into()))?;
        Ok(finish(
            items
                .iter()
                .map(|r| (s(r, "url"), s(r, "title"), s(r, "content"))),
            q.limit,
        ))
    }
}

impl SearchBackend for TavilyBackend {
    fn name(&self) -> &'static str {
        "tavily"
    }
    fn search<'a>(
        &'a self,
        query: &'a SearchQuery,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SearchResult>>> + Send + 'a>> {
        Box::pin(self.run(query))
    }
}
