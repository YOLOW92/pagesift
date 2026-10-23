//! 持有一个 `pagesift::Scraper`（连接池、礼貌策略状态、浏览器池都在里面），供同步和异步接口共用。
//!
//! 抓取全部跑在共享的 tokio 运行时上：同步接口等待期间释放 GIL，批量接口的并发也在 Rust 侧完成。
//! 结果先以 Rust 值保存，Python 访问字段时才转换（见 document.rs）。

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pagesift::api::ApiError;
use pagesift::api::types::{self, ScrapeOptions};
use pagesift::{Scraper, ScraperConfig};
use pagesift_cdp::BrowserSource;
use pyo3::exceptions::{PyStopAsyncIteration, PyValueError};
use pyo3::prelude::*;
use tokio::runtime::Runtime;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;

use crate::document::Document;
use crate::errors::to_py_err;
use crate::options::{Options, check_url};

pub struct Outcome {
    pub index: Option<usize>,
    pub url: String,
    pub result: Result<types::Document, ApiError>,
}

fn runtime() -> &'static Runtime {
    pyo3_async_runtimes::tokio::get_runtime()
}

#[pyclass(frozen, module = "pagesift._pagesift")]
pub struct Engine {
    scraper: Arc<Scraper>,
}

#[pymethods]
impl Engine {
    #[new]
    #[pyo3(signature = (
        *,
        user_agent = None,
        allow_private_network = false,
        allow_cidrs = None,
        proxy = None,
        request_interval = 1.0,
        per_host_concurrency = 2,
        max_concurrency = 64,
        browser_executable = None,
        browser_url = None,
        max_browsers = 2,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        user_agent: Option<String>,
        allow_private_network: bool,
        allow_cidrs: Option<Vec<String>>,
        proxy: Option<String>,
        request_interval: f64,
        per_host_concurrency: usize,
        max_concurrency: usize,
        browser_executable: Option<PathBuf>,
        browser_url: Option<String>,
        max_browsers: usize,
    ) -> PyResult<Self> {
        if !(request_interval.is_finite() && request_interval >= 0.0) {
            return Err(PyValueError::new_err(
                "request_interval must be >= 0 seconds",
            ));
        }
        if per_host_concurrency == 0 || max_concurrency == 0 || max_browsers == 0 {
            return Err(PyValueError::new_err(
                "concurrency limits must be at least 1",
            ));
        }
        let mut cfg = ScraperConfig::default();
        if let Some(ua) = user_agent {
            cfg.fetcher.user_agent = ua;
        }
        cfg.fetcher.proxy = proxy;
        cfg.fetcher.ssrf.allow_private_network = allow_private_network;
        if let Some(cidrs) = allow_cidrs {
            for c in cidrs {
                match c.parse() {
                    Ok(parsed) => cfg.fetcher.ssrf.allow_cidrs.push(parsed),
                    Err(e) => {
                        return Err(PyValueError::new_err(format!(
                            "invalid CIDR {c:?}: {e}"
                        )));
                    }
                }
            }
        }
        cfg.policy.rate.default_interval = Duration::from_secs_f64(request_interval);
        cfg.policy.rate.per_host_concurrency = per_host_concurrency;
        cfg.policy.rate.global_concurrency = max_concurrency;
        cfg.browser.max_browsers = max_browsers;
        cfg.browser.source = match (browser_executable, browser_url) {
            (Some(_), Some(_)) => {
                return Err(PyValueError::new_err(
                    "pass either browser_executable or browser_url, not both",
                ));
            }
            (Some(p), None) => BrowserSource::Executable(p),
            (None, Some(u)) => BrowserSource::Remote(u),
            (None, None) => BrowserSource::Auto,
        };
        let _guard = runtime().enter();
        let scraper =
            Scraper::new(cfg).map_err(|e| to_py_err(&ApiError::invalid(e.to_string()), None))?;
        Ok(Self {
            scraper: Arc::new(scraper),
        })
    }

    /// 抓一个页面，失败时抛出对应的异常。
    fn scrape(&self, py: Python<'_>, url: String, options: &Options) -> PyResult<Document> {
        let scraper = self.scraper.clone();
        let opts = options.inner.clone();
        let out = block_on(
            py,
            async move { scrape_one(&scraper, None, url, &opts).await },
        )?;
        raise_or_document(out)
    }

    fn ascrape<'py>(
        &self,
        py: Python<'py>,
        url: String,
        options: &Options,
    ) -> PyResult<Bound<'py, PyAny>> {
        let scraper = self.scraper.clone();
        let opts = options.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            raise_or_document(scrape_one(&scraper, None, url, &opts).await)
        })
    }

    /// 批量抓取，全部完成后按输入顺序返回；单个失败不影响其他项。
    fn scrape_many(
        &self,
        py: Python<'_>,
        urls: Vec<String>,
        options: &Options,
        concurrency: usize,
    ) -> PyResult<Vec<Document>> {
        let n = urls.len();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let tasks = spawn_all(&self.scraper, urls, &options.inner, concurrency, tx)?;
        let mut out = block_on(py, async move {
            let mut out = Vec::with_capacity(n);
            while let Some(o) = rx.recv().await {
                out.push(Document::from(o));
            }
            drop(tasks);
            out
        })?;
        out.sort_unstable_by_key(|d| d.position());
        Ok(out)
    }

    /// 立即开始批量抓取，返回按完成顺序产出结果的 [`Batch`]（同步、异步迭代都可以）。
    fn stream(&self, urls: Vec<String>, options: &Options, concurrency: usize) -> PyResult<Batch> {
        let total = urls.len();
        let (tx, rx) = mpsc::unbounded_channel();
        let tasks = spawn_all(&self.scraper, urls, &options.inner, concurrency, tx)?;
        Ok(Batch {
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
            tasks: Mutex::new(Some(tasks)),
            total,
        })
    }

    /// 关闭浏览器池。连接池随对象释放。
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let scraper = self.scraper.clone();
        block_on(py, async move { scraper.shutdown().await })
    }

    fn aclose<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let scraper = self.scraper.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            scraper.shutdown().await;
            Ok(())
        })
    }
}

