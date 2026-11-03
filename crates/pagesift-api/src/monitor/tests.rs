//! 监控的模块测试：脚本化的假引擎，经真实路由走创建、检查、落盘、复制。

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pagesift_store::Store;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::engine::{BoxFuture, CrawlSink, Engine};
use crate::error::{ApiError, ErrorCode};
use crate::limits::KeyLimits;
use crate::server::{Config, router};
use crate::types::{CrawlRequest, CrawlSummary, Document, Metadata, ScrapeOptions};

/// 一个网址的回应。
#[derive(Clone)]
pub(super) enum Reply {
    Page(u16, String),
    Fail(ErrorCode),
}

/// 一次爬取的剧本。
#[derive(Clone, Default)]
pub(super) struct CrawlScript {
    pub docs: Vec<(String, u16, String)>,
    pub errors: Vec<(String, ErrorCode)>,
    pub failed: bool,
    pub stop: Option<&'static str>,
}

#[derive(Default)]
pub(super) struct Script {
    pub pages: Mutex<HashMap<String, Reply>>,
    pub crawl: Mutex<CrawlScript>,
    pub scrapes: AtomicUsize,
    pub crawls: AtomicUsize,
}

impl Script {
    pub fn page(&self, url: &str, status: u16, md: &str) {
        self.pages
            .lock()
            .unwrap()
            .insert(url.into(), Reply::Page(status, md.into()));
    }

    pub fn fail(&self, url: &str, code: ErrorCode) {
        self.pages
            .lock()
            .unwrap()
            .insert(url.into(), Reply::Fail(code));
    }

    pub fn crawl(&self, c: CrawlScript) {
        *self.crawl.lock().unwrap() = c;
    }
}

fn doc(url: &str, status: u16, md: &str) -> Document {
    Document {
        markdown: Some(md.into()),
        metadata: Metadata {
            source_url: Some(url.into()),
            status_code: status,
            ..Default::default()
        },
        ..Default::default()
    }
}

struct Scripted(Arc<Script>);

impl Engine for Scripted {
    fn scrape<'a>(
        &'a self,
        url: &'a str,
        _opts: &'a ScrapeOptions,
    ) -> BoxFuture<'a, Result<Document, ApiError>> {
        Box::pin(async move {
            self.0.scrapes.fetch_add(1, Ordering::SeqCst);
            let reply = self.0.pages.lock().unwrap().get(url).cloned();
            match reply {
                Some(Reply::Page(st, md)) => Ok(doc(url, st, &md)),
                Some(Reply::Fail(code)) => Err(ApiError::new(code, "scripted failure")),
                None => Err(ApiError::new(ErrorCode::Internal, "no script")),
            }
        })
    }

    fn crawl<'a>(
        &'a self,
        _req: &'a CrawlRequest,
        sink: Arc<dyn CrawlSink>,
    ) -> BoxFuture<'a, Result<(), ApiError>> {
        Box::pin(async move {
            self.0.crawls.fetch_add(1, Ordering::SeqCst);
            let c = self.0.crawl.lock().unwrap().clone();
            if c.failed {
                return Err(ApiError::new(ErrorCode::NetworkError, "scripted crawl"));
            }
            for (u, st, md) in &c.docs {
                sink.page(doc(u, *st, md)).await;
            }
            for (u, code) in &c.errors {
                sink.error(u, ApiError::new(*code, "scripted page failure"))
                    .await;
            }
            sink.summary(CrawlSummary {
                stop_reason: c.stop.unwrap_or("exhausted").into(),
                coverage: None,
                term_evidence: Default::default(),
                pages: c.docs.len(),
            })
            .await;
            Ok(())
        })
    }
}

