//! 单页出口套上钩子（`pagesift_hooks`）：抓取前改请求或跳过、抓取后重试一次或丢弃、抽取后改文档。
//! 没有钩子时直接走原路径（一次 `is_empty` 判断）。

use pagesift_api::types::{Document, ScrapeOptions};
use pagesift_api::{ApiError, ErrorCode};
use pagesift_hooks::{Request, Verdict};

use super::{Scraper, Source};

fn attempt_scrape_options(
    opts: &ScrapeOptions,
) -> Option<(pagesift_graph::GraphOptions, ScrapeOptions)> {
    let g = crate::graph::effective_graph_options(opts).filter(|g| g.persist)?;
    let mut attempt = opts.clone();
    attempt.graph = Some(g.as_attempt());
    Some((g, attempt))
}

impl Scraper {
    pub(super) async fn scrape_inner(
        &self,
        source: Source<'_>,
        opts: &ScrapeOptions,
    ) -> Result<Document, ApiError> {
        let is_url = matches!(source, Source::Url(_, _));
        if self.hooks.is_empty() {
            if let Some((orig_gopts, attempt_opts)) =
                is_url.then(|| attempt_scrape_options(opts)).flatten()
            {
                let (res, mut staged) =
                    crate::graph::stage::with_graph_stage(self.scrape_plain(source, &attempt_opts))
                        .await;
                let mut doc = res?;
                self.finalize_staged_graph_commit(
                    &orig_gopts,
                    &attempt_opts,
                    &mut doc,
                    &mut staged,
                );
                return Ok(doc);
            }
            return self.scrape_plain(source, opts).await;
        }
        let (url, mode) = match source {
            Source::Url(url, mode) => (url.to_string(), mode),
            Source::Upload(_) => {
                let doc = Box::pin(self.scrape_plain(source, opts)).await?;
                let name = doc.metadata.source_url.clone().unwrap_or_default();
                return Ok(self.hooks.run_document(&name, doc));
            }
        };
        let req = self
            .hooks
            .run_request(Request {
                url,
                options: opts.clone(),
            })
            .map_err(|why| {
                ApiError::new(
                    ErrorCode::UnsupportedOption,
                    format!("skipped by a request hook: {why}"),
                )
            })?;
        let deferred = attempt_scrape_options(&req.options);
        let attempt_ref = deferred.as_ref().map_or(&req.options, |(_, a)| a);
        let (res, mut staged) = crate::graph::stage::with_graph_stage(async {
            let mut doc =
                Box::pin(self.scrape_plain(Source::Url(&req.url, mode), attempt_ref)).await?;
            match self.hooks.run_response(&req.url, &doc) {
                Verdict::Keep => {}
                // 只重试一次：钩子再要求重试也照用第二次的结果
                Verdict::Retry => {
                    doc = Box::pin(self.scrape_plain(Source::Url(&req.url, mode), attempt_ref))
                        .await?;
                }
                Verdict::Drop(why) => {
                    return Err(ApiError::new(
                        ErrorCode::UpstreamError,
                        format!("dropped by a response hook: {why}"),
                    ));
                }
            }
            Ok(doc)
        })
        .await;
        let mut doc = res?;
        if let Some((orig_gopts, _)) = &deferred {
            self.finalize_staged_graph_commit(orig_gopts, &req.options, &mut doc, &mut staged);
        }
        Ok(self.hooks.run_document(&req.url, doc))
    }

    fn finalize_staged_graph_commit(
        &self,
        orig_gopts: &pagesift_graph::GraphOptions,
        opts: &ScrapeOptions,
        doc: &mut Document,
        staged: &mut std::collections::HashMap<String, crate::graph::stage::StagedGraphCommit>,
    ) {
        if doc.metadata.cache_state.as_deref() == Some("hit")
            || doc
                .metadata
                .pagesift
                .as_ref()
                .and_then(|p| p.engine.as_deref())
                == Some("upload")
        {
            return;
        }
        let Some(store) = &self.store else {
            return;
        };
        let Some(entry) = doc.graph.as_ref().and_then(|g| staged.remove(&g.doc_id)) else {
            return;
        };
        crate::graph::commit_staged_document(
            store,
            orig_gopts,
            opts.effective_graph_tenant().as_deref(),
            doc,
            entry,
        );
    }
}

/// `Scraper::builder(config).hook(...)`：在代码里注册钩子（和 `[[rules]]` 编译出的放在同一组链里）。
pub struct ScraperBuilder {
    config: super::ScraperConfig,
}

impl Scraper {
    pub fn builder(config: super::ScraperConfig) -> ScraperBuilder {
        ScraperBuilder { config }
    }

    /// 输出阶段的钩子由写结果的一方调用：Rust 用户在自己写结果的地方调；内置入口里只有 CLI 的 crawl 调用。
    pub fn hooks(&self) -> &pagesift_hooks::Hooks {
        &self.hooks
    }
}

impl ScraperBuilder {
    /// 注册一组钩子（`pagesift_hooks::Hooks` 上用 `request`、`response`、`document`、`sink` 加好）。
    pub fn hook(mut self, hooks: &pagesift_hooks::Hooks) -> Self {
        self.config.hooks.extend(hooks);
        self
    }

    pub fn build(self) -> Result<Scraper, super::ScraperError> {
        Scraper::new(self.config)
    }
}
