//! 整站爬取（`Scraper.crawl`）和站点链接列表（`Scraper.map`）。
//!
//! 爬取在 napi 的 tokio 运行时上进行，结果经有界通道交给 [`CrawlStream`]（`for await`）：
//! 消费方慢时通道满，爬取在交付页面处等待（背压），不丢页。`break`、`cancel()` 或对象被回收后，
//! `CrawlSink::cancelled` 返回 true，引擎在下一轮调度前停止。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use napi::bindgen_prelude::*;
use napi_derive::napi;
use pagesift::api::types::{
    CrawlRequest, CrawlSummary as ApiSummary, Document as ApiDocument, MapLink as ApiMapLink,
    MapRequest,
};
use pagesift::api::{
    ApiError, BoxFuture, CrawlSink, MAX_CRAWL_DEPTH, MAX_CRAWL_LIMIT, MAX_MAP_LIMIT,
    check_scrape_options, check_url,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::error::{BatchError, Fallible};
use crate::page::Page;

/// `Scraper.crawl` 的选项：爬取范围，加上每页的抓取选项（`formats`、`onlyMainContent`、`engine`）。
#[napi(object, object_from_js = false)]
#[derive(Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CrawlOptions {
    /// 最多抓多少页；不给时和 HTTP 接口的默认值一样。带 `adaptive` 时只是上限。
    pub limit: Option<u32>,
    /// 从起始页算起最多跟几层链接。
    pub max_depth: Option<u32>,
    /// 只抓路径匹配这些正则的页面。
    pub include_paths: Option<Vec<String>>,
    /// 不抓路径匹配这些正则的页面。
    pub exclude_paths: Option<Vec<String>>,
    /// 不限于起始页所在目录，整个主机都爬。
    pub crawl_entire_domain: Option<bool>,
    /// 也爬同一站点的子域名。
    pub allow_subdomains: Option<bool>,
    /// `include`（默认，sitemap 和页面链接都用）、`skip`（不读 sitemap）、`only`（只抓 sitemap 里的）。
    #[napi(ts_type = "'include' | 'skip' | 'only'")]
    pub sitemap: Option<String>,
    /// 按和查询的相关性优先爬取。
    pub query: Option<String>,
    /// 内容覆盖查询后自动停止（需要 `query`）。
    pub adaptive: Option<bool>,
    /// 不检查 robots.txt（整个爬取和每一页）。
    pub ignore_robots_txt: Option<bool>,
    /// 这次爬取同时在抓的页面数上限。
    pub max_concurrency: Option<u32>,
    /// 每页计算哪些输出，默认 `['markdown']`。
    #[napi(ts_type = "Format[]")]
    pub formats: Option<Vec<String>>,
    /// 每页只保留正文，默认 true。
    pub only_main_content: Option<bool>,
    /// `auto`（默认）、`http` 或 `browser`。
    #[napi(ts_type = "'auto' | 'http' | 'browser'")]
    pub engine: Option<String>,
}

/// `Scraper.map` 的选项。
#[napi(object, object_from_js = false)]
#[derive(Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MapOptions {
    /// 最多返回多少个链接；不给时和 HTTP 接口的默认值一样。
    pub limit: Option<u32>,
    /// 地址里命中搜索词多的链接排在前面，没命中的保留在后面。
    pub search: Option<String>,
    /// 取值同 `CrawlOptions.sitemap`。
    #[napi(ts_type = "'include' | 'skip' | 'only'")]
    pub sitemap: Option<String>,
    /// 不检查 robots.txt。
    pub ignore_robots_txt: Option<bool>,
}

/// 站点里的一个链接。
#[napi(object, object_from_js = false)]
pub struct MapLink {
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
}

impl From<ApiMapLink> for MapLink {
    fn from(l: ApiMapLink) -> Self {
        Self {
            url: l.url,
            title: l.title,
            description: l.description,
        }
    }
}

/// 爬取里一页的结果：`page` 和 `error` 恰好有一个（失败项和 `scrapeMany` 的一样）。
#[napi(object, object_from_js = false)]
pub struct CrawlItem {
    pub url: String,
    pub page: Option<Page>,
    pub error: Option<BatchError>,
}

