use super::*;

impl Scraper {
    /// `exportImages`：内部要图片列表（调用方没要 `images` 格式时最后去掉），不读也不写结果缓存
    /// （缓存里的文档不带附件）；PDF、Office 在转换时导出，网页图片在这之后取回。
    pub(super) async fn scrape_exporting_images(
        &self,
        source: Source<'_>,
        opts: &ScrapeOptions,
    ) -> Result<Document, ApiError> {
        let mode = match &source {
            Source::Url(_, mode) => *mode,
            Source::Upload(_) => RobotsMode::Respect,
        };
        let mut inner = opts.clone();
        if opts.image_assets.is_some() {
            // Document formats produce their original media through this path.
            inner.export_images = true;
            if !inner.wants("markdown") {
                inner
                    .formats
                    .push(pagesift_api::types::Format::new("markdown"));
            }
        }
        if !inner.wants("images") {
            inner
                .formats
                .push(pagesift_api::types::Format::new("images"));
        }
        inner.max_age = Some(0);
        inner.store_in_cache = false;
        let mut result = self.scrape_document(source, &inner).await;
        if let Ok(doc) = &mut result {
            if let Some(asset_mode) = opts.image_assets {
                if let (Some(progress), Some(markdown)) = (&opts.markdown_progress, &doc.markdown)
                    && progress.0.receiver_count() > 0
                {
                    let limit = 1024 * 1024;
                    let mut end = markdown.len().min(limit);
                    while !markdown.is_char_boundary(end) {
                        end -= 1;
                    }
                    let _ = progress.0.send(pagesift_api::types::MarkdownPreview {
                        url: doc.metadata.source_url.clone().unwrap_or_default(),
                        markdown: markdown[..end].to_string(),
                        truncated: end < markdown.len(),
                    });
                }
                self.assetize_images(doc, mode, asset_mode, opts.asset_fetch_cache.as_ref())
                    .await;
                if !opts.wants("markdown") {
                    doc.markdown = None;
                }
            } else {
                self.export_web_images(doc, mode).await;
            }
            if !opts.wants("images") {
                doc.images = None;
            }
            crate::notice::fill_warning(doc);
        }
        result
    }

