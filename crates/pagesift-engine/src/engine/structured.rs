//! pagesift 扩展格式 `{"type": "extract", "schema": {...}}`：按 CSS schema 抽取，结果放在
//! `document.extract`（不复用 Firecrawl 的 `json` 格式，那个是交给 LLM 的）。
//!
//! 配了存储时选择器有记忆：键是“注册域 + schema id + 字段”，页面改版、选择器失效时用
//! `pagesift_extract::locate` 重定位，写回稳健的新选择器，在 `warnings` 里报 `relocated`。

use pagesift_api::types::{Document, Format, ScrapeOptions};
use pagesift_api::{ApiError, ErrorCode};
use pagesift_extract::locate::Target;
use pagesift_extract::{Schema, SelectorMemory};
use serde_json::Value;

use super::Scraper;
use super::Source;
use super::cpu::cpu_task;

/// 格式名。
pub const EXTRACT: &str = "extract";

/// 请求里的 extract 格式：解析并校验 schema（写错在抓取前就报）。
pub fn parse_format(opts: &ScrapeOptions) -> Result<Option<(Schema, String)>, ApiError> {
    let Some(f) = opts.formats.iter().find(|f| f.kind == EXTRACT) else {
        return Ok(None);
    };
    let raw = f
        .options
        .get("schema")
        .ok_or_else(|| ApiError::invalid("the extract format needs a `schema` object"))?;
    let schema = Schema::from_json(raw).map_err(|e| ApiError::invalid(format!("schema: {e}")))?;
    pagesift_extract::JsonCssExtractor::new(&schema)
        .map_err(|e| ApiError::invalid(format!("schema: {e}")))?;
    Ok(Some((schema, schema_id(raw))))
}

/// schema 的 id：规范化 JSON（键有序）的 BLAKE3 摘要前 16 个十六进制字符（64 位，同一站点上
/// 碰撞概率约 n²/2⁶⁵，可以忽略）。schema 改了 id 就变，旧记忆不会套到新 schema 上。
pub fn schema_id(raw: &Value) -> String {
    let canonical = serde_json::to_string(raw).unwrap_or_default();
    blake3::hash(canonical.as_bytes()).to_hex()[..16].to_string()
}

struct StoreMemory {
    store: pagesift_store::Store,
    site: String,
    schema: String,
}

impl SelectorMemory for StoreMemory {
    fn recall(&self, field: &str) -> Option<Target> {
        let e = self
            .store
            .selectors()
            .get(&self.site, &self.schema, field)
            .ok()??;
        serde_json::from_slice(&e.value).ok()
    }

    fn remember(&self, field: &str, target: &Target) {
        if let Ok(v) = serde_json::to_vec(target) {
            // 写失败只影响下次能否重定位，不影响这次的结果
            let _ = self
                .store
                .selectors()
                .put(&self.site, &self.schema, field, &v);
        }
    }
}

impl Scraper {
    /// 单页结果的出口：先走正文管线，要了 `extract` 时再在最终 HTML 上抽取。
    pub(super) async fn scrape_document(
        &self,
        source: Source<'_>,
        opts: &ScrapeOptions,
    ) -> Result<Document, ApiError> {
        let Some((schema, id)) = parse_format(opts)? else {
            return self.scrape_content(source, opts).await;
        };
        let site = match &source {
            Source::Url(url, _) => url::Url::parse(url)
                .ok()
                .and_then(|u| u.host_str().map(pagesift_content::site_key)),
            Source::Upload(_) => None,
        };
        let mut inner = opts.clone();
        inner.formats.retain(|f| f.kind != EXTRACT);
        let wants_raw = inner.wants("rawHtml");
        if !wants_raw {
            inner.formats.push(Format::new("rawHtml"));
        }
        let mut doc = self.scrape_content(source, &inner).await?;
        let html = if wants_raw {
            doc.raw_html.clone()
        } else {
            doc.raw_html.take()
        }
        .unwrap_or_default();
        let memory = match (&self.store, site) {
            (Some(store), Some(site)) => Some(StoreMemory {
                store: store.clone(),
                site,
                schema: id,
            }),
            _ => None,
        };
        let outcome = cpu_task(move || {
            let parsed = pagesift_html::Html::parse(&html);
            match &memory {
                Some(m) => pagesift_extract::extract_adaptive(&parsed, &schema, m),
                None => pagesift_extract::extract_adaptive(
                    &parsed,
                    &schema,
                    &pagesift_extract::NoMemory,
                ),
            }
        })
        .await?
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("extract: {e}")))?;
        let warnings = outcome.warnings();
        if !warnings.is_empty() {
            doc.metadata
                .pagesift
                .get_or_insert_with(Default::default)
                .warnings
                .extend(warnings);
        }
        doc.extract = Some(Value::Array(outcome.items));
        Ok(doc)
    }
}
