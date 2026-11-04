//! 整站爬取：在内部运行时上爬完，页面按完成顺序收集，一次返回。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pagesift_core::Scraper;
use pagesift_core::api::types::{
    CrawlRequest, CrawlSummary, Document as ApiDocument, ScrapeOptions,
};
use pagesift_core::api::{
    ApiError, BoxFuture, CrawlSink, MAX_CRAWL_DEPTH, MAX_CRAWL_LIMIT, check_scrape_options,
    check_url,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::runtime::Runtime;

use crate::args;
use crate::doc::{Outcome, document_value, failure_value};
use crate::ops::check_sitemap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CrawlArgs {
    limit: Option<usize>,
    max_depth: Option<usize>,
    include_paths: Option<Vec<String>>,
    exclude_paths: Option<Vec<String>>,
    #[serde(default)]
    crawl_entire_domain: bool,
    allow_subdomains: Option<bool>,
    sitemap: Option<String>,
    query: Option<String>,
    #[serde(default)]
    adaptive: bool,
    max_concurrency: Option<usize>,
}

const CRAWL_KEYS: &[&str] = &[
    "limit",
    "max_depth",
    "include_paths",
    "exclude_paths",
    "crawl_entire_domain",
    "allow_subdomains",
    "sitemap",
    "query",
    "adaptive",
    "max_concurrency",
];

/// 拼 Firecrawl 的 camelCase 请求再解析（和命令行、Python 一样），然后按 HTTP 接口的规则校验。
/// `ignoreRobotsTxt` 跟随每页选项里的 `ignore_robots_txt`。
fn crawl_request(
    scraper: &Scraper,
    url: &str,
    opts: &ScrapeOptions,
    a: CrawlArgs,
) -> Result<CrawlRequest, ApiError> {
    check_url(url)?;
    let sitemap = a.sitemap.unwrap_or_else(|| "include".into());
    check_sitemap(&sitemap)?;
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
        "sitemap": sitemap,
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

/// 收集引擎回报的页面。页面到手就转成 JSON，不留两份。
#[derive(Default)]
struct Collector {
    documents: Mutex<Vec<Value>>,
    summary: Mutex<Option<CrawlSummary>>,
    pages: AtomicUsize,
    errors: AtomicUsize,
}

impl Collector {
    fn push(&self, o: Outcome) {
        if let Ok(mut d) = self.documents.lock() {
            d.push(document_value(&o));
        }
    }
}

impl CrawlSink for Collector {
    fn page(&self, doc: ApiDocument) -> BoxFuture<'_, ()> {
        self.pages.fetch_add(1, Ordering::AcqRel);
        let url = doc
            .metadata
            .source_url
            .clone()
            .or_else(|| doc.metadata.url.clone())
            .unwrap_or_default();
        self.push(Outcome {
            index: None,
            url: Some(url),
            result: Ok(doc),
        });
        Box::pin(async {})
    }

    fn error<'a>(&'a self, url: &'a str, err: ApiError) -> BoxFuture<'a, ()> {
        self.errors.fetch_add(1, Ordering::AcqRel);
        self.push(Outcome {
            index: None,
            url: Some(url.to_string()),
            result: Err(err),
        });
        Box::pin(async {})
    }

    fn total(&self, _total: usize) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn cancelled(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { false })
    }

    fn summary(&self, s: CrawlSummary) -> BoxFuture<'_, ()> {
        if let Ok(mut slot) = self.summary.lock() {
            *slot = Some(s);
        }
        Box::pin(async {})
    }
}

pub fn crawl(rt: &Runtime, s: &Arc<Scraper>, url: &str, options: Option<&str>) -> Value {
    let prepared = args::object(options).and_then(|mut m| {
        let a: CrawlArgs = args::take(&mut m, CRAWL_KEYS)?;
        let opts = args::scrape_options(m)?;
        crawl_request(s, url, &opts, a)
    });
    let req = match prepared {
        Ok(r) => r,
        Err(e) => return failure_value(&e),
    };
    let sink = Arc::new(Collector::default());
    let result = rt.block_on(s.crawl(&req, sink.clone()));
    let documents = std::mem::take(&mut *sink.documents.lock().unwrap_or_else(|e| e.into_inner()));
    let pages = sink.pages.load(Ordering::Acquire);
    let done = pages + sink.errors.load(Ordering::Acquire);
    // 广度优先爬取不给结束说明：按完成页数推断
    let summary = sink
        .summary
        .lock()
        .ok()
        .and_then(|s| s.clone())
        .unwrap_or_else(|| CrawlSummary {
            stop_reason: if done >= req.limit {
                "limit_reached".into()
            } else {
                "exhausted".into()
            },
            coverage: None,
            term_evidence: Default::default(),
            pages,
        });
    let summary = json!({
        "stop_reason": summary.stop_reason,
        "pages": summary.pages,
        "coverage": summary.coverage,
        "term_evidence": summary.term_evidence,
    });
    match result {
        Ok(()) => json!({"success": true, "documents": documents, "summary": summary}),
        Err(e) => {
            // 爬取本身出错（比如路径规则写错）：已经抓到的页面也还给调用方
            let mut v = failure_value(&e);
            if let Value::Object(m) = &mut v {
                m.insert("documents".into(), Value::Array(documents));
            }
            v
        }
    }
}
