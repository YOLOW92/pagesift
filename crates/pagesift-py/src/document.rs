//! 抓取结果。结果以 Rust 值保存，字段第一次被访问时才转换成 Python 对象：
//! 不访问的大字段（raw_html 等）不产生拷贝，失败项的异常对象也是访问 `error` 时才创建。
//! 字符串字段转换后缓存，重复访问不再拷贝。

use pagesift::api::ApiError;
use pagesift::api::types::{self, Metadata, PagesiftMeta};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyString};
use serde_json::Value;

use crate::engine::Outcome;
use crate::errors::{exception, json};

type StrCache = PyOnceLock<Option<Py<PyString>>>;

#[pyclass(frozen, module = "pagesift", name = "Document")]
pub struct Document {
    index: Option<usize>,
    url: Option<String>,
    doc: Option<types::Document>,
    err: Option<ApiError>,
    /// `convert` 的结果沿用原始 meta 键名（`og:image`），抓取结果用 snake_case（`og_image`）。
    raw_meta_keys: bool,
    markdown_py: StrCache,
    html_py: StrCache,
    raw_html_py: StrCache,
    error_py: PyOnceLock<Py<PyAny>>,
}

impl From<Outcome> for Document {
    fn from(o: Outcome) -> Self {
        let (doc, err) = match o.result {
            Ok(d) => (Some(d), None),
            Err(e) => (None, Some(e)),
        };
        Self::new(o.index, Some(o.url), doc, err, false)
    }
}

impl Document {
    fn new(
        index: Option<usize>,
        url: Option<String>,
        doc: Option<types::Document>,
        err: Option<ApiError>,
        raw_meta_keys: bool,
    ) -> Self {
        Self {
            index,
            url,
            doc,
            err,
            raw_meta_keys,
            markdown_py: PyOnceLock::new(),
            html_py: PyOnceLock::new(),
            raw_html_py: PyOnceLock::new(),
            error_py: PyOnceLock::new(),
        }
    }

    pub fn position(&self) -> Option<usize> {
        self.index
    }

    /// `convert` 的结果：只有 markdown 和元数据，元数据键名和以前一样。
    pub fn converted(doc: pagesift_content::Document) -> Self {
        let m = doc.metadata;
        let mut extra = std::collections::BTreeMap::new();
        for (k, v) in [("author", m.author), ("canonical_url", m.canonical_url)] {
            if let Some(v) = v {
                extra.insert(k.to_string(), Value::String(v));
            }
        }
        for (k, v) in m.extra {
            extra.insert(k, Value::String(v));
        }
        let d = types::Document {
            markdown: Some(doc.markdown),
            metadata: Metadata {
                title: m.title,
                description: m.description,
                language: m.language,
                keywords: m.keywords,
                robots: m.robots,
                favicon: m.favicon,
                published_time: m.published_time,
                modified_time: m.modified_time,
                extra,
                ..Default::default()
            },
            ..Default::default()
        };
        Self::new(None, None, Some(d), None, true)
    }

    fn meta(&self) -> Option<&Metadata> {
        self.doc.as_ref().map(|d| &d.metadata)
    }

    fn ext(&self) -> Option<&PagesiftMeta> {
        self.meta().and_then(|m| m.pagesift.as_ref())
    }

    fn scraped(&self) -> bool {
        self.doc.is_some() && !self.raw_meta_keys
    }
}

fn cached<'py>(
    py: Python<'py>,
    cell: &StrCache,
    s: Option<&String>,
) -> Option<Bound<'py, PyString>> {
    cell.get_or_init(py, || s.map(|s| PyString::new(py, s).unbind()))
        .as_ref()
        .map(|p| p.bind(py).clone())
}