pub(super) async fn call_as(
    api: &Router,
    key: Option<&str>,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(k) = key {
        req = req.header("authorization", format!("Bearer {k}"));
    }
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let resp = api.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let st = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

pub(super) async fn call(
    api: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    call_as(api, None, method, uri, body).await
}

pub(super) fn scripted(script: &Arc<Script>, config: Config) -> Router {
    router(Arc::new(Scripted(script.clone())), config)
}

pub(super) async fn create(api: &Router, body: Value) -> String {
    let (st, v) = call(api, "POST", "/v2/monitor", Some(body)).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    v["data"]["id"].as_str().unwrap().to_string()
}

/// 等创建后后台的第一次检查结束（之后到期时间在很久以后，`run` 的结果才是确定的）。
pub(super) async fn baseline(api: &Router, id: &str) {
    for _ in 0..2000 {
        let (_, v) = call(api, "GET", &format!("/v2/monitor/{id}/checks"), None).await;
        if v["data"].as_array().is_some_and(|c| !c.is_empty()) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("the first check never finished");
}

pub(super) async fn run(api: &Router, id: &str) -> Value {
    let (st, v) = call(api, "POST", &format!("/v2/monitor/{id}/run"), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    v["data"].clone()
}

/// 这次检查里各网址的状态。
pub(super) fn statuses(check: &Value) -> BTreeMap<String, String> {
    check["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["url"].as_str().unwrap().to_string(),
                p["status"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

const SEED: &str = "https://site.test/";
pub(super) const A: &str = "https://site.test/a";
const B: &str = "https://site.test/b";

fn two_pages() -> CrawlScript {
    CrawlScript {
        docs: vec![
            (SEED.into(), 200, "# Home\n\nWelcome to the site.".into()),
            (A.into(), 200, "# A\n\nThe first page body.".into()),
            (B.into(), 200, "# B\n\nThe second page body.".into()),
        ],
        ..Default::default()
    }
}

fn crawl_target() -> Value {
    json!({"targets": [{"type": "crawl", "url": SEED}]})
}

fn scrape_target() -> Value {
    json!({"targets": [{"type": "scrape", "urls": [A]}]})
}

/// 重启：第二个实例读同一份存储，crawl 型监控（页面没有自己的到期时刻）还在，内容也在
/// （第二次检查报 same，不是 new）。
#[test]
fn a_crawl_monitor_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let script = Arc::new(Script::default());
    script.crawl(two_pages());
    let id = {
        // 运行时结束时调度任务和它持有的存储一起没了，相当于进程退出
        let rt = runtime();
        rt.block_on(async {
            let cfg = Config {
                store: Some(Store::open_dir(dir.path()).unwrap()),
                ..Default::default()
            };
            let api = scripted(&script, cfg);
            let id = create(&api, crawl_target()).await;
            baseline(&api, &id).await;
            // 手动检查返回时已经落盘
            let first = run(&api, &id).await;
            assert_eq!(statuses(&first)[A], "same", "{first}");
            id
        })
    };
    let rt = runtime();
    rt.block_on(async {
        let cfg = Config {
            store: Some(Store::open_dir(dir.path()).unwrap()),
            ..Default::default()
        };
        let api = scripted(&script, cfg);
        let (st, v) = call(&api, "GET", &format!("/v2/monitor/{id}"), None).await;
        assert_eq!(st, StatusCode::OK, "lost after a restart: {v}");
        let second = run(&api, &id).await;
        assert_eq!(statuses(&second)[A], "same", "{second}");
    });
}

/// 读不回来的记录不能静默跳过：`/metrics` 里有计数；记录留在存储里不删。
#[test]
fn an_unreadable_saved_monitor_is_counted_not_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    store
        .tasks()
        .kv_put("monitor/broken", b"{not json")
        .unwrap();
    let script = Arc::new(Script::default());
    let rt = runtime();
    rt.block_on(async {
        let cfg = Config {
            store: Some(store.clone()),
            ..Default::default()
        };
        let api = scripted(&script, cfg);
        let resp = api
            .clone()
            .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("pagesift_monitor_load_failures_total 1"),
            "{text}"
        );
    });
    assert!(store.tasks().kv_get("monitor/broken").unwrap().is_some());
}

/// 爬取整个失败：旧页面不能报 removed，检查状态如实是 failed，内容留着（恢复后是 same）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_crawl_does_not_report_removed() {
    let script = Arc::new(Script::default());
    script.crawl(two_pages());
    let api = scripted(&script, Config::default());
    let id = create(&api, crawl_target()).await;
    baseline(&api, &id).await;

    script.crawl(CrawlScript {
        failed: true,
        ..Default::default()
    });
    let check = run(&api, &id).await;
    assert_eq!(check["status"], "failed", "{check}");
    assert_eq!(check["summary"]["removed"], 0, "{check}");
    assert_eq!(statuses(&check)[SEED], "error", "{check}");

    script.crawl(two_pages());
    let back = run(&api, &id).await;
    assert_eq!(back["status"], "completed", "{back}");
    assert_eq!(statuses(&back)[A], "same", "{back}");
}

/// 有页面出错、被 limit 截断或一页都没拿到：没爬到的页面只是不知道，不是消失。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_incomplete_crawl_does_not_report_removed() {
    let script = Arc::new(Script::default());
    script.crawl(two_pages());
    let api = scripted(&script, Config::default());
    let id = create(&api, crawl_target()).await;
    baseline(&api, &id).await;

    // 中间页面网络出错，它下面的 B 没被发现
    let mut partial = two_pages();
    partial.docs.truncate(1);
    partial.errors = vec![(A.into(), ErrorCode::NetworkError)];
    script.crawl(partial);
    let check = run(&api, &id).await;
    assert_eq!(check["status"], "partial", "{check}");
    assert_eq!(statuses(&check)[A], "error", "{check}");
    assert_eq!(check["summary"]["removed"], 0, "{check}");

    // 被 limit 截断
    let mut limited = two_pages();
    limited.docs.truncate(1);
    limited.stop = Some("limit_reached");
    script.crawl(limited);
    let check = run(&api, &id).await;
    assert_eq!(check["summary"]["removed"], 0, "{check}");

    // 一页都没拿到，却没报错
    script.crawl(CrawlScript::default());
    let check = run(&api, &id).await;
    assert_eq!(check["status"], "failed", "{check}");
    assert_eq!(check["summary"]["removed"], 0, "{check}");

    // 完整爬完、确实没有了 B：这才是 removed；同一页再查不重复报
    let mut complete = two_pages();
    complete.docs.truncate(2);
    script.crawl(complete.clone());
    let check = run(&api, &id).await;
    assert_eq!(statuses(&check)[B], "removed", "{check}");
    let again = run(&api, &id).await;
    assert!(!statuses(&again).contains_key(B), "{again}");
}

/// `crawlOptions` 在创建时校验，不等到每次检查才悄悄失败。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crawl_options_are_checked_when_the_monitor_is_created() {
    let script = Arc::new(Script::default());
    let api = scripted(&script, Config::default());
    for bad in [
        json!({"limit": "many"}),
        json!({"limit": 100_000_000}),
        json!({"includePaths": ["("]}),
        json!({"maxDiscoveryDepth": 100_000_000}),
    ] {
        let body = json!({"targets": [{"type": "crawl", "url": SEED, "crawlOptions": bad}]});
        let (st, v) = call(&api, "POST", "/v2/monitor", Some(body)).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    }
}

