//! 整站爬取（`Scraper.crawl`）和站点链接列表（`Scraper.map`）。
//!
//! 爬取在运行时上的一个任务里进行，结果经有界通道交给 [`Crawl`] 迭代器：消费方慢时通道满，
//! 爬取在交付页面处等待（背压），不丢页。迭代器被丢弃或调用 `cancel()` 后，`CrawlSink::cancelled`
//! 返回 true，引擎在下一轮调度前停止。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pagesift::Scraper;
use pagesift::api::types::{CrawlRequest, CrawlSummary, Document as ApiDocument, MapRequest};
use pagesift::api::{
    ApiError, BoxFuture, CrawlSink, MAX_CRAWL_DEPTH, MAX_CRAWL_LIMIT, MAX_MAP_LIMIT,
    check_scrape_options, check_url,
};
use pyo3::exceptions::PyStopAsyncIteration;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::document::Document;
use crate::engine::{Outcome, block_on, runtime};
use crate::errors::to_py_err;

/// 通道里的消息：一页（成功或失败），或者爬取结束（引擎的返回值）。
enum Event {
    Page(Box<Outcome>),
    Done(Result<(), ApiError>),
}

/// 爬取任务和迭代器共享的状态。
#[derive(Default)]
struct Shared {
    cancelled: AtomicBool,
    pages: AtomicUsize,
    errors: AtomicUsize,
    total: AtomicUsize,
    /// 带 `query` 的爬取由引擎给出；普通爬取在结束时由 [`Shared::finish`] 按页数补上。
    summary: Mutex<Option<CrawlSummary>>,
}

impl Shared {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// 引擎没给结束说明（广度优先爬取不给）时，按取消标记和完成页数推断停止原因。
    fn finish(&self, limit: usize) {
        let Ok(mut s) = self.summary.lock() else {
            return;
        };
        if s.is_some() {
            return;
        }
        let pages = self.pages.load(Ordering::Acquire);
        let done = pages + self.errors.load(Ordering::Acquire);
        let stop_reason = if self.is_cancelled() {
            "cancelled"
        } else if done >= limit {
            "limit_reached"
        } else {
            "exhausted"
        };
        *s = Some(CrawlSummary {
            stop_reason: stop_reason.into(),
            coverage: None,
            term_evidence: Default::default(),
            pages,
        });
    }
}

/// 把引擎的回报转成通道消息。
struct ChannelSink {
    tx: mpsc::Sender<Event>,
    shared: Arc<Shared>,
}

impl ChannelSink {
    async fn deliver(&self, o: Outcome) {
        if self.shared.is_cancelled() {
            return;
        }
        // 消费方已经不在（迭代器被丢弃）：当作取消
        if self.tx.send(Event::Page(Box::new(o))).await.is_err() {
            self.shared.cancel();
        }
    }
}

impl CrawlSink for ChannelSink {
    fn page(&self, doc: ApiDocument) -> BoxFuture<'_, ()> {
        self.shared.pages.fetch_add(1, Ordering::AcqRel);
        let url = doc
            .metadata
            .source_url
            .clone()
            .or_else(|| doc.metadata.url.clone())
            .unwrap_or_default();
        Box::pin(self.deliver(Outcome {
            index: None,
            url,
            result: Ok(doc),
        }))
    }

    fn error<'a>(&'a self, url: &'a str, err: ApiError) -> BoxFuture<'a, ()> {
        self.shared.errors.fetch_add(1, Ordering::AcqRel);
        Box::pin(self.deliver(Outcome {
            index: None,
            url: url.to_string(),
            result: Err(err),
        }))
    }

    fn total(&self, total: usize) -> BoxFuture<'_, ()> {
        self.shared.total.store(total, Ordering::Release);
        Box::pin(async {})
    }

    fn cancelled(&self) -> BoxFuture<'_, bool> {
        let c = self.shared.is_cancelled();
        Box::pin(async move { c })
    }

    fn summary(&self, s: CrawlSummary) -> BoxFuture<'_, ()> {
        if let Ok(mut slot) = self.shared.summary.lock() {
            *slot = Some(s);
        }
        Box::pin(async {})
    }
}