    /// 基础文档（抓取或上传文件解析）之上，再做 `queryFilter`、分块、LLM 格式和变化检测。
    pub(super) async fn scrape_content(
        &self,
        source: Source<'_>,
        opts: &ScrapeOptions,
    ) -> Result<Document, ApiError> {
        let wants_llm = opts.wants("summary") || opts.wants("json");
        let tracking = opts
            .formats
            .iter()
            .find(|f| f.kind == "changeTracking")
            .map(crate::change::Tracking::parse)
            .transpose()?;
        if !wants_llm
            && tracking.is_none()
            && opts.query_filter.is_none()
            && opts.chunking.is_none()
        {
            return self.base_document(source, opts).await;
        }
        let url = match &source {
            Source::Url(url, _) => Some(*url),
            Source::Upload(_) => None,
        };
        if tracking.is_some() && url.is_none() {
            return Err(ApiError::unsupported(
                "changeTracking needs a URL: it is not available for uploaded files",
            ));
        }
        if (wants_llm || tracking.as_ref().is_some_and(|t| t.json)) && self.llm.is_none() {
            return Err(ApiError::unsupported(
                "summary and json formats (and the json mode of changeTracking) need an LLM: set [llm] in pagesift.toml",
            ));
        }
        if tracking.is_some() && self.store.is_none() {
            return Err(ApiError::unsupported(
                "changeTracking keeps history in the store: enable [cache] in pagesift.toml",
            ));
        }
        // 这些都基于正文 Markdown；用户没要 markdown 时内部照样取，最后去掉
        let mut inner = opts.clone();
        inner
            .formats
            .retain(|f| f.kind != "summary" && f.kind != "json");
        let wants_md = inner.wants("markdown");
        if !wants_md {
            inner
                .formats
                .push(pagesift_api::types::Format::new("markdown"));
        }
        // 变化检测要和最新的页面比较：调用方没给 maxAge 时不读缓存
        if tracking.is_some() && inner.max_age.is_none() {
            inner.max_age = Some(0);
        }
        let mut doc = self.base_document(source, &inner).await?;
        let pre_filter_md = (opts.query_filter.is_some() && doc.graph.is_some())
            .then(|| doc.markdown.clone())
            .flatten();
        if let Some(q) = &opts.query_filter {
            apply_query_filter(&mut doc, q).await?;
        }
        if let Some(c) = &opts.chunking {
            doc.chunks = Some(chunk_markdown(
                doc.markdown.as_deref().unwrap_or_default(),
                c,
            )?);
        }
        if (opts.query_filter.is_some() || opts.chunking.is_some())
            && let Some(gopts) = crate::graph::effective_graph_options(opts)
        {
            crate::graph::resync_graph_after_transform(&mut doc, &gopts, pre_filter_md.as_deref());
        }
        let md = doc.markdown.clone().unwrap_or_default();
        if let Some(llm) = self.llm.as_ref().filter(|_| wants_llm) {
            if opts.wants("summary") {
                let summary = pagesift_llm::summarize(llm, &md, &Default::default())
                    .await
                    .map_err(llm_error)?;
                doc.summary = Some(summary);
            }
            if let Some(f) = opts.formats.iter().find(|f| f.kind == "json") {
                let schema = f
                    .options
                    .get("schema")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({ "type": "object" }));
                let eo = pagesift_llm::ExtractOptions {
                    prompt: f
                        .options
                        .get("prompt")
                        .and_then(|p| p.as_str())
                        .map(String::from),
                };
                doc.json = Some(
                    pagesift_llm::extract_json(llm, &md, &schema, &eo)
                        .await
                        .map_err(llm_error)?,
                );
            }
        }
        if let (Some(t), Some(store), Some(url)) = (&tracking, &self.store, url) {
            self.track_changes(store, t, url, opts, &mut doc).await?;
        }
        if !wants_md {
            doc.markdown = None;
        }
        Ok(doc)
    }

    async fn base_document(
        &self,
        source: Source<'_>,
        opts: &ScrapeOptions,
    ) -> Result<Document, ApiError> {
        match source {
            Source::Url(url, mode) => self.scrape_cached(url, opts, mode).await,
            Source::Upload(upload) => self.parse_upload(upload, opts).await,
        }
    }

    /// `changeTracking`：在服务端可信作用域下和上一次的正文比较，结果放进 `doc.change_tracking`。
    pub(super) async fn track_changes(
        &self,
        store: &pagesift_store::Store,
        t: &crate::change::Tracking,
        url: &str,
        opts: &ScrapeOptions,
        doc: &mut Document,
    ) -> Result<(), ApiError> {
        use crate::change::{ChangeScope, Current, Previous};
        let ext = doc.metadata.pagesift.get_or_insert_with(Default::default);
        let current = if ext.not_modified {
            Current::NotModified
        } else if matches!(doc.metadata.status_code, 404 | 410) {
            Current::Gone
        } else {
            Current::Page(doc.markdown.as_deref().unwrap_or_default())
        };
        let scope = ChangeScope::from_scrape_options(opts);
        let mut outcome = crate::change::track_scoped(store, t, &scope, url, current).await?;
        ext.warnings.append(&mut outcome.warnings);
        if let (true, "changed", Previous::Page(prev), Some(llm)) = (
            t.json,
            outcome.tracking.change_status.as_str(),
            &outcome.previous,
            &self.llm,
        ) {
            let schema = t
                .schema
                .clone()
                .unwrap_or_else(|| serde_json::json!({ "type": "object" }));
            let eo = pagesift_llm::ExtractOptions {
                prompt: t.prompt.clone(),
            };
            let cur = doc.markdown.as_deref().unwrap_or_default();
            let (before, after) = futures_util::future::try_join(
                pagesift_llm::extract_json(llm, prev, &schema, &eo),
                pagesift_llm::extract_json(llm, cur, &schema, &eo),
            )
            .await
            .map_err(llm_error)?;
            outcome.tracking.json = Some(crate::change::json_changes(&before, &after));
        }
        doc.change_tracking = Some(outcome.tracking);
        Ok(())
    }
}
