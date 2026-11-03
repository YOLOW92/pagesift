//! `find_similar` 的对外入口：抓一页，从例子元素找结构相同的兄弟。

use pagesift_api::ApiError;
use pagesift_api::types::{ScrapeOptions, SimilarData, SimilarItem};
use pagesift_html::{Html, Selector};

use super::Scraper;
use super::cpu::cpu_task;

impl Scraper {
    /// `selector` 取第一个命中的元素当例子；`opts` 要包含 `rawHtml` 格式。
    pub async fn find_similar(
        &self,
        url: &str,
        selector: &str,
        opts: &ScrapeOptions,
    ) -> Result<SimilarData, ApiError> {
        // 写错的选择器在抓取前就报
        Selector::parse(selector)
            .map_err(|e| ApiError::invalid(format!("selector {selector:?}: {e}")))?;
        let doc = self.scrape(url, opts).await?;
        let html = doc.raw_html.unwrap_or_default();
        let selector = selector.to_string();
        cpu_task(move || similar_in(&html, &selector)).await?
    }
}

fn similar_in(html: &str, selector: &str) -> Result<SimilarData, ApiError> {
    let doc = Html::parse(html);
    let sel = Selector::parse(selector)
        .map_err(|e| ApiError::invalid(format!("selector {selector:?}: {e}")))?;
    let example = doc.select(&sel).next().ok_or_else(|| {
        ApiError::invalid(format!("selector {selector:?} matches nothing on the page"))
    })?;
    let Some(found) = pagesift_extract::locate::find_similar(example, doc.root()) else {
        return Ok(SimilarData::default());
    };
    Ok(SimilarData {
        selector: found.selector,
        depth: Some(found.depth),
        items: found
            .items
            .iter()
            .map(|n| SimilarItem {
                text: n.text().split_whitespace().collect::<Vec<_>>().join(" "),
                html: n.html(),
            })
            .collect(),
    })
}
