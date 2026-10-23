//! 抓取选项：Python 关键字参数 → `ScrapeOptions`，和 HTTP 接口用同样的校验规则（PRD 6.5.1）。

use std::collections::BTreeMap;
use std::sync::Arc;

use pagesift::api::ApiError;
use pagesift::api::types::{Format, Location, ScrapeOptions};
use pyo3::prelude::*;

use crate::errors::to_py_err;

/// 校验过的选项。由 Python 层的 `ScrapeConfig` 生成，抓取时直接复用，不再解析。
#[pyclass(frozen, module = "pagesift._pagesift")]
pub struct Options {
    pub inner: Arc<ScrapeOptions>,
}

#[pymethods]
impl Options {
    #[new]
    #[pyo3(signature = (
        *,
        formats = None,
        only_main_content = true,
        engine = None,
        timeout = None,
        wait_for = 0,
        headers = None,
        include_tags = None,
        exclude_tags = None,
        ignore_robots_txt = false,
        mobile = false,
        languages = None,
        proxy = None,
        if_none_match = None,
        if_modified_since = None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        formats: Option<Vec<String>>,
        only_main_content: bool,
        engine: Option<String>,
        timeout: Option<u64>,
        wait_for: u64,
        headers: Option<BTreeMap<String, String>>,
        include_tags: Option<Vec<String>>,
        exclude_tags: Option<Vec<String>>,
        ignore_robots_txt: bool,
        mobile: bool,
        languages: Option<Vec<String>>,
        proxy: Option<String>,
        if_none_match: Option<String>,
        if_modified_since: Option<String>,
    ) -> PyResult<Self> {
        let mut o = ScrapeOptions::default();
        if let Some(f) = formats {
            o.formats = f.iter().map(|k| Format::new(format_name(k))).collect();
        }
        o.only_main_content = only_main_content;
        o.engine = engine;
        o.timeout = timeout;
        o.wait_for = wait_for;
        o.headers = headers.unwrap_or_default();
        o.include_tags = include_tags.unwrap_or_default();
        o.exclude_tags = exclude_tags.unwrap_or_default();
        o.ignore_robots_txt = ignore_robots_txt;
        o.mobile = mobile;
        o.proxy = proxy;
        o.if_none_match = if_none_match;
        o.if_modified_since = if_modified_since;
        if let Some(l) = languages {
            o.location = Some(Location {
                country: None,
                languages: l,
            });
        }
        validate(&o).map_err(|e| to_py_err(&e, None))?;
        Ok(Self { inner: Arc::new(o) })
    }

    fn __repr__(&self) -> String {
        format!("<pagesift._pagesift.Options {:?}>", self.inner)
    }
}

/// `pagesift::Scraper` 支持的格式（和它的 `Engine::supported_formats` 一致）。
const FORMATS: &[&str] = &["markdown", "html", "rawHtml", "links", "images"];

/// Python 里用 snake_case（`raw_html`），Firecrawl 用 camelCase（`rawHtml`），两种都接受。
fn format_name(k: &str) -> &str {
    match k {
        "raw_html" => "rawHtml",
        other => other,
    }
}

fn validate(o: &ScrapeOptions) -> Result<(), ApiError> {
    if let Some(f) = o
        .formats
        .iter()
        .find(|f| !FORMATS.contains(&f.kind.as_str()))
    {
        return Err(ApiError::unsupported(format!(
            "format {:?} is not supported (supported: markdown, html, raw_html, links, images)",
            f.kind
        )));
    }
    if let Some(e) = o.engine.as_deref()
        && !matches!(e, "http" | "browser" | "auto")
    {
        return Err(ApiError::invalid(format!(
            "engine must be http, browser or auto, got {e:?}"
        )));
    }
    // 引擎还没有实现按标签筛选：明确报错，不悄悄忽略
    if !o.include_tags.is_empty() || !o.exclude_tags.is_empty() {
        return Err(ApiError::unsupported(
            "include_tags / exclude_tags are not supported by this build yet",
        ));
    }
    if let Some(t) = o.timeout {
        if t < 1000 {
            return Err(ApiError::invalid(
                "timeout is in milliseconds and must be at least 1000",
            ));
        }
        if o.wait_for > t / 2 {
            return Err(ApiError::invalid(
                "wait_for must not exceed half of timeout",
            ));
        }
    }
    if o.wait_for > 60_000 {
        return Err(ApiError::invalid("wait_for must not exceed 60000 ms"));
    }
    Ok(())
}

/// 和 HTTP 接口的 `check_url` 一致：只接受带主机名的 http(s) 地址。
pub fn check_url(raw: &str) -> Result<(), ApiError> {
    match url::Url::parse(raw) {
        Ok(u) if matches!(u.scheme(), "http" | "https") && u.host().is_some() => Ok(()),
        Ok(_) => Err(ApiError::invalid(format!(
            "only http(s) URLs are supported: {raw}"
        ))),
        Err(e) => Err(ApiError::invalid(format!("invalid URL {raw:?}: {e}"))),
    }
}
