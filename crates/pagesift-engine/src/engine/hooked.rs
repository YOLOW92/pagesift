//! 单页出口套上钩子（`pagesift_hooks`）：抓取前改请求或跳过、抓取后重试一次或丢弃、抽取后改文档。
//! 没有钩子时直接走原路径（一次 `is_empty` 判断）。

use pagesift_api::types::{Document, ScrapeOptions};
use pagesift_api::{ApiError, ErrorCode};
use pagesift_hooks::{Request, Verdict};

use super::{Scraper, Source};

impl Scraper {
    pub(super) async fn scrape_inner(
        &self,
        source: Source<'_>,
        opts: &ScrapeOptions,
    ) -> Result<Document, ApiError> {
        if self.hooks.is_empty() {
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
        let mut doc =
            Box::pin(self.scrape_plain(Source::Url(&req.url, mode), &req.options)).await?;
        match self.hooks.run_response(&req.url, &doc) {
            Verdict::Keep => {}
            // 只重试一次：钩子再要求重试也照用第二次的结果
            Verdict::Retry => {
                doc =
                    Box::pin(self.scrape_plain(Source::Url(&req.url, mode), &req.options)).await?;
            }
            Verdict::Drop(why) => {
                return Err(ApiError::new(
                    ErrorCode::UpstreamError,
                    format!("dropped by a response hook: {why}"),
                ));
            }
        }
        Ok(self.hooks.run_document(&req.url, doc))
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
