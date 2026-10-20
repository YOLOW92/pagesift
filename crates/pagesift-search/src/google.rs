//! Google Programmable Search Engine（Custom Search JSON API）
//! 文档: https://developers.google.com/custom-search/v1/reference/rest/v1/cse/list
//!
//! `GET https://customsearch.googleapis.com/customsearch/v1`，key 放在查询参数 `key`。
//! 参数: cx、q、num（1~10）、lr（`lang_xx`）、gl（两位国家码）、
//! dateRestrict（d[n] / w[n] / m[n] / y[n]）。单次最多 10 条，limit 更大时只返回 10 条。
//! 响应: `items[].title/link/snippet`（字段名来自 Search 类型参考，本次未能抓取到该页，
//! 属于未在官方文档重新核实的部分）。无结果时没有 `items`。
//! 配额用尽时 Google 返回 403（reason 为 dailyLimitExceeded / rateLimitExceeded 等），
//! 这里归入 RateLimited 而不是 Auth。

use std::future::Future;
use std::pin::Pin;

use crate::backend::{SearchBackend, SearchQuery, SearchResult, TimeRange, finish};
use crate::error::Result;
use crate::http::{HttpOptions, build_client, require_nonempty, s, send_json};

const DEFAULT_ENDPOINT: &str = "https://customsearch.googleapis.com/customsearch/v1";
const MAX_NUM: usize = 10;

pub struct GoogleCseBackend {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
    engine_id: String,
}

impl GoogleCseBackend {
    pub fn new(
        api_key: impl Into<String>,
        engine_id: impl Into<String>,
        opts: HttpOptions,
    ) -> Result<Self> {
        Self::with_endpoint(DEFAULT_ENDPOINT, api_key, engine_id, opts)
    }

    pub fn with_endpoint(
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
        engine_id: impl Into<String>,
        opts: HttpOptions,
    ) -> Result<Self> {
        let (api_key, engine_id) = (api_key.into(), engine_id.into());
        require_nonempty("api_key", &api_key)?;
        require_nonempty("engine_id (cx)", &engine_id)?;
        Ok(Self {
            http: build_client(&opts)?,
            endpoint: endpoint.into(),
            api_key,
            engine_id,
        })
    }

    async fn run(&self, q: &SearchQuery) -> Result<Vec<SearchResult>> {
        let mut params = vec![
            ("key", self.api_key.clone()),
            ("cx", self.engine_id.clone()),
            ("q", q.query.clone()),
            ("num", q.limit.clamp(1, MAX_NUM).to_string()),
        ];
        if let Some(l) = &q.lang {
            params.push(("lr", lr_value(l)));
        }
        if let Some(c) = &q.country {
            params.push(("gl", c.to_lowercase()));
        }
        if let Some(t) = q.time_range {
            params.push((
                "dateRestrict",
                match t {
                    TimeRange::Day => "d1",
                    TimeRange::Week => "w1",
                    TimeRange::Month => "m1",
                    TimeRange::Year => "y1",
                }
                .to_owned(),
            ));
        }
        let v = send_json(self.http.get(&self.endpoint).query(&params), true).await?;
        let items = v
            .get("items")
            .and_then(|r| r.as_array())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        Ok(finish(
            items
                .iter()
                .map(|r| (s(r, "link"), s(r, "title"), s(r, "snippet"))),
            q.limit,
        ))
    }
}

/// `lr` 的取值是 `lang_xx`，中文区分 `lang_zh-CN` 和 `lang_zh-TW`。
fn lr_value(lang: &str) -> String {
    let l = lang.to_lowercase();
    if l == "zh-tw" || l == "zh-hant" {
        "lang_zh-TW".into()
    } else if l.starts_with("zh") {
        "lang_zh-CN".into()
    } else {
        format!("lang_{}", l.split('-').next().unwrap_or(&l))
    }
}

impl SearchBackend for GoogleCseBackend {
    fn name(&self) -> &'static str {
        "google_cse"
    }
    fn search<'a>(
        &'a self,
        query: &'a SearchQuery,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SearchResult>>> + Send + 'a>> {
        Box::pin(self.run(query))
    }
}
