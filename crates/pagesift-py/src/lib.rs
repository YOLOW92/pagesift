//! Python 包 `pagesift`：Rust 核心外面的一层薄包装。
//!
//! 这个 crate 不写 `#![forbid(unsafe_code)]`：PyO3 的宏会生成 unsafe 代码（PRD 第 7 节规定绑定层是唯一的例外）。
//! 我们自己的代码里不写 unsafe。处理 HTML 时释放 GIL，Python 里用普通线程池就能用满多核。

#[pyo3::pymodule]
mod pagesift {
    use pyo3::prelude::*;
    use pyo3::types::PyDict;

    /// 转换结果：`markdown` 和 `metadata`（dict）。
    #[pyclass(frozen, module = "pagesift")]
    pub struct Document {
        #[pyo3(get)]
        markdown: String,
        meta: pagesift_content::Metadata,
    }

    #[pymethods]
    impl Document {
        #[getter]
        fn metadata<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
            let d = PyDict::new(py);
            let m = &self.meta;
            for (k, v) in [
                ("title", &m.title),
                ("description", &m.description),
                ("language", &m.language),
                ("keywords", &m.keywords),
                ("robots", &m.robots),
                ("author", &m.author),
                ("canonical_url", &m.canonical_url),
                ("favicon", &m.favicon),
                ("published_time", &m.published_time),
                ("modified_time", &m.modified_time),
            ] {
                if let Some(v) = v {
                    d.set_item(k, v)?;
                }
            }
            for (k, v) in &m.extra {
                d.set_item(k, v)?;
            }
            Ok(d)
        }

        fn __repr__(&self) -> String {
            let title = self.meta.title.as_deref().unwrap_or("");
            format!("<pagesift.Document title={title:?} markdown={} chars>", self.markdown.chars().count())
        }
    }

    /// 把一段 HTML 转成干净的 Markdown。
    ///
    /// - `url`：页面地址，用来把相对链接转成绝对地址；
    /// - `only_main_content`：只保留正文（去掉导航、页眉页脚、侧边栏等），默认 True。
    ///
    /// 处理期间释放 GIL。
    #[pyfunction]
    #[pyo3(signature = (html, url=None, only_main_content=true))]
    fn convert(py: Python<'_>, html: &str, url: Option<&str>, only_main_content: bool) -> PyResult<Document> {
        let base_url = match url {
            Some(u) => Some(
                url::Url::parse(u)
                    .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid url {u:?}: {e}")))?,
            ),
            None => None,
        };
        let opts = pagesift_content::Options {
            base_url,
            only_main_content,
        };
        let html = html.to_owned();
        let doc = py.detach(move || pagesift_content::process(&html, &opts));
        Ok(Document {
            markdown: doc.markdown,
            meta: doc.metadata,
        })
    }

    #[pymodule_init]
    fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
        m.add("__version__", env!("CARGO_PKG_VERSION"))
    }
}
