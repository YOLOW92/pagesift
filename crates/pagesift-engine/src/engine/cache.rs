//! 结果缓存：按 URL 和影响结果的参数查找、写入 `Document`。查找、解码、编码、写入都在 CPU 工人上。

use super::*;

impl Scraper {
    pub(super) async fn scrape_cached(
        &self,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
    ) -> Result<Document, ApiError> {
        // 审计要存快照时内部多要一份 rawHtml；缓存键仍按调用方的参数算，写缓存前去掉
        let capture =
            !opts.wants("rawHtml") && self.audit.as_ref().is_some_and(|a| a.wants_raw_html());
        let with_raw = capture.then(|| {
            let mut o = opts.clone();
            o.formats.push(pagesift_api::types::Format::new("rawHtml"));
            o
        });
        let fetch_opts = with_raw.as_ref().unwrap_or(opts);
        let Some((store, key)) = self.cache_key(url, opts) else {
            return self.fetch_and_extract(url, fetch_opts, mode).await;
        };
        let policy = pagesift_store::CachePolicy::from_firecrawl(
            opts.max_age,
            opts.min_age,
            Some(opts.store_in_cache),
        );
        // 查缓存和解码 JSON 在同一个 CPU 任务里做，不占异步线程
        let lookup = {
            let (store, key) = (store.clone(), key.clone());
            cpu_task(move || {
                store
                    .cache()
                    .lookup(&key, &policy)
                    .map(|found| match found {
                        pagesift_store::CacheLookup::Hit(entry) => {
                            // 命中但解不出来（格式版本变了）：当作没命中
                            match pagesift_sink::cache::decode(&entry.value) {
                                Some(doc) => Cached::Hit(Box::new(doc), entry.stored_at),
                                None => Cached::Miss,
                            }
                        }
                        pagesift_store::CacheLookup::NoCachedData => Cached::NoData,
                        pagesift_store::CacheLookup::Miss => Cached::Miss,
                    })
            })
            .await?
        };
        match lookup {
            Ok(Cached::Hit(doc, stored_at)) => {
                let mut doc = *doc;
                doc.metadata.cache_state = Some("hit".into());
                doc.metadata.cached_at = Some(pagesift_api::rfc3339(stored_at));
                return Ok(doc);
            }
            Ok(Cached::NoData) => {
                return Err(ApiError::new(
                    ErrorCode::NotFound,
                    "no cached data matches minAge for this URL",
                ));
            }
            Ok(Cached::Miss) => {}
            // 缓存读不出来不影响抓取
            Err(e) => tracing::warn!("cache lookup failed: {e}"),
        }
        let mut doc = self.fetch_and_extract(url, fetch_opts, mode).await?;
        doc.metadata.cache_state = Some("miss".into());
        let captured = if capture { doc.raw_html.take() } else { None };
        if policy.store_in_cache {
            // 编码和写入一起交给 CPU 工人，写完把文档交回来（不拷贝）
            let (back, r) = cpu_task(move || {
                let r =
                    pagesift_sink::cache::encode(&doc).map(|bytes| store.cache().put(&key, &bytes));
                (doc, r)
            })
            .await?;
            doc = back;
            if let Ok(Err(e)) = r {
                tracing::warn!("cache write failed: {e}");
            }
        }
        if captured.is_some() {
            doc.raw_html = captured;
        }
        Ok(doc)
    }

    /// 缓存键：URL 加上影响结果的参数。带自定义请求头、actions 或条件请求时不走缓存（和 Firecrawl 一致）。
    pub(super) fn cache_key(
        &self,
        url: &str,
        opts: &ScrapeOptions,
    ) -> Option<(pagesift_store::Store, Vec<u8>)> {
        let store = self.store.clone()?;
        if !opts.headers.is_empty()
            || opts.discover_scripts
            || opts.explain
            || !opts.actions.is_empty()
            || opts.if_none_match.is_some()
            || opts.if_modified_since.is_some()
        {
            return None;
        }
        let mut formats: Vec<&str> = opts.formats.iter().map(|f| f.kind.as_str()).collect();
        formats.sort_unstable();
        // 第一项是缓存格式的代数：2 起是二进制文档（`pagesift_sink::cache`），1 的 JSON 缓存不再命中
        let mut key = serde_json::json!([
            2,
            url,
            formats,
            opts.only_main_content,
            opts.include_tags,
            opts.exclude_tags,
            opts.engine.as_deref().unwrap_or("auto"),
            opts.mobile,
            opts.wait_for,
            opts.block_ads,
            opts.location,
        ]);
        // PDF 的 parsers 改变结果；没给时不加这一项，已有的缓存键不变
        if let (Some(p), Some(items)) = (&opts.parsers, key.as_array_mut()) {
            items.push(p.clone());
        }
        Some((store, key.to_string().into_bytes()))
    }
}

/// 缓存查找的结果（已在 CPU 工人上解码）。
enum Cached {
    Hit(Box<Document>, std::time::SystemTime),
    NoData,
    Miss,
}