/// 404 只报一次 removed；之后还是 404 就是 same，不重复通知；回来了报 new。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gone_page_is_reported_once() {
    let script = Arc::new(Script::default());
    script.page(A, 200, "# A\n\nThe first page body.");
    let api = scripted(&script, Config::default());
    let id = create(&api, scrape_target()).await;
    baseline(&api, &id).await;

    script.page(A, 404, "# Not found");
    assert_eq!(statuses(&run(&api, &id).await)[A], "removed");
    assert_eq!(statuses(&run(&api, &id).await)[A], "same");

    // 引擎直接报 NOT_FOUND 也一样
    script.page(A, 200, "# A\n\nThe first page body.");
    assert_eq!(statuses(&run(&api, &id).await)[A], "new");
    script.fail(A, ErrorCode::NotFound);
    assert_eq!(statuses(&run(&api, &id).await)[A], "removed");
    assert_eq!(statuses(&run(&api, &id).await)[A], "same");
}

/// 5xx、429 这类响应是抓取失败：不算变化，不换掉保存的内容，恢复后比较的还是旧内容。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_error_response_is_not_a_change() {
    let script = Arc::new(Script::default());
    script.page(A, 200, "# A\n\nThe first page body.");
    let api = scripted(&script, Config::default());
    let id = create(&api, scrape_target()).await;
    baseline(&api, &id).await;
    script.page(A, 503, "# Service unavailable\n\nTry again later.");
    let check = run(&api, &id).await;
    assert_eq!(statuses(&check)[A], "error", "{check}");
    assert_eq!(check["status"], "failed", "{check}");
    script.page(A, 200, "# A\n\nThe first page body.");
    assert_eq!(statuses(&run(&api, &id).await)[A], "same");
}

