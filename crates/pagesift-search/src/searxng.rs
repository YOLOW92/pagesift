//! SearXNG Search API
//! 文档: https://docs.searxng.org/dev/search_api.html
//!
//! `GET {base}/search?q=&format=json&language=&time_range=&pageno=`。
//! `time_range` 文档只列了 day、month、year；`format=json` 需要实例在 settings.yml
//! 的 `search.formats` 里启用，否则返回 403。
//! JSON 响应字段（`results[].url/title/content`）文档页没有写，来自实例实际输出，
//! 属于未在官方文档核实的部分。SearXNG 没有条数参数，limit 在本地截断。

use std::future::Future;
use std::pin::Pin;

use crate::backend::{SearchBackend, SearchQuery, SearchResult, TimeRange, finish};
use crate::error::{Result, SearchError};
use crate::http::{HttpOptions, build_client, require_nonempty, s, send_json};

pub struct SearxngBackend {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
}

impl SearxngBackend {
    /// `base_url` 是实例根地址，如 `http://localhost:8080`。
    /// `api_key` 仅用于前面有反向代理做 Bearer 鉴权的实例。
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        opts: HttpOptions,
    ) -> Result<Self> {
        let base_url = base_url.into();
        require_nonempty("base_url", &base_url)?;
        Ok(Self {
            http: build_client(&opts)?,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
        })
    }

    async fn run(&self, q: &SearchQuery) -> Result<Vec<SearchResult>> {
        let mut params = vec![
            ("q", q.query.clone()),
            ("format", "json".to_owned()),
            ("pageno", "1".to_owned()),
        ];
        let lang = match (&q.lang, &q.country) {
            (Some(l), Some(c)) if !l.contains('-') => Some(format!("{l}-{}", c.to_uppercase())),
            (Some(l), _) => Some(l.clone()),
            _ => None,
        };
        if let Some(l) = lang {
            params.push(("language", l));
        }
        if let Some(t) = q.time_range {
            // 文档没有 week，用更宽的 month，宁可多返回也不漏
            params.push((
                "time_range",
                match t {
                    TimeRange::Day => "day",
                    TimeRange::Week | TimeRange::Month => "month",
                    TimeRange::Year => "year",
                }
                .to_owned(),
            ));
        }
        let mut rb = self
            .http
            .get(format!("{}/search", self.base_url))
            .query(&params);
        if let Some(k) = &self.api_key {
            rb = rb.bearer_auth(k);
        }
        let v = send_json(rb, false).await.map_err(|e| match e {
            SearchError::Auth { status: 403, .. } => SearchError::Upstream {
                status: 403,
                message: "实例拒绝 format=json，请在 settings.yml 的 search.formats 里启用 json"
                    .into(),
            },
            e => e,
        })?;
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

impl SearchBackend for SearxngBackend {
    fn name(&self) -> &'static str {
        "searxng"
    }
    fn search<'a>(
        &'a self,
        query: &'a SearchQuery,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SearchResult>>> + Send + 'a>> {
        Box::pin(self.run(query))
    }
}