/// `Engine.crawl` 的参数（Python 层的关键字参数原样传进来）。
pub struct CrawlArgs {
    pub limit: Option<usize>,
    pub max_depth: Option<usize>,
    pub include_paths: Option<Vec<String>>,
    pub exclude_paths: Option<Vec<String>>,
    pub crawl_entire_domain: bool,
    pub allow_subdomains: Option<bool>,
    pub sitemap: String,
    pub query: Option<String>,
    pub adaptive: bool,
    pub max_concurrency: Option<usize>,
}

/// 拼 Firecrawl 的 camelCase 请求再解析（和命令行一样），然后按 HTTP 接口的规则校验。
/// `ignoreRobotsTxt` 跟随每页选项里的 `ignore_robots_txt`。
fn crawl_request(
    scraper: &Scraper,
    url: String,
    opts: &pagesift::api::types::ScrapeOptions,
    a: CrawlArgs,
) -> Result<CrawlRequest, ApiError> {
    check_url(&url)?;
    check_sitemap(&a.sitemap)?;
    if a.adaptive && a.query.is_none() {
        return Err(ApiError::invalid("adaptive crawling needs a query"));
    }
    let mut body = json!({
        "url": url,
        "maxDiscoveryDepth": a.max_depth,
        "includePaths": a.include_paths.unwrap_or_default(),
        "excludePaths": a.exclude_paths.unwrap_or_default(),
        "crawlEntireDomain": a.crawl_entire_domain,
        "allowSubdomains": a.allow_subdomains,
        "sitemap": a.sitemap,
        "query": a.query,
        "adaptive": a.adaptive,
        "maxConcurrency": a.max_concurrency,
        "ignoreRobotsTxt": opts.ignore_robots_txt,
    });
    // 没给 limit 时用请求类型自己的默认值（和 HTTP 接口一致）
    if let (Some(l), Value::Object(m)) = (a.limit, &mut body) {
        m.insert("limit".into(), l.into());
    }
    let mut req: CrawlRequest =
        serde_json::from_value(body).map_err(|e| ApiError::invalid(e.to_string()))?;
    if req.limit > MAX_CRAWL_LIMIT {
        return Err(ApiError::invalid(format!(
            "limit must not exceed {MAX_CRAWL_LIMIT}"
        )));
    }
    if req
        .max_discovery_depth
        .is_some_and(|d| d as u64 > MAX_CRAWL_DEPTH)
    {
        return Err(ApiError::invalid(format!(
            "max_depth must not exceed {MAX_CRAWL_DEPTH}"
        )));
    }
    if req.max_concurrency == Some(0) {
        return Err(ApiError::invalid("max_concurrency must be at least 1"));
    }
    check_scrape_options(opts, scraper)?;
    req.scrape_options = opts.clone();
    Ok(req)
}

fn check_sitemap(s: &str) -> Result<(), ApiError> {
    if matches!(s, "include" | "skip" | "only") {
        Ok(())
    } else {
        Err(ApiError::invalid(format!(
            "sitemap must be include, skip or only, got {s:?}"
        )))
    }
}

/// 校验参数并在运行时上开始爬取，返回迭代器。
pub fn start(
    scraper: &Arc<Scraper>,
    url: String,
    opts: &pagesift::api::types::ScrapeOptions,
    args: CrawlArgs,
) -> PyResult<Crawl> {
    let req = crawl_request(scraper, url, opts, args).map_err(|e| to_py_err(&e, None))?;
    // 通道容量取爬取的并发上限：正在抓的页面都能交付，消费方跟不上时引擎停在交付处
    let capacity = req
        .max_concurrency
        .unwrap_or_else(|| scraper.concurrency())
        .max(1);
    let (tx, rx) = mpsc::channel(capacity);
    let shared = Arc::new(Shared::default());
    let sink = Arc::new(ChannelSink {
        tx,
        shared: shared.clone(),
    });
    let limit = req.limit;
    let scraper = scraper.clone();
    let state = shared.clone();
    runtime().spawn(async move {
        let result = scraper.crawl(&req, sink.clone()).await;
        state.finish(limit);
        // 迭代器已经不在时发不出去，没关系
        let _ = sink.tx.send(Event::Done(result)).await;
    });
    Ok(Crawl {
        rx: Arc::new(tokio::sync::Mutex::new(rx)),
        shared,
        ended: Arc::new(AtomicBool::new(false)),
    })
}