/// `ogImage` → `og_image`、`numPages` → `num_pages`。
fn snake_case(k: &str) -> String {
    let mut out = String::with_capacity(k.len() + 4);
    for c in k.chars() {
        if c.is_ascii_uppercase() {
            if !out.is_empty() {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[pymethods]
impl Document {
    /// 在批量输入里的位置；单页抓取和 `convert` 为 None。
    #[getter]
    fn index(&self) -> Option<usize> {
        self.index
    }
    /// 请求的地址。
    #[getter]
    fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }
    /// 跟随重定向后的最终地址。
    #[getter]
    fn final_url(&self) -> Option<&str> {
        self.meta().and_then(|m| m.url.as_deref())
    }
    #[getter]
    fn success(&self) -> bool {
        self.err.is_none()
    }
    #[getter]
    fn status_code(&self) -> Option<u16> {
        self.scraped()
            .then(|| self.meta().map(|m| m.status_code))
            .flatten()
    }
    #[getter]
    fn markdown<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyString>> {
        let s = self.doc.as_ref().and_then(|d| d.markdown.as_ref());
        cached(py, &self.markdown_py, s)
    }
    /// 清理后的 HTML（`formats` 里有 `html` 时才有）。
    #[getter]
    fn html<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyString>> {
        let s = self.doc.as_ref().and_then(|d| d.html.as_ref());
        cached(py, &self.html_py, s)
    }
    /// 原始 HTML（`formats` 里有 `raw_html` 时才有）。
    #[getter]
    fn raw_html<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyString>> {
        let s = self.doc.as_ref().and_then(|d| d.raw_html.as_ref());
        cached(py, &self.raw_html_py, s)
    }
    #[getter]
    fn links(&self) -> Option<Vec<&str>> {
        let l = self.doc.as_ref()?.links.as_ref()?;
        Some(l.iter().map(String::as_str).collect())
    }
    #[getter]
    fn images(&self) -> Option<Vec<&str>> {
        let l = self.doc.as_ref()?.images.as_ref()?;
        Some(l.iter().map(String::as_str).collect())
    }
    #[getter]
    fn metadata<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        let Some(m) = self.meta() else { return Ok(d) };
        for (k, v) in [
            ("title", &m.title),
            ("description", &m.description),
            ("language", &m.language),
            ("keywords", &m.keywords),
            ("robots", &m.robots),
            ("favicon", &m.favicon),
            ("published_time", &m.published_time),
            ("modified_time", &m.modified_time),
            ("content_type", &m.content_type),
        ] {
            if let Some(v) = v {
                d.set_item(k, v)?;
            }
        }
        for (k, v) in &m.extra {
            let v = json(py, v)?;
            if self.raw_meta_keys {
                d.set_item(k, v)?;
            } else {
                d.set_item(snake_case(k), v)?;
            }
        }
        Ok(d)
    }
    /// 失败原因（`PagesiftError` 子类的实例），成功时为 None。
    #[getter]
    fn error(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let Some(e) = &self.err else { return Ok(None) };
        if let Some(obj) = self.error_py.get(py) {
            return Ok(Some(obj.clone_ref(py)));
        }
        let obj = exception(py, e, self.url.as_deref())?;
        Ok(Some(self.error_py.get_or_init(py, || obj).clone_ref(py)))
    }
    #[getter]
    fn error_code(&self) -> Option<&'static str> {
        self.err.as_ref().map(|e| e.code.as_str())
    }
    #[getter]
    fn error_message(&self) -> Option<&str> {
        self.err.as_ref().map(|e| e.message.as_str())
    }
    /// 实际使用的引擎：`http` 或 `browser`。
    #[getter]
    fn engine(&self) -> Option<&str> {
        self.ext()?.engine.as_deref()
    }
    /// Markdown 的 BLAKE3 哈希（十六进制），用来判断内容是否变化。
    #[getter]
    fn content_hash(&self) -> Option<&str> {
        self.ext()?.content_hash.as_deref()
    }
    #[getter]
    fn raw_hash(&self) -> Option<&str> {
        self.ext()?.raw_hash.as_deref()
    }
    #[getter]
    fn text_hash(&self) -> Option<&str> {
        self.ext()?.text_hash.as_deref()
    }
    /// 总耗时（毫秒）。
    #[getter]
    fn elapsed_ms(&self) -> Option<f64> {
        self.ext()?.timings.get("total").copied()
    }
    /// 各阶段耗时（毫秒）：fetch、render、extract、pdf、total。
    #[getter]
    fn timings(&self) -> std::collections::BTreeMap<String, f64> {
        self.ext().map(|e| e.timings.clone()).unwrap_or_default()
    }
    #[getter]
    fn warnings(&self) -> Vec<&str> {
        self.ext()
            .map(|e| e.warnings.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }
    #[getter]
    fn redirect_chain(&self) -> Vec<&str> {
        self.ext()
            .map(|e| e.redirect_chain.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }
    #[getter]
    fn etag(&self) -> Option<&str> {
        self.ext()?.etag.as_deref()
    }
    #[getter]
    fn last_modified(&self) -> Option<&str> {
        self.ext()?.last_modified.as_deref()
    }
    #[getter]
    fn not_modified(&self) -> bool {
        self.ext().is_some_and(|e| e.not_modified)
    }

    /// 全部字段转成 dict（会转换所有字段，只要部分字段时直接访问属性更快）。
    fn to_dict<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyDict>> {
        let py = slf.py();
        let d = PyDict::new(py);
        for name in FIELDS {
            d.set_item(*name, slf.getattr(*name)?)?;
        }
        match &slf.get().err {
            Some(e) => {
                let err = PyDict::new(py);
                err.set_item("code", e.code.as_str())?;
                err.set_item("message", &e.message)?;
                let details = e.details.as_ref().map(|v| json(py, v)).transpose()?;
                err.set_item(
                    "details",
                    details.unwrap_or_else(|| PyDict::new(py).into_any()),
                )?;
                err.set_item("retry_after", e.retry_after)?;
                d.set_item("error", err)?;
            }
            None => d.set_item("error", py.None())?,
        }
        Ok(d)
    }

    fn __repr__(&self) -> String {
        let url = self.url.as_deref().unwrap_or("");
        if let Some(e) = &self.err {
            return format!("<pagesift.Document url={url:?} error={}>", e.code.as_str());
        }
        let title = self.meta().and_then(|m| m.title.as_deref()).unwrap_or("");
        let chars = self
            .doc
            .as_ref()
            .and_then(|d| d.markdown.as_deref())
            .map_or(0, |m| m.chars().count());
        format!("<pagesift.Document url={url:?} title={title:?} markdown={chars} chars>")
    }
}

/// `to_dict()` 里的字段（`error` 单独处理）。
const FIELDS: &[&str] = &[
    "index",
    "url",
    "final_url",
    "success",
    "status_code",
    "markdown",
    "html",
    "raw_html",
    "links",
    "images",
    "metadata",
    "error_code",
    "error_message",
    "engine",
    "content_hash",
    "raw_hash",
    "text_hash",
    "elapsed_ms",
    "timings",
    "warnings",
    "redirect_chain",
    "etag",
    "last_modified",
    "not_modified",
];