/// 进行中的批量抓取，按完成顺序产出 `Document`。`for` 和 `async for` 都可以用，只能迭代一遍。
#[pyclass(frozen, module = "pagesift._pagesift")]
pub struct Batch {
    rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Outcome>>>,
    /// 对象被丢弃或调用 `cancel()` 时，`JoinSet` 跟着释放，未完成的抓取全部取消。
    tasks: Mutex<Option<JoinSet<()>>>,
    total: usize,
}

#[pymethods]
impl Batch {
    fn __len__(&self) -> usize {
        self.total
    }

    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// 等待下一个完成的结果，期间释放 GIL。
    fn __next__(&self, py: Python<'_>) -> PyResult<Option<Document>> {
        let rx = self.rx.clone();
        let next = block_on(py, async move { rx.lock().await.recv().await })?;
        Ok(next.map(Document::from))
    }

    fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = self.rx.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            match rx.lock().await.recv().await {
                Some(o) => Ok(Document::from(o)),
                None => Err(PyStopAsyncIteration::new_err(())),
            }
        })
    }

    /// 取消还没完成的抓取；已经完成的结果仍然可以取出。
    fn cancel(&self) {
        if let Ok(mut t) = self.tasks.lock() {
            t.take();
        }
    }
}

async fn scrape_one(
    scraper: &Scraper,
    index: Option<usize>,
    url: String,
    opts: &ScrapeOptions,
) -> Outcome {
    let result = match check_url(&url) {
        Ok(()) => scraper.scrape(&url, opts).await,
        Err(e) => Err(e),
    };
    Outcome { index, url, result }
}

fn raise_or_document(o: Outcome) -> PyResult<Document> {
    match &o.result {
        Err(e) => Err(to_py_err(e, Some(&o.url))),
        Ok(_) => Ok(Document::from(o)),
    }
}

/// 每个 URL 一个任务，信号量控制同时进行的数量；结果按完成顺序送进 `tx`。
fn spawn_all(
    scraper: &Arc<Scraper>,
    urls: Vec<String>,
    opts: &Arc<ScrapeOptions>,
    concurrency: usize,
    tx: mpsc::UnboundedSender<Outcome>,
) -> PyResult<JoinSet<()>> {
    if concurrency == 0 {
        return Err(PyValueError::new_err("concurrency must be at least 1"));
    }
    let sem = Arc::new(Semaphore::new(concurrency));
    let mut set = JoinSet::new();
    for (i, url) in urls.into_iter().enumerate() {
        let (scraper, opts, sem, tx) = (scraper.clone(), opts.clone(), sem.clone(), tx.clone());
        set.spawn_on(
            async move {
                let Ok(_permit) = sem.acquire().await else {
                    return;
                };
                let _ = tx.send(scrape_one(&scraper, Some(i), url, &opts).await);
            },
            runtime().handle(),
        );
    }
    Ok(set)
}

/// 在运行时上等待 `fut`，期间释放 GIL；每 100ms 检查一次 Ctrl-C，收到就取消。
fn block_on<F>(py: Python<'_>, fut: F) -> PyResult<F::Output>
where
    F: Future + Send,
    F::Output: Send,
{
    py.detach(|| {
        runtime().block_on(async {
            tokio::pin!(fut);
            loop {
                tokio::select! {
                    r = &mut fut => return Ok(r),
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        Python::attach(|py| py.check_signals())?;
                    }
                }
            }
        })
    })
}