/// 进行中的整站爬取：每完成一页产出一个 `Document`（失败页 `success` 为 False）。
/// `for` 和 `async for` 都可以用，只能迭代一遍；结束后 `summary` 给出停止原因。
#[pyclass(frozen, module = "pagesift._pagesift")]
pub struct Crawl {
    rx: Arc<tokio::sync::Mutex<mpsc::Receiver<Event>>>,
    shared: Arc<Shared>,
    /// 已经收到结束消息（之后迭代直接结束）。
    ended: Arc<AtomicBool>,
}

/// 取下一页。`Ok(None)` 表示结束；引擎报错（比如路径规则写错）时返回 `Err`。
async fn next_page(
    rx: &tokio::sync::Mutex<mpsc::Receiver<Event>>,
    shared: &Shared,
    ended: &AtomicBool,
) -> Result<Option<Outcome>, ApiError> {
    if shared.is_cancelled() || ended.load(Ordering::Acquire) {
        return Ok(None);
    }
    let next = rx.lock().await.recv().await;
    if shared.is_cancelled() {
        return Ok(None);
    }
    match next {
        Some(Event::Page(o)) => Ok(Some(*o)),
        Some(Event::Done(r)) => {
            ended.store(true, Ordering::Release);
            r.map(|()| None)
        }
        None => {
            ended.store(true, Ordering::Release);
            Ok(None)
        }
    }
}

#[pymethods]
impl Crawl {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// 等待下一页，期间释放 GIL。
    fn __next__(&self, py: Python<'_>) -> PyResult<Option<Document>> {
        let (rx, shared, ended) = (self.rx.clone(), self.shared.clone(), self.ended.clone());
        let next = block_on(py, async move { next_page(&rx, &shared, &ended).await })?;
        match next {
            Ok(o) => Ok(o.map(Document::from)),
            Err(e) => Err(to_py_err(&e, None)),
        }
    }

    fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let (rx, shared, ended) = (self.rx.clone(), self.shared.clone(), self.ended.clone());
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            match next_page(&rx, &shared, &ended).await {
                Ok(Some(o)) => Ok(Document::from(o)),
                Ok(None) => Err(PyStopAsyncIteration::new_err(())),
                Err(e) => Err(to_py_err(&e, None)),
            }
        })
    }

    /// 停止爬取：引擎不再调度新页面，之后迭代立即结束，已完成、还没取出的页面也丢弃。
    fn cancel(&self) {
        self.shared.cancel();
        // 关闭通道让正在交付的页面立即返回；有人正在等结果时锁拿不到，交给运行时等它释放
        let rx = self.rx.clone();
        runtime().spawn(async move {
            let mut rx = rx.lock().await;
            rx.close();
            while rx.try_recv().is_ok() {}
        });
    }

    /// 已经确定要抓的页面数（随着发现新链接增长，不超过 `limit`）。
    #[getter]
    fn total(&self) -> usize {
        self.shared.total.load(Ordering::Acquire)
    }

    /// 引擎已经交付的成功页数（含还没取出的）。
    #[getter]
    fn pages(&self) -> usize {
        self.shared.pages.load(Ordering::Acquire)
    }

    /// 引擎已经交付的失败页数（含还没取出的）。
    #[getter]
    fn errors(&self) -> usize {
        self.shared.errors.load(Ordering::Acquire)
    }

    /// 爬取结束后的说明，结束前为 None：`stop_reason`（`limit_reached`、`exhausted`、`cancelled`，
    /// 带 query 时还有 `coverage_reached`、`saturated`、`no_relevant_pages`）、`pages`，
    /// 自适应爬取另有 `coverage` 和 `term_evidence`。
    #[getter]
    fn summary<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        let Some(s) = self.shared.summary.lock().ok().and_then(|s| s.clone()) else {
            return Ok(None);
        };
        let d = PyDict::new(py);
        d.set_item("stop_reason", s.stop_reason)?;
        d.set_item("pages", s.pages)?;
        d.set_item("coverage", s.coverage)?;
        d.set_item("term_evidence", s.term_evidence)?;
        Ok(Some(d))
    }
}

