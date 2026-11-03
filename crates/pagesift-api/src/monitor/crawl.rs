//! crawl 目标的一次检查：爬一遍，逐页比较，判断哪些页面消失了。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;

use super::run::{compare, failure, mark_gone};
use super::{MonitorState, PageResult, PageState};
use crate::error::{ApiError, ErrorCode};
use crate::limits::{Caller, Limiter};
use crate::server::AppState;
use crate::types::{CrawlRequest, CrawlSummary, Document, ScrapeOptions};

/// 把 crawl 目标的选项解析成爬取请求（创建时校验、每次检查都用它）。
pub(super) fn crawl_request(target: &super::Target, seed: &str) -> Result<CrawlRequest, ApiError> {
    let mut body = target.crawl_options.clone().unwrap_or_else(|| json!({}));
    if !body.is_object() {
        return Err(ApiError::invalid("crawlOptions must be an object"));
    }
    body["url"] = json!(seed);
    serde_json::from_value(body).map_err(|e| ApiError::invalid(format!("crawlOptions: {e}")))
}

/// crawl 目标：爬一遍，逐页比较。只有完整爬完（没有被截断、没有页面出错）才能说上次有、这次没爬到的
/// 页面消失了（removed）；否则只是不知道。第二个返回值是这次对变化率的观测。
pub(super) async fn check_crawl(
    s: &AppState,
    m: &MonitorState,
    seed: &str,
    target: &super::Target,
    opts: &ScrapeOptions,
    substantive_only: bool,
    caller: &Caller,
) -> (Vec<(String, PageState, PageResult)>, Option<bool>) {
    let mut req = match crawl_request(target, seed) {
        Ok(r) => r,
        Err(e) => {
            let st = PageState::default();
            return (vec![(seed.to_string(), st, failure(seed, &e))], None);
        }
    };
    let mut o = opts.clone();
    o.max_age = Some(0);
    req.scrape_options = o;
    let sink = Arc::new(Collect {
        limiter: s.limiter.clone(),
        caller: caller.clone(),
        docs: Default::default(),
        errors: Default::default(),
        stop: Default::default(),
        quota_hit: Default::default(),
    });
    let crawled = s.engine.crawl(&req, sink.clone()).await;
    let docs = std::mem::take(&mut *sink.docs.lock().await);
    let errors = std::mem::take(&mut *sink.errors.lock().await);
    let stop = sink.stop.lock().await.take();
    let quota_hit = sink.quota_hit.load(Ordering::Relaxed);
    let tid = target.id.clone().unwrap_or_default();
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut any = false;
    let mut seen_ok = false;
    for doc in docs {
        let url = doc
            .metadata
            .source_url
            .clone()
            .or(doc.metadata.url.clone())
            .unwrap_or_default();
        seen.insert(url.clone());
        let prev = m.pages.get(&url).cloned().unwrap_or(PageState {
            target: tid.clone(),
            next_due: f64::INFINITY,
            ..Default::default()
        });
        let mut next = prev.clone();
        let r = compare(&url, &prev, &doc, substantive_only, &mut next);
        any |= matches!(r.status.as_str(), "changed" | "new" | "removed");
        seen_ok |= r.status != "error";
        next.next_due = f64::INFINITY; // 爬取目标里的页面跟着目标一起检查
        out.push((url, next, r));
    }
    // 单页错误：404 是页面没了，robots 是策略，都不影响“这次爬全了没有”；其他错误下面的页面可能没被发现
    let mut gaps = false;
    for (u, e) in errors {
        seen.insert(u.clone());
        let prev = m.pages.get(&u);
        let mut r = failure(&u, &e);
        match (e.code, prev) {
            (ErrorCode::NotFound, Some(p)) => {
                let mut next = p.clone();
                mark_gone(p, &mut next, &mut r);
                any |= r.status == "removed";
                out.push((u, next, r));
                continue;
            }
            (ErrorCode::NotFound | ErrorCode::RobotsDisallowed, _) => {}
            _ => gaps = true,
        }
        // 保存的内容不动：这里只记结果
        out.push((
            u.clone(),
            prev.cloned().unwrap_or(PageState {
                target: tid.clone(),
                next_due: f64::INFINITY,
                ..Default::default()
            }),
            r,
        ));
    }
    let whole_failure = match &crawled {
        Err(e) => Some(failure(seed, e)),
        Ok(()) if quota_hit => Some(failure(seed, &crate::limits::quota_error(caller))),
        Ok(()) if !seen_ok && !gaps => Some(failure(
            seed,
            &ApiError::new(ErrorCode::UpstreamError, "the crawl returned no pages"),
        )),
        Ok(()) => None,
    };
    if let Some(r) = &whole_failure {
        // 种子页这次也许有结果（配额中途用完）：只多记一条说明
        out.push((
            seed.to_string(),
            m.pages.get(seed).cloned().unwrap_or(PageState {
                target: tid.clone(),
                next_due: f64::INFINITY,
                ..Default::default()
            }),
            r.clone(),
        ));
    }
    let complete = whole_failure.is_none()
        && !gaps
        && req.query.is_none()
        && stop.as_deref() == Some("exhausted");
    for (u, p) in &m.pages {
        if complete
            && p.target == tid
            && !u.starts_with("crawl:")
            && !seen.contains(u)
            && p.markdown.is_some()
        {
            any = true;
            let mut gone = p.clone();
            gone.markdown = None;
            gone.extract = None;
            gone.etag = None;
            gone.last_modified = None;
            gone.removed = true;
            out.push((
                u.clone(),
                gone,
                PageResult {
                    url: u.clone(),
                    status: "removed".into(),
                    error: None,
                    noise_only: false,
                    diff: None,
                    field_changes: Vec::new(),
                    not_modified: false,
                },
            ));
        }
    }
    let observed = if any {
        Some(true)
    } else {
        (whole_failure.is_none() && !gaps).then_some(false)
    };
    (out, observed)
}

struct Collect {
    limiter: Arc<Limiter>,
    caller: Caller,
    docs: tokio::sync::Mutex<Vec<Document>>,
    errors: tokio::sync::Mutex<Vec<(String, ApiError)>>,
    stop: tokio::sync::Mutex<Option<String>>,
    quota_hit: AtomicBool,
}

impl crate::CrawlSink for Collect {
    fn page(&self, doc: Document) -> crate::BoxFuture<'_, ()> {
        Box::pin(async move {
            self.limiter.consume_pages(&self.caller, 1).await;
            self.docs.lock().await.push(doc);
        })
    }
    fn error<'a>(&'a self, url: &'a str, e: ApiError) -> crate::BoxFuture<'a, ()> {
        Box::pin(async move { self.errors.lock().await.push((url.to_string(), e)) })
    }
    fn total(&self, _: usize) -> crate::BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn cancelled(&self) -> crate::BoxFuture<'_, bool> {
        Box::pin(async {
            let out = self.limiter.remaining_pages(&self.caller) == Some(0);
            if out {
                self.quota_hit.store(true, Ordering::Relaxed);
            }
            out
        })
    }
    fn summary(&self, summary: CrawlSummary) -> crate::BoxFuture<'_, ()> {
        Box::pin(async move { *self.stop.lock().await = Some(summary.stop_reason) })
    }
}
