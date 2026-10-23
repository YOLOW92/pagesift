//! Python 包 `pagesift` 的原生部分（`pagesift._pagesift`），Python 层在 `python/pagesift`。
//!
//! 这个 crate 不写 `#![forbid(unsafe_code)]`：PyO3 的宏会生成 unsafe 代码（PRD 第 7 节规定绑定层是唯一的例外）。
//! 我们自己的代码里不写 unsafe。抓取和处理都在 Rust 里完成，期间释放 GIL。

mod document;
mod engine;
mod errors;
mod options;

#[pyo3::pymodule]
mod _pagesift {
    use pyo3::prelude::*;

    #[pymodule_export]
    use crate::document::Document;
    #[pymodule_export]
    use crate::engine::{Batch, Engine};
    #[pymodule_export]
    use crate::options::Options;

    /// 把一段 HTML 转成干净的 Markdown。
    ///
    /// - `url`：页面地址，用来把相对链接转成绝对地址；
    /// - `only_main_content`：只保留正文（去掉导航、页眉页脚、侧边栏等），默认 True。
    ///
    /// 处理期间释放 GIL。
    #[pyfunction]
    #[pyo3(signature = (html, url=None, only_main_content=true))]
    fn convert(
        py: Python<'_>,
        html: &str,
        url: Option<&str>,
        only_main_content: bool,
    ) -> PyResult<Document> {
        let base_url = match url {
            Some(u) => Some(url::Url::parse(u).map_err(|e| {
                crate::errors::to_py_err(
                    &pagesift::api::ApiError::invalid(format!("invalid url {u:?}: {e}")),
                    None,
                )
            })?),
            None => None,
        };
        let opts = pagesift_content::Options {
            base_url,
            only_main_content,
            ..Default::default()
        };
        let html = html.to_owned();
        let doc = py.detach(move || pagesift_content::process(&html, &opts));
        Ok(Document::converted(doc))
    }

    /// 启动 pagesift MCP 服务器。
    #[pyfunction]
    #[pyo3(signature = (transport="stdio", port=3001))]
    fn run_mcp(py: Python<'_>, transport: &str, port: u16) -> PyResult<()> {
        let sc = pagesift::ScraperConfig::default();
        let scraper = std::sync::Arc::new(
            pagesift::Scraper::new(sc)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?,
        );
        let transport = transport.to_string();
        py.detach(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
            rt.block_on(async move {
                let res = pagesift::mcp::run_server(scraper.clone(), &transport, port).await;
                scraper.shutdown().await;
                res.map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
            })
        })
    }

    #[pymodule_init]
    fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
        m.add("__version__", env!("CARGO_PKG_VERSION"))
    }
}