impl Drop for Crawl {
    /// 迭代器被丢弃（`for` 里 `break` 之后）：停止爬取。
    fn drop(&mut self) {
        self.shared.cancel();
    }
}

/// `Engine.map` 的参数。
pub struct MapArgs {
    pub limit: Option<usize>,
    pub search: Option<String>,
    pub sitemap: String,
    pub ignore_robots_txt: bool,
}

pub fn map_request(url: String, a: MapArgs) -> Result<MapRequest, ApiError> {
    check_url(&url)?;
    check_sitemap(&a.sitemap)?;
    let mut body = json!({
        "url": url,
        "search": a.search,
        "sitemap": a.sitemap,
        "ignoreRobotsTxt": a.ignore_robots_txt,
    });
    if let (Some(l), Value::Object(m)) = (a.limit, &mut body) {
        m.insert("limit".into(), l.into());
    }
    let req: MapRequest =
        serde_json::from_value(body).map_err(|e| ApiError::invalid(e.to_string()))?;
    if req.limit > MAX_MAP_LIMIT {
        return Err(ApiError::invalid(format!(
            "limit must not exceed {MAX_MAP_LIMIT}"
        )));
    }
    Ok(req)
}

/// 链接列表转成 `[{"url", "title", "description"}]`。
pub fn links_to_py<'py>(
    py: Python<'py>,
    links: Vec<pagesift::api::types::MapLink>,
) -> PyResult<Vec<Bound<'py, PyDict>>> {
    links
        .into_iter()
        .map(|l| {
            let d = PyDict::new(py);
            d.set_item("url", l.url)?;
            d.set_item("title", l.title)?;
            d.set_item("description", l.description)?;
            Ok(d)
        })
        .collect()
}

/// 同步 map：等待期间释放 GIL。
pub fn map_blocking<'py>(
    py: Python<'py>,
    scraper: &Arc<Scraper>,
    req: MapRequest,
) -> PyResult<Vec<Bound<'py, PyDict>>> {
    let scraper = scraper.clone();
    let out = block_on(py, async move { scraper.map(&req).await })?;
    match out {
        Ok(links) => links_to_py(py, links),
        Err(e) => Err(to_py_err(&e, None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_args(sitemap: &str, limit: Option<usize>) -> MapArgs {
        MapArgs {
            limit,
            search: None,
            sitemap: sitemap.into(),
            ignore_robots_txt: false,
        }
    }

    #[test]
    fn map_request_validates_like_the_http_api() {
        let ok = map_request("https://example.com/".into(), map_args("skip", Some(7))).unwrap();
        assert_eq!((ok.limit, ok.sitemap.as_str()), (7, "skip"));
        let default =
            map_request("https://example.com/".into(), map_args("include", None)).unwrap();
        assert!(default.limit > 0);
        for (url, args) in [
            ("ftp://example.com/", map_args("include", None)),
            ("https://example.com/", map_args("never", None)),
            (
                "https://example.com/",
                map_args("include", Some(MAX_MAP_LIMIT + 1)),
            ),
        ] {
            assert!(map_request(url.into(), args).is_err(), "{url}");
        }
    }

    #[test]
    fn summary_is_inferred_only_when_the_engine_gives_none() {
        let s = Shared::default();
        s.pages.store(3, Ordering::Release);
        s.errors.store(1, Ordering::Release);
        s.finish(4);
        let got = s.summary.lock().unwrap().clone().unwrap();
        assert_eq!((got.stop_reason.as_str(), got.pages), ("limit_reached", 3));

        let s = Shared::default();
        s.finish(4);
        assert_eq!(
            s.summary.lock().unwrap().as_ref().unwrap().stop_reason,
            "exhausted"
        );

        let s = Shared::default();
        s.cancel();
        s.finish(4);
        assert_eq!(
            s.summary.lock().unwrap().as_ref().unwrap().stop_reason,
            "cancelled"
        );

        let s = Shared::default();
        let engine = CrawlSummary {
            stop_reason: "saturated".into(),
            coverage: Some(0.5),
            term_evidence: Default::default(),
            pages: 9,
        };
        *s.summary.lock().unwrap() = Some(engine.clone());
        s.finish(4);
        assert_eq!(s.summary.lock().unwrap().clone(), Some(engine));
    }
}
