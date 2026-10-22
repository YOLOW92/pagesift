use super::*;

/// extract 最多处理多少个页面，每页最多取多少字符的正文。
pub(super) const EXTRACT_MAX_PAGES: usize = 20;
pub(super) const EXTRACT_MAX_CHARS_PER_PAGE: usize = 20_000;

impl Scraper {
    pub async fn search(
        &self,
        req: &pagesift_api::types::SearchRequest,
    ) -> Result<serde_json::Value, ApiError> {
        let Some(backend) = &self.search else {
            return Err(ApiError::unsupported(
                "search is not configured: set [search] in pagesift.toml",
            ));
        };
        let mut q = pagesift_search::SearchQuery::new(req.query.clone(), req.limit);
        q.lang = req.lang.clone();
        q.country = req.country.clone();
        q.time_range = match req.tbs.as_deref() {
            Some("qdr:d") => Some(pagesift_search::TimeRange::Day),
            Some("qdr:w") => Some(pagesift_search::TimeRange::Week),
            Some("qdr:m") => Some(pagesift_search::TimeRange::Month),
            Some("qdr:y") => Some(pagesift_search::TimeRange::Year),
            _ => None,
        };
        let search = backend.search(&q);
        let results = match req.timeout {
            Some(ms) => tokio::time::timeout(Duration::from_millis(ms), search)
                .await
                .map_err(|_| ApiError::new(ErrorCode::Timeout, "search timed out"))?,
            None => search.await,
        }
        .map_err(|e| {
            ApiError::new(
                ErrorCode::UpstreamError,
                format!("search backend failed: {e}"),
            )
        })?;

        // 给了 scrapeOptions.formats 才抓取结果页面，并把文档字段合并进对应的结果
        let scrape = req
            .scrape_options
            .as_ref()
            .filter(|o| !o.formats.is_empty());
        let docs: Vec<Option<Document>> = match scrape {
            Some(opts) => {
                let futs = results
                    .iter()
                    .map(|r| async move { self.scrape(&r.url, opts).await.ok() });
                futures_util::future::join_all(futs).await
            }
            None => vec![None; results.len()],
        };
        let web: Vec<serde_json::Value> = results
            .into_iter()
            .zip(docs)
            .map(|(r, doc)| {
                let mut item = serde_json::json!({
                    "url": r.url,
                    "title": r.title,
                    "description": r.description,
                    "position": r.position,
                });
                if let Some(doc) = doc
                    && let (Some(obj), Ok(serde_json::Value::Object(d))) =
                        (item.as_object_mut(), serde_json::to_value(&doc))
                {
                    for (k, v) in d {
                        obj.entry(k).or_insert(v);
                    }
                }
                item
            })
            .collect();
        Ok(serde_json::json!({ "web": web }))
    }

    pub async fn extract(
        &self,
        req: &pagesift_api::types::ExtractRequest,
    ) -> Result<serde_json::Value, ApiError> {
        let Some(llm) = &self.llm else {
            return Err(ApiError::unsupported(
                "extract needs an LLM: set [llm] in pagesift.toml",
            ));
        };
        let mut urls: Vec<String> = Vec::new();
        for u in &req.urls {
            if let Some(site) = u.strip_suffix("/*") {
                let map: pagesift_api::types::MapRequest = serde_json::from_value(
                    serde_json::json!({ "url": site, "limit": EXTRACT_MAX_PAGES }),
                )
                .map_err(|e| ApiError::invalid(e.to_string()))?;
                urls.extend(self.map(&map).await?.into_iter().map(|l| l.url));
            } else {
                urls.push(u.clone());
            }
        }
        urls.truncate(EXTRACT_MAX_PAGES);
        let opts = req.scrape_options.clone().unwrap_or_default();
        let opts = &opts;
        let futs = urls
            .iter()
            .map(|u| async move { (u, self.scrape(u, opts).await) });
        let mut corpus = String::new();
        let mut errors = Vec::new();
        for (u, r) in futures_util::future::join_all(futs).await {
            match r {
                Ok(doc) => {
                    let md: String = doc
                        .markdown
                        .unwrap_or_default()
                        .chars()
                        .take(EXTRACT_MAX_CHARS_PER_PAGE)
                        .collect();
                    corpus.push_str(&format!("\n\n## Source: {u}\n\n{md}"));
                }
                Err(e) => errors.push(format!("{u}: {}", e.code.as_str())),
            }
        }
        if corpus.is_empty() {
            return Err(ApiError::new(
                ErrorCode::UpstreamError,
                format!("none of the pages could be scraped ({})", errors.join(", ")),
            ));
        }
        let schema = req
            .schema
            .clone()
            .unwrap_or_else(|| serde_json::json!({ "type": "object" }));
        let eo = pagesift_llm::ExtractOptions {
            prompt: req.prompt.clone(),
        };
        pagesift_llm::extract_json(llm, &corpus, &schema, &eo)
            .await
            .map_err(llm_error)
    }
}

pub(super) fn llm_error(e: pagesift_llm::LlmError) -> ApiError {
    ApiError::new(ErrorCode::UpstreamError, format!("LLM request failed: {e}"))
}