/// 爬取结束后的说明。
#[napi(object, object_from_js = false)]
#[derive(Clone)]
pub struct CrawlSummary {
    /// `limit_reached`、`exhausted`、`cancelled`；带 `query` 时还有 `coverage_reached`、
    /// `saturated`、`no_relevant_pages`。
    pub stop_reason: String,
    /// 自适应爬取结束时的覆盖度（0–1）。
    pub coverage: Option<f64>,
    /// 自适应爬取里每个查询词收集到的证据块数。
    pub term_evidence: HashMap<String, u32>,
    pub pages: u32,
}

impl From<ApiSummary> for CrawlSummary {
    fn from(s: ApiSummary) -> Self {
        Self {
            stop_reason: s.stop_reason,
            coverage: s.coverage,
            term_evidence: s
                .term_evidence
                .into_iter()
                .map(|(k, v)| (k, saturate(v)))
                .collect(),
            pages: saturate(s.pages),
        }
    }
}

fn saturate(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// 通道里的消息：一页（成功或失败），或者爬取结束（引擎的返回值）。
enum Event {
    Item(Box<CrawlItem>),
    Done(std::result::Result<(), ApiError>),
}

/// 爬取任务和迭代器共享的状态。
#[derive(Default)]
struct Shared {
    cancelled: AtomicBool,
    ended: AtomicBool,
    pages: AtomicUsize,
    errors: AtomicUsize,
    total: AtomicUsize,
    /// 带 `query` 的爬取由引擎给出；普通爬取在结束时由 [`Shared::finish`] 按页数补上。
    summary: Mutex<Option<ApiSummary>>,
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
        *s = Some(ApiSummary {
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
    async fn deliver(&self, item: CrawlItem) {
        if self.shared.is_cancelled() {
            return;
        }
        // 消费方已经不在（迭代器被回收）：当作取消
        if self.tx.send(Event::Item(Box::new(item))).await.is_err() {
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
        Box::pin(self.deliver(CrawlItem {
            url,
            page: Some(Page::from_scrape(doc)),
            error: None,
        }))
    }

    fn error<'a>(&'a self, url: &'a str, err: ApiError) -> BoxFuture<'a, ()> {
        self.shared.errors.fetch_add(1, Ordering::AcqRel);
        Box::pin(self.deliver(CrawlItem {
            url: url.to_string(),
            page: None,
            error: Some(err.into()),
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

    fn summary(&self, s: ApiSummary) -> BoxFuture<'_, ()> {
        if let Ok(mut slot) = self.shared.summary.lock() {
            *slot = Some(s);
        }
        Box::pin(async {})
    }
}

fn check_sitemap(s: &str) -> Fallible<()> {
    if matches!(s, "include" | "skip" | "only") {
        Ok(())
    } else {
        Err(ApiError::invalid(format!(
            "sitemap must be include, skip or only, got {s:?}"
        )))
    }
}

/// 拼 Firecrawl 的 camelCase 请求再解析（和命令行一样），然后按 HTTP 接口的规则校验。
fn crawl_request(core: &pagesift::Scraper, url: String, o: CrawlOptions) -> Fallible<CrawlRequest> {
    check_url(&url)?;
    let sitemap = o.sitemap.unwrap_or_else(|| "include".into());
    check_sitemap(&sitemap)?;
    let adaptive = o.adaptive.unwrap_or(false);
    if adaptive && o.query.is_none() {
        return Err(ApiError::invalid("adaptive crawling needs a query"));
    }
    if o.max_concurrency == Some(0) {
        return Err(ApiError::invalid("maxConcurrency must be at least 1"));
    }
    if o.max_depth.is_some_and(|d| u64::from(d) > MAX_CRAWL_DEPTH) {
        return Err(ApiError::invalid(format!(
            "maxDepth must not exceed {MAX_CRAWL_DEPTH}"
        )));
    }
    let ignore_robots = o.ignore_robots_txt.unwrap_or(false);
    let page = crate::options::ScrapeOptions {
        formats: o.formats,
        only_main_content: o.only_main_content,
        engine: o.engine,
        ignore_robots_txt: Some(ignore_robots),
        ..Default::default()
    }
    .to_api()?;
    check_scrape_options(&page, core)?;
    let mut body = json!({
        "url": url,
        "maxDiscoveryDepth": o.max_depth,
        "includePaths": o.include_paths.unwrap_or_default(),
        "excludePaths": o.exclude_paths.unwrap_or_default(),
        "crawlEntireDomain": o.crawl_entire_domain.unwrap_or(false),
        "allowSubdomains": o.allow_subdomains,
        "sitemap": sitemap,
        "query": o.query,
        "adaptive": adaptive,
        "maxConcurrency": o.max_concurrency,
        "ignoreRobotsTxt": ignore_robots,
    });
    // 没给 limit 时用请求类型自己的默认值（和 HTTP 接口一致）
    if let (Some(l), Value::Object(m)) = (o.limit, &mut body) {
        m.insert("limit".into(), l.into());
    }
    let mut req: CrawlRequest =
        serde_json::from_value(body).map_err(|e| ApiError::invalid(e.to_string()))?;
    if req.limit > MAX_CRAWL_LIMIT {
        return Err(ApiError::invalid(format!(
            "limit must not exceed {MAX_CRAWL_LIMIT}"
        )));
    }
    req.scrape_options = page;
    Ok(req)
}

/// 校验参数并在运行时上开始爬取，返回异步迭代器。
pub fn start(core: Arc<pagesift::Scraper>, url: String, o: CrawlOptions) -> Fallible<CrawlStream> {
    let req = crawl_request(&core, url, o)?;
    // 通道容量取爬取的并发上限：正在抓的页面都能交付，消费方跟不上时引擎停在交付处
    let capacity = req
        .max_concurrency
        .unwrap_or_else(|| core.concurrency())
        .max(1);
    let (tx, rx) = mpsc::channel(capacity);
    let shared = Arc::new(Shared::default());
    let sink = Arc::new(ChannelSink {
        tx,
        shared: shared.clone(),
    });
    let limit = req.limit;
    let state = shared.clone();
    napi::bindgen_prelude::spawn(async move {
        let result = core.crawl(&req, sink.clone()).await;
        state.finish(limit);
        // 迭代器已经不在时发不出去，没关系
        let _ = sink.tx.send(Event::Done(result)).await;
    });
    Ok(CrawlStream {
        rx: Arc::new(tokio::sync::Mutex::new(rx)),
        guard: CancelOnDrop(shared),
    })
}

/// 迭代器被回收时停止爬取。
struct CancelOnDrop(Arc<Shared>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// 进行中的整站爬取，每完成一页产出一个 `CrawlItem`（`for await`）。提前 `break` 或 `cancel()` 停止爬取；
/// 结束后 `summary` 给出停止原因。爬取本身出错（比如路径正则写错）时迭代以 Error 拒绝，
/// 消息以错误码开头（如 `INVALID_REQUEST: …`）。
#[napi(async_iterator)]
pub struct CrawlStream {
    rx: Arc<tokio::sync::Mutex<mpsc::Receiver<Event>>>,
    guard: CancelOnDrop,
}

#[napi]
impl AsyncGenerator for CrawlStream {
    type Yield = CrawlItem;
    type Next = ();
    type Return = ();

    fn next(
        &mut self,
        _value: Option<()>,
    ) -> impl Future<Output = Result<Option<CrawlItem>>> + Send + 'static {
        let (rx, shared) = (self.rx.clone(), self.guard.0.clone());
        async move {
            if shared.is_cancelled() || shared.ended.load(Ordering::Acquire) {
                return Ok(None);
            }
            let next = rx.lock().await.recv().await;
            if shared.is_cancelled() {
                return Ok(None);
            }
            match next {
                Some(Event::Item(item)) => Ok(Some(*item)),
                Some(Event::Done(r)) => {
                    shared.ended.store(true, Ordering::Release);
                    r.map(|()| None).map_err(|e| {
                        Error::from_reason(format!("{}: {}", e.code.as_str(), e.message))
                    })
                }
                None => {
                    shared.ended.store(true, Ordering::Release);
                    Ok(None)
                }
            }
        }
    }

    fn complete(
        &mut self,
        _value: Option<()>,
    ) -> impl Future<Output = Result<Option<CrawlItem>>> + Send + 'static {
        self.stop();
        async { Ok(None) }
    }
}

#[napi]
impl CrawlStream {
    /// 停止爬取：引擎不再调度新页面，之后迭代立即结束，已完成、还没取出的页面也丢弃。
    #[napi]
    pub fn cancel(&self) {
        self.stop();
    }

    /// 已经确定要抓的页面数（随着发现新链接增长，不超过 `limit`）。
    #[napi(getter)]
    pub fn total(&self) -> u32 {
        saturate(self.guard.0.total.load(Ordering::Acquire))
    }

    /// 引擎已经交付的成功页数（含还没取出的）。
    #[napi(getter)]
    pub fn pages(&self) -> u32 {
        saturate(self.guard.0.pages.load(Ordering::Acquire))
    }

    /// 引擎已经交付的失败页数（含还没取出的）。
    #[napi(getter)]
    pub fn errors(&self) -> u32 {
        saturate(self.guard.0.errors.load(Ordering::Acquire))
    }

    /// 爬取结束后的说明；结束前为 null。
    #[napi(getter)]
    pub fn summary(&self) -> Option<CrawlSummary> {
        let s = self.guard.0.summary.lock().ok()?.clone()?;
        Some(s.into())
    }
}

impl CrawlStream {
    fn stop(&self) {
        self.guard.0.cancel();
        // 关闭通道让正在交付的页面立即返回；有人正在等结果时锁拿不到，交给运行时等它释放
        let rx = self.rx.clone();
        napi::bindgen_prelude::spawn(async move {
            let mut rx = rx.lock().await;
            rx.close();
            while rx.try_recv().is_ok() {}
        });
    }
}

/// 校验 map 选项，组成请求。
pub fn map_request(url: String, o: MapOptions) -> Fallible<MapRequest> {
    check_url(&url)?;
    let sitemap = o.sitemap.unwrap_or_else(|| "include".into());
    check_sitemap(&sitemap)?;
    let mut body = json!({
        "url": url,
        "search": o.search,
        "sitemap": sitemap,
        "ignoreRobotsTxt": o.ignore_robots_txt.unwrap_or(false),
    });
    if let (Some(l), Value::Object(m)) = (o.limit, &mut body) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_request_validates_like_the_http_api() {
        let opts = |sitemap: Option<&str>, limit: Option<u32>| MapOptions {
            limit,
            sitemap: sitemap.map(Into::into),
            ..Default::default()
        };
        let ok = map_request("https://example.com/".into(), opts(Some("only"), Some(7))).unwrap();
        assert_eq!((ok.limit, ok.sitemap.as_str()), (7, "only"));
        let default = map_request("https://example.com/".into(), opts(None, None)).unwrap();
        assert_eq!(default.sitemap, "include");
        assert!(map_request("ftp://example.com/".into(), opts(None, None)).is_err());
        assert!(map_request("https://example.com/".into(), opts(Some("never"), None)).is_err());
    }

    #[test]
    fn summary_is_inferred_only_when_the_engine_gives_none() {
        let s = Shared::default();
        s.pages.store(2, Ordering::Release);
        s.finish(2);
        let got = CrawlSummary::from(s.summary.lock().unwrap().clone().unwrap());
        assert_eq!((got.stop_reason.as_str(), got.pages), ("limit_reached", 2));

        let s = Shared::default();
        s.cancel();
        s.finish(2);
        assert_eq!(
            s.summary.lock().unwrap().as_ref().unwrap().stop_reason,
            "cancelled"
        );

        let s = Shared::default();
        *s.summary.lock().unwrap() = Some(ApiSummary {
            stop_reason: "coverage_reached".into(),
            coverage: Some(0.9),
            term_evidence: [("rust".to_string(), 3)].into(),
            pages: 5,
        });
        s.finish(2);
        let got = CrawlSummary::from(s.summary.lock().unwrap().clone().unwrap());
        assert_eq!(got.stop_reason, "coverage_reached");
        assert_eq!(got.term_evidence.get("rust"), Some(&3));
    }
}