/// 时刻和时区没有实现：明确警告，cron 原样回显，不显示成别的值。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsupported_time_and_timezone_are_warned_about() {
    let script = Arc::new(Script::default());
    let api = scripted(&script, Config::default());
    let (st, v) = call(
        &api,
        "POST",
        "/v2/monitor",
        Some(json!({
            "schedule": {"cron": "30 9 * * *", "timezone": "America/New_York"},
            "targets": [{"type": "scrape", "urls": [A]}],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["schedule"]["cron"], "30 9 * * *", "{v}");
    assert_eq!(v["data"]["schedule"]["timezone"], "UTC", "{v}");
    let w = v["data"]["warnings"].to_string();
    assert!(w.contains("monitor_time_ignored"), "{w}");
    assert!(w.contains("monitor_timezone_ignored: timezone"), "{w}");
    assert!(w.contains("America/New_York"), "{w}");

    // 等间隔的 cron、UTC：没有什么被忽略
    let (_, v) = call(
        &api,
        "POST",
        "/v2/monitor",
        Some(json!({
            "schedule": {"cron": "*/30 * * * *", "timezone": "UTC"},
            "targets": [{"type": "scrape", "urls": [A]}],
        })),
    )
    .await;
    assert!(v["data"]["warnings"].is_null(), "{v}");
}

/// 间隔字段是毫秒，响应也用同样的名字。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn intervals_are_in_milliseconds() {
    let script = Arc::new(Script::default());
    let api = scripted(&script, Config::default());
    let (st, v) = call(
        &api,
        "POST",
        "/v2/monitor",
        Some(json!({
            "schedule": {"minIntervalMs": 600_000, "maxIntervalMs": 3_600_000},
            "targets": [{"type": "scrape", "urls": [A]}],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["schedule"]["minIntervalMs"], 600_000, "{v}");
    assert_eq!(v["data"]["schedule"]["maxIntervalMs"], 3_600_000, "{v}");
}

/// 抓取失败不是“没变”：变化率不动，观测区间不断开，下次到期仍然往后排（不会原地空转）。
#[test]
fn a_failed_fetch_is_not_an_unchanged_observation() {
    let mut m = super::MonitorState {
        id: "m".into(),
        owner: "anonymous".into(),
        req: serde_json::from_value(scrape_target()).unwrap(),
        created_at: String::new(),
        status: "active".into(),
        period: None,
        min_interval: 300,
        max_interval: None,
        webhook: None,
        pages: Default::default(),
        checks: Default::default(),
        last_run_at: None,
        warnings: Vec::new(),
    };
    let mut p = super::PageState::default();
    super::run::observe(&m, &mut p, 1000.0, Some(false));
    super::run::observe(&m, &mut p, 2000.0, Some(false));
    let before = (p.exposure, p.last_check, p.rate_mean);
    super::run::observe(&m, &mut p, 3000.0, None);
    assert_eq!((p.exposure, p.last_check, p.rate_mean), before);
    assert!(p.next_due > 3000.0, "{}", p.next_due);
    m.pages.insert(A.into(), p);
    super::run::allocate(&mut m);
    assert!(m.pages[A].next_due > 3000.0, "{}", m.pages[A].next_due);
    // 恢复后的观测区间从上次成功算起，覆盖失败的那段
    let mut p = m.pages[A].clone();
    let exposure = p.exposure.unwrap();
    super::run::observe(&m, &mut p, 4000.0, Some(false));
    assert_eq!(p.exposure.unwrap(), exposure + 2000.0);
}

/// 后台检查和手动检查都扣调用方的页面配额；配额用完不再抓，检查如实失败。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checks_spend_the_page_quota() {
    let script = Arc::new(Script::default());
    script.page(A, 200, "# A\n\nThe first page body.");
    let mut limits = HashMap::new();
    limits.insert(
        "key-1".to_string(),
        KeyLimits {
            pages_per_day: Some(2),
            ..Default::default()
        },
    );
    let api = scripted(
        &script,
        Config {
            key_limits: limits,
            ..Default::default()
        },
    );
    let (st, v) = call_as(
        &api,
        Some("key-1"),
        "POST",
        "/v2/monitor",
        Some(scrape_target()),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let id = v["data"]["id"].as_str().unwrap().to_string();
    let mut last = Value::Null;
    for _ in 0..5 {
        let (st, v) = call_as(
            &api,
            Some("key-1"),
            "POST",
            &format!("/v2/monitor/{id}/run"),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        last = v["data"].clone();
    }
    assert!(
        script.scrapes.load(Ordering::SeqCst) <= 2,
        "{} pages fetched with a quota of 2",
        script.scrapes.load(Ordering::SeqCst)
    );
    assert_eq!(last["status"], "failed", "{last}");
    assert!(
        last["pages"][0]["error"]
            .as_str()
            .unwrap()
            .contains("QUOTA"),
        "{last}"
    );
}
