//! 持有一个 `pagesift::Scraper`（连接池、礼貌策略状态、浏览器池都在里面），供同步和异步接口共用。
//!
//! 抓取全部跑在共享的 tokio 运行时上：同步接口等待期间释放 GIL，批量接口的并发也在 Rust 侧完成。
//! 结果先以 Rust 值保存，Python 访问字段时才转换（见 document.rs）。

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pagesift::api::ApiError;
use pagesift::api::types::{self, ScrapeOptions};
use pagesift::{Scraper, ScraperConfig};
use pagesift_cdp::BrowserSource;
use pyo3::exceptions::{PyStopAsyncIteration, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use tokio::runtime::Runtime;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;

use crate::crawl::{self, Crawl, CrawlArgs, MapArgs};
use crate::document::Document;
use crate::errors::to_py_err;
use crate::options::{Options, check_url};
use crate::parse::{self, FileInput};

pub struct Outcome {
    pub index: Option<usize>,
    pub url: String,
    pub result: Result<types::Document, ApiError>,
}

pub(crate) fn runtime() -> &'static Runtime {
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
        request_interval_ms = 1000,
        per_host_concurrency = 2,
        max_concurrency = 64,
        browser_executable = None,
        browser_url = None,
        max_browsers = 2,
        store_path = None,
        ocr = false,
        ocr_quality = None,
        ocr_formulas = false,
        models_dir = None,
        gpu = None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        user_agent: Option<String>,
        allow_private_network: bool,
        allow_cidrs: Option<Vec<String>>,
        proxy: Option<String>,
        request_interval_ms: i64,
        per_host_concurrency: usize,
        max_concurrency: usize,
        browser_executable: Option<PathBuf>,
        browser_url: Option<String>,
        max_browsers: usize,
        store_path: Option<PathBuf>,
        ocr: bool,
        ocr_quality: Option<String>,
        ocr_formulas: bool,
        models_dir: Option<PathBuf>,
        gpu: Option<String>,
    ) -> PyResult<Self> {
        let Ok(request_interval_ms) = u64::try_from(request_interval_ms) else {
            return Err(PyValueError::new_err("request_interval_ms must be >= 0"));
        };
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
                        return Err(PyValueError::new_err(format!("invalid CIDR {c:?}: {e}")));
                    }
                }
            }
        }
        cfg.policy.rate.default_interval = Duration::from_millis(request_interval_ms);
        cfg.policy.rate.per_host_concurrency = per_host_concurrency;
        // `max_concurrency` 是用户给的上限：在线调整只在它之下（冷启动就是它，和 0.81 一样）
        cfg.policy.rate.global_concurrency = max_concurrency;
        cfg.policy.rate.global_ceiling = Some(max_concurrency);
        cfg.browser.max_browsers = max_browsers;
        if let Some(dir) = store_path {
            cfg.store = Some(pagesift::store::Store::open_dir(&dir).map_err(|e| {
                PyValueError::new_err(format!("opening the store in {}: {e}", dir.display()))
            })?);
        }
        cfg.ocr = pagesift::config::OcrOptions {
            enabled: ocr,
            quality: ocr_quality,
            formulas: ocr_formulas,
            models_dir,
            gpu,
        }
        .engine(&cfg.fetcher)
        .map_err(|e| PyValueError::new_err(format!("{e:#}")))?;
        // 用库的人就是管理员（和命令行一样）：crawl、map 的 ignore_robots_txt 由调用方自己决定
        cfg.policy.allow_crawl_ignore_robots = true;
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
        Ok(Batch::new(rx, tasks, total))
    }

    /// 整站爬取，返回按完成顺序产出页面的 [`Crawl`]（同步、异步迭代都可以）。
    #[pyo3(signature = (
        url,
        options,
        *,
        limit = None,
        max_depth = None,
        include_paths = None,
        exclude_paths = None,
        crawl_entire_domain = false,
        allow_subdomains = None,
        sitemap = "include".to_string(),
        query = None,
        adaptive = false,
        max_concurrency = None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn crawl(
        &self,
        url: String,
        options: &Options,
        limit: Option<usize>,
        max_depth: Option<usize>,
        include_paths: Option<Vec<String>>,
        exclude_paths: Option<Vec<String>>,
        crawl_entire_domain: bool,
        allow_subdomains: Option<bool>,
        sitemap: String,
        query: Option<String>,
        adaptive: bool,
        max_concurrency: Option<usize>,
    ) -> PyResult<Crawl> {
        let args = CrawlArgs {
            limit,
            max_depth,
            include_paths,
            exclude_paths,
            crawl_entire_domain,
            allow_subdomains,
            sitemap,
            query,
            adaptive,
            max_concurrency,
        };
        crawl::start(&self.scraper, url, &options.inner, args)
    }

    /// 站点的链接列表（sitemap 加页面上的链接），每项是 `{"url", "title", "description"}`。
    #[pyo3(signature = (url, *, limit = None, search = None, sitemap = "include".to_string(), ignore_robots_txt = false))]
    fn map<'py>(
        &self,
        py: Python<'py>,
        url: String,
        limit: Option<usize>,
        search: Option<String>,
        sitemap: String,
        ignore_robots_txt: bool,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let args = MapArgs {
            limit,
            search,
            sitemap,
            ignore_robots_txt,
        };
        let req = crawl::map_request(url, args).map_err(|e| to_py_err(&e, None))?;
        crawl::map_blocking(py, &self.scraper, req)
    }

    #[pyo3(signature = (url, *, limit = None, search = None, sitemap = "include".to_string(), ignore_robots_txt = false))]
    fn amap<'py>(
        &self,
        py: Python<'py>,
        url: String,
        limit: Option<usize>,
        search: Option<String>,
        sitemap: String,
        ignore_robots_txt: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let args = MapArgs {
            limit,
            search,
            sitemap,
            ignore_robots_txt,
        };
        let req = crawl::map_request(url, args).map_err(|e| to_py_err(&e, None))?;
        let scraper = self.scraper.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let links = scraper.map(&req).await.map_err(|e| to_py_err(&e, None))?;
            Python::attach(|py| {
                crawl::links_to_py(py, links)
                    .map(|l| l.into_iter().map(Bound::unbind).collect::<Vec<_>>())
            })
        })
    }

    /// 解析一个文件（PDF、HTML、Office 文档等），失败时抛出对应的异常。
    #[pyo3(signature = (data, filename, options, *, content_type = None, url = None))]
    fn parse(
        &self,
        py: Python<'_>,
        data: &Bound<'_, PyBytes>,
        filename: String,
        options: &Options,
        content_type: Option<String>,
        url: Option<String>,
    ) -> PyResult<Document> {
        let file = file_input(data, filename, content_type, url);
        let opts = options.inner.clone();
        parse::check_options(&self.scraper, &opts).map_err(|e| to_py_err(&e, None))?;
        let scraper = self.scraper.clone();
        let out = block_on(py, async move {
            parse::parse_one(&scraper, None, file, &opts).await
        })?;
        raise_or_document(out)
    }

    #[pyo3(signature = (data, filename, options, *, content_type = None, url = None))]
    fn aparse<'py>(
        &self,
        py: Python<'py>,
        data: &Bound<'_, PyBytes>,
        filename: String,
        options: &Options,
        content_type: Option<String>,
        url: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let file = file_input(data, filename, content_type, url);
        let opts = options.inner.clone();
        parse::check_options(&self.scraper, &opts).map_err(|e| to_py_err(&e, None))?;
        let scraper = self.scraper.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            raise_or_document(parse::parse_one(&scraper, None, file, &opts).await)
        })
    }

    /// 立即开始批量解析，返回按完成顺序产出结果的 [`Batch`]；`files` 是 `(字节, 文件名)` 的列表。
    fn parse_stream(
        &self,
        files: Vec<(Bound<'_, PyBytes>, String)>,
        options: &Options,
        concurrency: usize,
    ) -> PyResult<Batch> {
        parse::check_options(&self.scraper, &options.inner).map_err(|e| to_py_err(&e, None))?;
        let total = files.len();
        let inputs: Vec<FileInput> = files
            .into_iter()
            .map(|(data, name)| file_input(&data, name, None, None))
            .collect();
        let (tx, rx) = mpsc::unbounded_channel();
        let (scraper, opts) = (self.scraper.clone(), options.inner.clone());
        let tasks = spawn_jobs(inputs, concurrency, tx, move |i, f| {
            let (scraper, opts) = (scraper.clone(), opts.clone());
            async move { parse::parse_one(&scraper, Some(i), f, &opts).await }
        })?;
        Ok(Batch::new(rx, tasks, total))
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
    /// `cancel()` 之后迭代立即结束，取消前已完成、还没取出的结果一并丢弃。
    cancelled: Arc<AtomicBool>,
    total: usize,
}

/// 取下一个结果；已取消时不再产出（通道里排着的也不要）。
async fn next_outcome(
    rx: &tokio::sync::Mutex<mpsc::UnboundedReceiver<Outcome>>,
    cancelled: &AtomicBool,
) -> Option<Outcome> {
    if cancelled.load(Ordering::Acquire) {
        return None;
    }
    let next = rx.lock().await.recv().await;
    next.filter(|_| !cancelled.load(Ordering::Acquire))
}

impl Batch {
    fn new(rx: mpsc::UnboundedReceiver<Outcome>, tasks: JoinSet<()>, total: usize) -> Self {
        Self {
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
            tasks: Mutex::new(Some(tasks)),
            cancelled: Arc::new(AtomicBool::new(false)),
            total,
        }
    }
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
        let (rx, cancelled) = (self.rx.clone(), self.cancelled.clone());
        let next = block_on(py, async move { next_outcome(&rx, &cancelled).await })?;
        Ok(next.map(Document::from))
    }

    fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let (rx, cancelled) = (self.rx.clone(), self.cancelled.clone());
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            match next_outcome(&rx, &cancelled).await {
                Some(o) => Ok(Document::from(o)),
                None => Err(PyStopAsyncIteration::new_err(())),
            }
        })
    }

    /// 取消整批：还没完成的抓取全部取消，之后迭代立即结束；取消前已完成、还没取出的结果也丢弃
    /// （否则 `cancel()` 之后还会冒出几条结果，取决于取消那一刻恰好有几条刚完成）。
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Ok(mut t) = self.tasks.lock() {
            t.take();
        }
        // 没有人正在等结果时顺手清空通道；正在等的那一方醒来后看到取消标记，也不会产出。
        if let Ok(mut rx) = self.rx.try_lock() {
            rx.close();
            while rx.try_recv().is_ok() {}
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

fn file_input(
    data: &Bound<'_, PyBytes>,
    filename: String,
    content_type: Option<String>,
    url: Option<String>,
) -> FileInput {
    FileInput {
        bytes: data.as_bytes().to_vec(),
        filename,
        content_type,
        url,
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
    let (scraper, opts) = (scraper.clone(), opts.clone());
    spawn_jobs(urls, concurrency, tx, move |i, url| {
        let (scraper, opts) = (scraper.clone(), opts.clone());
        async move { scrape_one(&scraper, Some(i), url, &opts).await }
    })
}

/// 每一项一个任务（第 `i` 项交给 `job(i, item)`），信号量控制同时进行的数量；结果按完成顺序送进 `tx`。
fn spawn_jobs<T, F, Fut>(
    items: Vec<T>,
    concurrency: usize,
    tx: mpsc::UnboundedSender<Outcome>,
    job: F,
) -> PyResult<JoinSet<()>>
where
    T: Send + 'static,
    F: Fn(usize, T) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Outcome> + Send + 'static,
{
    if concurrency == 0 {
        return Err(PyValueError::new_err("concurrency must be at least 1"));
    }
    let sem = Arc::new(Semaphore::new(concurrency));
    let job = Arc::new(job);
    let mut set = JoinSet::new();
    for (i, item) in items.into_iter().enumerate() {
        let (job, sem, tx) = (job.clone(), sem.clone(), tx.clone());
        set.spawn_on(
            async move {
                let Ok(_permit) = sem.acquire().await else {
                    return;
                };
                let _ = tx.send(job(i, item).await);
            },
            runtime().handle(),
        );
    }
    Ok(set)
}

/// 在运行时上等待 `fut`，期间释放 GIL；每 100ms 检查一次 Ctrl-C，收到就取消。
pub(crate) fn block_on<F>(py: Python<'_>, fut: F) -> PyResult<F::Output>
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
