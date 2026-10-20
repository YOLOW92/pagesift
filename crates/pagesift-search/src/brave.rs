//! Brave Search API，Web Search
//! 文档: https://api-dashboard.search.brave.com/app/documentation/web-search/get-started
//!
//! `GET https://api.search.brave.com/res/v1/web/search`，鉴权头 `X-Subscription-Token`。
//! 参数: q、count（最大 20）、country（两位国家码）、search_lang、
//! freshness（pd / pw / pm / py）。响应: `web.results[].title/url/description`。

use std::future::Future;
use std::pin::Pin;

use crate::backend::{SearchBackend, SearchQuery, SearchResult, TimeRange, finish};
use crate::error::Result;
use crate::http::{HttpOptions, build_client, require_nonempty, s, send_json};

const DEFAULT_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";
const MAX_COUNT: usize = 20;

pub struct BraveBackend {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl BraveBackend {
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
        if let Some(c) = &q.country {
            params.push(("country", c.to_uppercase()));
        }
        if let Some(l) = &q.lang {
            // search_lang 只要主语言子标签，如 zh-CN → zh
            params.push((
                "search_lang",
                l.split('-').next().unwrap_or(l).to_lowercase(),
            ));
        }
        if let Some(t) = q.time_range {
            params.push((
                "freshness",
                match t {
                    TimeRange::Day => "pd",
                    TimeRange::Week => "pw",
                    TimeRange::Month => "pm",
                    TimeRange::Year => "py",
                }
                .to_owned(),
            ));
        }
        let rb = self
            .http
            .get(&self.endpoint)
            .header("X-Subscription-Token", &self.api_key)
            .header("Accept", "application/json")
            .query(&params);
        let v = send_json(rb, false).await?;
        // 没有结果时 web 字段可能整个缺失
        let items = v
            .pointer("/web/results")
            .and_then(|r| r.as_array())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        Ok(finish(
            items
                .iter()
                .map(|r| (s(r, "url"), s(r, "title"), s(r, "description"))),
            q.limit,
        ))
    }
}

impl SearchBackend for BraveBackend {
    fn name(&self) -> &'static str {
        "brave"
    }
    fn search<'a>(
        &'a self,
        query: &'a SearchQuery,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SearchResult>>> + Send + 'a>> {
        Box::pin(self.run(query))
    }
}
