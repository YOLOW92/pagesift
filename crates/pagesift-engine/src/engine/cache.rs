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
                if mode == RobotsMode::Respect {
                    let target = Url::parse(url).map_err(|e| ApiError::invalid(e.to_string()))?;
                    self.policy
                        .check_robots(&target)
                        .await
                        .map_err(policy_error)?;
                    if let Some(meta) = &doc.metadata.pagesift {
                        for hop in &meta.redirect_chain {
                            let hop =
                                Url::parse(hop).map_err(|e| ApiError::invalid(e.to_string()))?;
                            self.policy.check_robots(&hop).await.map_err(policy_error)?;
                        }
                    }
                    if let Some(final_url) = doc.metadata.url.as_deref() {
                        let final_url =
                            Url::parse(final_url).map_err(|e| ApiError::invalid(e.to_string()))?;
                        if final_url != target {
                            self.policy
                                .check_robots(&final_url)
                                .await
                                .map_err(policy_error)?;
                        }
                    }
                }
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
        // OCR 没做成（模型还在下载、识别出错）的结果不进缓存：下次要能重新识别
        let ocr_missed = doc.metadata.pagesift.as_ref().is_some_and(|m| {
            m.warnings
                .iter()
                .any(|w| w.starts_with("ocr_failed") || w.starts_with("ocr_incomplete"))
        });
        if policy.store_in_cache && !ocr_missed {
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
        // 带已认证调用方身份（owner）的结果可能来自该身份的会话，不能与匿名或其他身份共用缓存。
        // 身份池按 owner × origin × egress × fingerprint 隔离，缓存键里没有这些维度，所以整类请求不走缓存。
        if opts.authenticated_owner().is_some()
            || !opts.headers.is_empty()
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
        // respondTiming 决定等到页面的哪个阶段；带标签，不和上面按位置的 parsers 相撞
        if let (Some(t), Some(items)) = (&opts.respond_timing, key.as_array_mut()) {
            items.push(serde_json::json!({ "respondTiming": t }));
        }
        // The default retains existing cache keys; the opt-in output must not
        // share an entry with Markdown that removed embedded images.
        if !opts.remove_base64_images
            && let Some(items) = key.as_array_mut()
        {
            items.push(serde_json::json!({ "removeBase64Images": false }));
        }
        if let Some(g) = opts.graph.as_ref().filter(|g| g.enabled)
            && let Some(items) = key.as_array_mut()
        {
            items.push(serde_json::json!({ "graph": g }));
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, response::Html, routing::get};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn cached_page_cannot_bypass_robots() {
        let app = Router::new()
            .route(
                "/robots.txt",
                get(|| async { "User-agent: *\nDisallow: /secret\n" }),
            )
            .route(
                "/secret",
                get(|| async { Html("<html><body><h1>Secret</h1></body></html>") }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/secret", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let scraper = scraper(pagesift_store::Store::open_dir(dir.path()).unwrap());
        let opts = ScrapeOptions::default();
        scraper
            .scrape_cached(&url, &opts, RobotsMode::Ignore)
            .await
            .unwrap();
        let error = scraper
            .scrape_cached(&url, &opts, RobotsMode::Respect)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::RobotsDisallowed);
    }

    async fn site() -> (String, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let app = Router::new().route(
            "/",
            get(move || {
                let counter = counter.clone();
                async move {
                    let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                    Html(format!(
                        "<html><body><article><h1>Visit {n}</h1><p>This article is long enough \
                         for the main-content extractor to keep it, and the visit number shows \
                         whether this response came from the network or from the result cache.</p>\
                         </article></body></html>"
                    ))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/"), calls)
    }

    fn scraper(store: pagesift_store::Store) -> Scraper {
        let mut cfg = ScraperConfig {
            store: Some(store),
            ..Default::default()
        };
        cfg.fetcher.env_proxy = false;
        cfg.fetcher.ssrf.allow_private_network = true;
        cfg.policy.rate.default_interval = Duration::ZERO;
        Scraper::new(cfg).unwrap()
    }

    #[tokio::test]
    async fn embedded_image_option_isolated_in_cache() {
        let app = Router::new().route(
            "/",
            get(|| async {
                Html(r#"<html><body><p>A picture in this page.</p><img alt="x" src="data:image/png;base64,aGVsbG8="></body></html>"#)
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let scraper = scraper(pagesift_store::Store::open_dir(dir.path()).unwrap());
        let stripped = ScrapeOptions {
            engine: Some("http".into()),
            ..Default::default()
        };
        let kept = ScrapeOptions {
            remove_base64_images: false,
            ..stripped.clone()
        };
        for opts in [&stripped, &kept, &stripped, &kept] {
            let doc = scraper
                .scrape_cached(&url, opts, RobotsMode::Ignore)
                .await
                .unwrap();
            assert_eq!(
                doc.markdown
                    .unwrap()
                    .contains("data:image/png;base64,aGVsbG8="),
                !opts.remove_base64_images
            );
        }
    }

    /// 带 owner 的请求既不读匿名缓存，也不把自己的结果写进匿名缓存。
    #[tokio::test]
    async fn owner_scoped_requests_bypass_the_shared_result_cache() {
        let (url, calls) = site().await;
        let dir = tempfile::tempdir().unwrap();
        let store = pagesift_store::Store::open_dir(dir.path()).unwrap();
        let scraper = scraper(store);

        let anonymous = ScrapeOptions::default();
        let first = scraper
            .scrape_cached(&url, &anonymous, RobotsMode::Ignore)
            .await
            .unwrap();
        assert!(first.markdown.as_deref().unwrap().contains("Visit 1"));
        assert_eq!(first.metadata.cache_state.as_deref(), Some("miss"));

        let hit = scraper
            .scrape_cached(&url, &anonymous, RobotsMode::Ignore)
            .await
            .unwrap();
        assert!(hit.markdown.as_deref().unwrap().contains("Visit 1"));
        assert_eq!(hit.metadata.cache_state.as_deref(), Some("hit"));

        let mut tenant = ScrapeOptions {
            owner: Some("tenant-a".into()),
            ..Default::default()
        };
        let tenant_first = scraper
            .scrape_cached(&url, &tenant, RobotsMode::Ignore)
            .await
            .unwrap();
        assert!(
            tenant_first
                .markdown
                .as_deref()
                .unwrap()
                .contains("Visit 2")
        );
        assert_eq!(tenant_first.metadata.cache_state, None);

        let tenant_again = scraper
            .scrape_cached(&url, &tenant, RobotsMode::Ignore)
            .await
            .unwrap();
        assert!(
            tenant_again
                .markdown
                .as_deref()
                .unwrap()
                .contains("Visit 3")
        );
        assert_eq!(tenant_again.metadata.cache_state, None);

        let still_anonymous = scraper
            .scrape_cached(&url, &anonymous, RobotsMode::Ignore)
            .await
            .unwrap();
        assert!(
            still_anonymous
                .markdown
                .as_deref()
                .unwrap()
                .contains("Visit 1")
        );
        assert_eq!(still_anonymous.metadata.cache_state.as_deref(), Some("hit"));

        tenant.owner = Some("tenant-b".into());
        let other_tenant = scraper
            .scrape_cached(&url, &tenant, RobotsMode::Ignore)
            .await
            .unwrap();
        assert!(
            other_tenant
                .markdown
                .as_deref()
                .unwrap()
                .contains("Visit 4")
        );
        assert_eq!(other_tenant.metadata.cache_state, None);
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }
}
