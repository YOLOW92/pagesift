//! 单个页面的渲染流程：建 target、开拦截、导航、执行 actions、取结果。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::capture::{CaptureOptions, Captured, capture_html};
use crate::conn::{Connection, EventStream, Session, str_field};
use crate::error::{Error, Result};
use crate::guard::{BlockResources, RequestGuard, SubRequest, Verdict};
use crate::options::*;

const DESKTOP: (u32, u32, f64) = (1920, 1080, 1.0);
const MOBILE: (u32, u32, f64) = (390, 844, 3.0);
const MAX_CAPTURE_PX: f64 = 16384.0;
/// 网络静默多久算就绪。
const NET_QUIET: Duration = Duration::from_millis(100);
/// DOMContentLoaded 之后最多等多久网络静默（长轮询之类的页面永远等不到）。
const NET_IDLE_CAP: Duration = Duration::from_secs(5);
/// 截图前等 `load` 的上限。
const LOAD_CAP: Duration = Duration::from_secs(10);
/// 这些类型的请求没结束，页面内容就可能还在变；图片、字体、样式不影响 HTML。
const TRACKED: &[&str] = &["Document", "Script", "XHR", "Fetch"];

pub(crate) struct PageEnv {
    pub guard: Arc<dyn RequestGuard>,
    pub user_agent: String,
}

/// 渲染过程中创建的浏览器资源，超时被取消后也要能清理。
#[derive(Default)]
pub(crate) struct PageResources {
    context_id: Option<String>,
    target_id: Option<String>,
    monitor: Option<JoinHandle<()>>,
}

impl PageResources {
    pub async fn cleanup(&mut self, conn: &Connection) {
        if let Some(m) = self.monitor.take() {
            m.abort();
        }
        // 销毁上下文会一并关掉其中的 target。
        if let Some(c) = self.context_id.take() {
            let _ = conn.dispose_browser_context(&c).await;
        } else if let Some(t) = self.target_id.take() {
            let _ = conn.close_target(&t).await;
        }
    }
}

struct Net {
    /// (session, requestId)
    inflight: HashSet<(String, String)>,
    last_change: Instant,
}

struct Shared {
    main_frame: String,
    page_session: String,
    status: Mutex<Option<u16>>,
    blocked: Mutex<Vec<(bool, BlockedRequest)>>,
    intercept_error: Mutex<Option<String>>,
    /// 主框架的 (loaderId, 生命周期事件名)。
    lifecycle: Mutex<HashSet<(String, String)>>,
    main_loader: Mutex<Option<String>>,
    load_notify: Notify,
    net: Mutex<Net>,
    /// 跨进程 iframe 的 target id -> session id。
    oopif: Mutex<HashMap<String, String>>,
}

impl Shared {
    fn has_event(&self, loader_id: &str, name: &str) -> bool {
        self.lifecycle
            .lock()
            .unwrap()
            .contains(&(loader_id.to_owned(), name.to_owned()))
    }

    async fn wait_event(&self, loader_id: &str, name: &str) {
        loop {
            let notified = self.load_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.has_event(loader_id, name) {
                return;
            }
            notified.await;
        }
    }

    /// 等到跟踪的请求全部结束并静默 `quiet`；超过 `cap` 返回 false。
    async fn wait_idle(&self, quiet: Duration, cap: Duration) -> bool {
        let deadline = Instant::now() + cap;
        loop {
            let (n, since) = {
                let net = self.net.lock().unwrap();
                (net.inflight.len(), net.last_change.elapsed())
            };
            if n == 0 && since >= quiet {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let nap = if n == 0 {
                quiet - since
            } else {
                Duration::from_millis(20)
            };
            tokio::time::sleep(nap.min(deadline - now)).await;
        }
    }

    fn net_update(&self, f: impl FnOnce(&mut HashSet<(String, String)>) -> bool) {
        let mut net = self.net.lock().unwrap();
        if f(&mut net.inflight) {
            net.last_change = Instant::now();
        }
    }
}

/// 页面级的上下文，actions 和取 HTML 共用。
struct PageCtx<'a> {
    conn: &'a Connection,
    session: &'a Session,
    shared: &'a Shared,
    metrics: Metrics,
    opts: &'a RenderOptions,
    virtual_items: BTreeMap<String, Vec<String>>,
}

impl PageCtx<'_> {
    async fn html(&self) -> Result<Captured> {
        let o = self.opts;
        capture_html(
            self.conn,
            self.session,
            &self.shared.main_frame,
            &self.shared.oopif,
            &CaptureOptions {
                iframes: o.process_iframes,
                max_depth: o.max_iframe_depth,
                max_iframes: o.max_iframes,
                shadow: o.flatten_shadow_dom,
                virtual_items: &self.virtual_items,
            },
        )
        .await
    }

    /// 点击、按键之后可能触发跳转或请求，等页面重新稳定。
    async fn settle(&self) {
        tokio::time::sleep(Duration::from_millis(50)).await;
        self.shared
            .wait_idle(Duration::from_millis(100), Duration::from_secs(3))
            .await;
        let loader = self.shared.main_loader.lock().unwrap().clone();
        if let Some(l) = loader
            && !self.shared.has_event(&l, "DOMContentLoaded")
        {
            let _ = tokio::time::timeout(
                Duration::from_secs(10),
                self.shared.wait_event(&l, "DOMContentLoaded"),
            )
            .await;
            self.shared
                .wait_idle(Duration::from_millis(100), Duration::from_secs(3))
                .await;
        }
    }
}

struct Metrics {
    width: u32,
    height: u32,
    scale: f64,
    mobile: bool,
}

impl Metrics {
    fn new(mobile: bool) -> Metrics {
        let (width, height, scale) = if mobile { MOBILE } else { DESKTOP };
        Metrics {
            width,
            height,
            scale,
            mobile,
        }
    }
}

pub(crate) async fn render_page(
    conn: &Connection,
    res: &mut PageResources,
    env: &PageEnv,
    url: &str,
    opts: &RenderOptions,
    timings: &mut Timings,
) -> Result<RenderedPage> {
    let setup_start = Instant::now();
    // 每个页面一个独立的浏览器上下文，cookie 和缓存不会串到别的请求。
    res.context_id = conn.create_browser_context().await.ok();
    let target = conn
        .create_target("about:blank", res.context_id.as_deref())
        .await?;
    res.target_id = Some(target.clone());
    let session = conn.attach(&target).await?;
    let events = conn.subscribe(None);

    let metrics = Metrics::new(opts.mobile);
    let touch = async {
        if opts.mobile {
            session
                .call(
                    "Emulation.setTouchEmulationEnabled",
                    json!({ "enabled": true, "maxTouchPoints": 5 }),
                )
                .await?;
        }
        Ok(())
    };
    let languages = async {
        if !opts.languages.is_empty() {
            // 原样传回浏览器自己的 UA，只改 Accept-Language。
            session
                .call(
                    "Emulation.setUserAgentOverride",
                    json!({ "userAgent": env.user_agent, "acceptLanguage": opts.languages.join(",") }),
                )
                .await?;
        }
        Ok::<_, Error>(())
    };
    // 这些命令互不依赖，一起发出去，省掉逐条往返。
    // 拦截打不开就不渲染：不能让子请求绕过检查。空白页不发请求，事件晚点消费也不会丢。
    let (_, tree, ..) = tokio::try_join!(
        session.call("Page.enable", json!({})),
        session.call("Page.getFrameTree", json!({})),
        enable_interception(&session),
        session.call("Page.setLifecycleEventsEnabled", json!({ "enabled": true })),
        session.call("Network.enable", json!({})),
        set_metrics(&session, &metrics, None),
        touch,
        languages,
    )?;
    let main_frame = tree
        .pointer("/frameTree/frame/id")
        .and_then(Value::as_str)
        .unwrap_or(&target)
        .to_owned();
    let shared = Arc::new(Shared {
        main_frame,
        page_session: session.id().to_owned(),
        status: Mutex::new(None),
        blocked: Mutex::new(Vec::new()),
        intercept_error: Mutex::new(None),
        lifecycle: Mutex::new(HashSet::new()),
        main_loader: Mutex::new(None),
        load_notify: Notify::new(),
        net: Mutex::new(Net {
            inflight: HashSet::new(),
            last_change: Instant::now(),
        }),
        oopif: Mutex::new(HashMap::new()),
    });
    // 事件已经在 `events` 里缓冲着，晚一点开始消费不会丢。
    res.monitor = Some(tokio::spawn(monitor(
        conn.clone(),
        events,
        shared.clone(),
        env.guard.clone(),
        opts.block.clone(),
    )));

    timings.setup = setup_start.elapsed();

    let nav_start = Instant::now();
    let nav = session.call("Page.navigate", json!({ "url": url })).await?;
    if let Some(err) = nav
        .get("errorText")
        .and_then(Value::as_str)
        .filter(|e| !e.is_empty())
    {
        if let Some((_, b)) = shared
            .blocked
            .lock()
            .unwrap()
            .iter()
            .find(|(main, _)| *main)
        {
            return Err(Error::Blocked {
                url: b.url.clone(),
                reason: b.reason.clone(),
            });
        }
        return Err(Error::Navigation(err.to_owned()));
    }
    let loader = nav
        .get("loaderId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let first_event = match opts.wait_until {
        WaitUntil::Load => "load",
        _ => "DOMContentLoaded",
    };
    if let Some(l) = &loader {
        shared.wait_event(l, first_event).await;
    }
    timings.navigation = nav_start.elapsed();
    check_interception(&shared)?;

    let wait_start = Instant::now();
    if opts.wait_until == WaitUntil::NetworkIdle {
        wait_ready(&session, &shared, NET_IDLE_CAP).await;
    }
    if !opts.wait_for.is_zero() {
        tokio::time::sleep(opts.wait_for).await;
    }
    let mut ctx = PageCtx {
        conn,
        session: &session,
        shared: &shared,
        metrics,
        opts,
        virtual_items: BTreeMap::new(),
    };
    if let Some(scan) = &opts.scan_full_page {
        scan_full_page(&ctx, scan).await?;
    }
    let mut virtual_scroll_items = None;
    if let Some(vs) = &opts.virtual_scroll
        && let Some((key, items)) = virtual_scroll(&session, vs).await?
    {
        virtual_scroll_items = Some(items.len());
        ctx.virtual_items.insert(key, items);
    }
    timings.wait = wait_start.elapsed();
    check_interception(&shared)?;

    let actions_start = Instant::now();
    let mut outputs = ActionOutputs::default();
    for (index, action) in opts.actions.iter().enumerate() {
        run_action(&ctx, action, &mut outputs)
            .await
            .map_err(|e| match e {
                ActionError::Cdp(e) => e,
                ActionError::Failed(message) => Error::Action { index, message },
            })?;
        check_interception(&shared)?;
    }
    timings.actions = actions_start.elapsed();

    let shot_start = Instant::now();
    let screenshot = match &opts.screenshot {
        Some(s) => {
            wait_load(&shared).await;
            Some(capture(&session, &ctx.metrics, s).await?)
        }
        None => None,
    };
    timings.screenshot = shot_start.elapsed();
    let html_start = Instant::now();
    let captured = ctx.html().await?;
    timings.html = html_start.elapsed();
    timings.iframes = captured.iframe_time;
    let final_url = eval_string(&session, "location.href").await?;
    check_interception(&shared)?;
    let status = *shared.status.lock().unwrap();
    let blocked_requests = shared
        .blocked
        .lock()
        .unwrap()
        .drain(..)
        .map(|(_, b)| b)
        .collect();
    Ok(RenderedPage {
        final_url,
        status,
        html: captured.html,
        screenshot,
        actions: outputs,
        blocked_requests,
        iframes_inlined: captured.inlined,
        iframes_skipped: captured.skipped,
        virtual_scroll_items,
        timings: timings.clone(),
    })
}

async fn enable_interception(session: &Session) -> Result<()> {
    session
        .call(
            "Fetch.enable",
            json!({ "patterns": [{ "urlPattern": "*", "requestStage": "Request" }] }),
        )
        .await?;
    // 跨站 iframe 和 worker 是单独的 target，要自动附加上去分别开拦截。
    session
        .call(
            "Target.setAutoAttach",
            json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
        )
        .await?;
    Ok(())
}

fn check_interception(shared: &Shared) -> Result<()> {
    match shared.intercept_error.lock().unwrap().clone() {
        Some(e) => Err(Error::InterceptionUnavailable(e)),
        None => Ok(()),
    }
}

async fn monitor(
    conn: Connection,
    mut events: EventStream,
    shared: Arc<Shared>,
    guard: Arc<dyn RequestGuard>,
    block: BlockResources,
) {
    let mut sessions: HashSet<String> = HashSet::from([shared.page_session.clone()]);
    while let Some(ev) = events.recv().await {
        let Some(sid) = ev.session_id.as_deref() else {
            continue;
        };
        if !sessions.contains(sid) {
            continue;
        }
        let is_page = sid == shared.page_session;
        let p = &ev.params;
        match ev.method.as_str() {
            "Fetch.requestPaused" => {
                let session = conn.session(sid);
                let (shared, guard, block, p) =
                    (shared.clone(), guard.clone(), block.clone(), p.clone());
                tokio::spawn(async move {
                    on_request(&session, &shared, &*guard, &block, &p, is_page).await
                });
            }
            "Target.attachedToTarget" => {
                if let Some(child) = p.get("sessionId").and_then(Value::as_str) {
                    sessions.insert(child.to_owned());
                    let kind = p
                        .pointer("/targetInfo/type")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    if kind == "iframe"
                        && let Some(t) = p.pointer("/targetInfo/targetId").and_then(Value::as_str)
                    {
                        shared
                            .oopif
                            .lock()
                            .unwrap()
                            .insert(t.to_owned(), child.to_owned());
                    }
                    let child = conn.session(child);
                    let shared = shared.clone();
                    tokio::spawn(async move { attach_child(&child, &kind, &shared).await });
                }
            }
            "Target.detachedFromTarget" => {
                if let Some(child) = p.get("sessionId").and_then(Value::as_str) {
                    sessions.remove(child);
                    shared.oopif.lock().unwrap().retain(|_, s| s != child);
                    shared.net_update(|f| {
                        let n = f.len();
                        f.retain(|(s, _)| s != child);
                        n != f.len()
                    });
                }
            }
            "Page.javascriptDialogOpening" if is_page => {
                let session = conn.session(sid);
                tokio::spawn(async move {
                    let _ = session
                        .call("Page.handleJavaScriptDialog", json!({ "accept": false }))
                        .await;
                });
            }
            "Network.responseReceived" if is_page => {
                if p["type"] == "Document" && p["frameId"] == shared.main_frame.as_str() {
                    let status = p.pointer("/response/status").and_then(Value::as_f64);
                    *shared.status.lock().unwrap() = status.map(|s| s as u16);
                }
            }
            "Network.requestWillBeSent" => {
                if TRACKED.contains(&p["type"].as_str().unwrap_or_default())
                    && let Some(id) = p["requestId"].as_str()
                {
                    let key = (sid.to_owned(), id.to_owned());
                    shared.net_update(|f| f.insert(key));
                }
            }
            "Network.loadingFinished" | "Network.loadingFailed" => {
                if let Some(id) = p["requestId"].as_str() {
                    let key = (sid.to_owned(), id.to_owned());
                    shared.net_update(|f| f.remove(&key));
                }
            }
            "Page.lifecycleEvent" if is_page => {
                if p["frameId"] == shared.main_frame.as_str()
                    && let (Some(loader), Some(name)) = (p["loaderId"].as_str(), p["name"].as_str())
                {
                    if name == "init" {
                        *shared.main_loader.lock().unwrap() = Some(loader.to_owned());
                    }
                    shared
                        .lifecycle
                        .lock()
                        .unwrap()
                        .insert((loader.to_owned(), name.to_owned()));
                    shared.load_notify.notify_waiters();
                }
            }
            _ => {}
        }
    }
}

/// 自动附加上来的子 target 在调试器放行前不会发出任何请求。
async fn attach_child(child: &Session, kind: &str, shared: &Shared) {
    if kind == "worker" {
        // 专用 worker 没有 Fetch 域，它的请求由所属页面的拦截处理。
        let _ = child
            .call(
                "Target.setAutoAttach",
                json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
            )
            .await;
    } else if let Err(e) = enable_interception(child).await {
        // 开不了拦截就让它一直停着，并让本次渲染失败。
        *shared.intercept_error.lock().unwrap() = Some(format!("{kind} target: {e}"));
        return;
    } else if kind == "iframe" {
        // 跨进程 iframe 的请求也要计入网络静默。
        let _ = child.call("Network.enable", json!({})).await;
    }
    let _ = child
        .call("Runtime.runIfWaitingForDebugger", json!({}))
        .await;
}

async fn on_request(
    session: &Session,
    shared: &Shared,
    guard: &dyn RequestGuard,
    block: &BlockResources,
    p: &Value,
    is_page: bool,
) {
    let Ok(request_id) = str_field(p, "requestId") else {
        return;
    };
    let resource_type = p["resourceType"].as_str().unwrap_or("Other").to_owned();
    let req = SubRequest {
        url: p
            .pointer("/request/url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        method: p
            .pointer("/request/method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_owned(),
        is_main_document: is_page
            && resource_type == "Document"
            && p["frameId"] == shared.main_frame.as_str(),
        resource_type,
    };
    let verdict = match block.reason(&req) {
        Some(r) => Verdict::Block(r.to_owned()),
        None => guard.check(&req).await,
    };
    match verdict {
        Verdict::Allow => {
            let _ = session
                .call("Fetch.continueRequest", json!({ "requestId": request_id }))
                .await;
        }
        Verdict::Block(reason) => {
            tracing::debug!(url = %req.url, %reason, "blocked sub-request");
            shared.blocked.lock().unwrap().push((
                req.is_main_document,
                BlockedRequest {
                    url: req.url,
                    resource_type: req.resource_type,
                    reason,
                },
            ));
            let _ = session
                .call(
                    "Fetch.failRequest",
                    json!({ "requestId": request_id, "errorReason": "BlockedByClient" }),
                )
                .await;
        }
    }
}

async fn set_metrics(session: &Session, m: &Metrics, viewport: Option<Viewport>) -> Result<()> {
    let (w, h) = viewport.map_or((m.width, m.height), |v| (v.width, v.height));
    session
        .call(
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": w, "height": h, "deviceScaleFactor": m.scale, "mobile": m.mobile,
                "screenWidth": w, "screenHeight": h,
            }),
        )
        .await
        .map(drop)
}

enum ActionError {
    Cdp(Error),
    Failed(String),
}

impl From<Error> for ActionError {
    fn from(e: Error) -> Self {
        ActionError::Cdp(e)
    }
}

struct RemoteValue {
    kind: String,
    value: Value,
}

/// 执行一段表达式。外层错误是协议错误，内层是页面里的 JS 异常。
async fn eval(session: &Session, expr: &str) -> Result<Result<RemoteValue, String>> {
    let mut attempt = 0;
    loop {
        let r = session
            .call(
                "Runtime.evaluate",
                json!({ "expression": expr, "returnByValue": true, "awaitPromise": true, "userGesture": true }),
            )
            .await;
        match r {
            // 页面刚跳转时旧的执行上下文会失效，稍等再试。
            Err(Error::Protocol { ref message, .. })
                if message.contains("context") && attempt < 50 =>
            {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => return Err(e),
            Ok(v) => {
                if let Some(ex) = v.get("exceptionDetails") {
                    let msg = ex
                        .pointer("/exception/description")
                        .or_else(|| ex.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("JavaScript exception");
                    return Ok(Err(msg.to_owned()));
                }
                return Ok(Ok(RemoteValue {
                    kind: v
                        .pointer("/result/type")
                        .and_then(Value::as_str)
                        .unwrap_or("undefined")
                        .to_owned(),
                    value: v.pointer("/result/value").cloned().unwrap_or(Value::Null),
                }));
            }
        }
    }
}

async fn eval_ok(session: &Session, expr: &str) -> Result<Value, ActionError> {
    eval(session, expr)
        .await?
        .map(|v| v.value)
        .map_err(ActionError::Failed)
}

async fn eval_string(session: &Session, expr: &str) -> Result<String> {
    match eval(session, expr).await? {
        Ok(v) => Ok(v.value.as_str().unwrap_or_default().to_owned()),
        Err(e) => Err(Error::UnexpectedResponse(e)),
    }
}

/// 网络静默后再在页面里跑一条空命令：它要排在正在执行的脚本之后，
/// 长脚本执行完又发出的请求就不会被误判成已经空闲。
async fn wait_ready(session: &Session, shared: &Shared, cap: Duration) {
    let deadline = Instant::now() + cap;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if !shared.wait_idle(NET_QUIET, left).await {
            return;
        }
        let probe = Instant::now();
        let left = deadline.saturating_duration_since(probe);
        if tokio::time::timeout(left, eval(session, "0"))
            .await
            .is_err()
        {
            return;
        }
        let net = shared.net.lock().unwrap();
        if net.inflight.is_empty() && net.last_change < probe {
            return;
        }
    }
}

/// 截图要等图片和字体；主框架当前文档的 `load` 最多等 `LOAD_CAP`。
async fn wait_load(shared: &Shared) {
    let loader = shared.main_loader.lock().unwrap().clone();
    if let Some(l) = loader {
        let _ = tokio::time::timeout(LOAD_CAP, shared.wait_event(&l, "load")).await;
    }
}

async fn scan_full_page(ctx: &PageCtx<'_>, scan: &FullPageScan) -> Result<()> {
    const STEP: &str = "(() => { window.scrollBy(0, window.innerHeight); \
        const se = document.scrollingElement || document.documentElement; \
        return [window.scrollY + window.innerHeight, se.scrollHeight]; })()";
    const POS: &str = "(() => { const se = document.scrollingElement || document.documentElement; \
        return [window.scrollY + window.innerHeight, se.scrollHeight]; })()";
    let pos =
        |v: Value| -> (f64, f64) { (v[0].as_f64().unwrap_or(0.0), v[1].as_f64().unwrap_or(0.0)) };
    let start = Instant::now();
    let mut last_height = pos(eval_value(ctx.session, POS).await?).1;
    for _ in 0..scan.max_steps {
        if start.elapsed() >= scan.timeout {
            break;
        }
        eval_value(ctx.session, STEP).await?;
        // 给懒加载一点时间发出请求，再等它们结束。
        tokio::time::sleep(Duration::from_millis(50)).await;
        ctx.shared
            .wait_idle(Duration::from_millis(100), scan.scroll_delay)
            .await;
        let (bottom, height) = pos(eval_value(ctx.session, POS).await?);
        if height > last_height {
            last_height = height;
            continue;
        }
        if bottom >= height - 2.0 {
            break;
        }
    }
    eval_value(ctx.session, "window.scrollTo(0, 0)").await?;
    Ok(())
}

const VIRTUAL_JS: &str = r#"(async (sel, count, by, waitMs) => {
  const c = document.querySelector(sel);
  if (!c) return null;
  const quiet = max => new Promise(res => {
    let t = null;
    const done = () => { ob.disconnect(); clearTimeout(cap); clearTimeout(t); res(); };
    const ob = new MutationObserver(() => { clearTimeout(t); t = setTimeout(done, 50); });
    ob.observe(c, { childList: true, subtree: true, characterData: true });
    const cap = setTimeout(done, max);
  });
  const seen = new Set(), items = [];
  const grab = () => {
    for (const ch of c.children) {
      const k = (ch.textContent || '').replace(/\s+/g, ' ').trim() || ch.outerHTML;
      if (!seen.has(k)) { seen.add(k); items.push(ch.outerHTML); }
    }
  };
  const own = c.scrollHeight > c.clientHeight + 1 && getComputedStyle(c).overflowY !== 'visible';
  grab();
  for (let i = 0; i < count; i++) {
    const step = by === 'container' ? (own ? c.clientHeight : innerHeight) : by === 'page' ? innerHeight : by;
    const before = own ? c.scrollTop : scrollY;
    const wait = quiet(waitMs);
    if (own) c.scrollTop += step; else window.scrollBy(0, step);
    await wait;
    grab();
    if ((own ? c.scrollTop : scrollY) === before) break;
  }
  const key = Math.random().toString(36).slice(2);
  c.setAttribute('data-pagesift-vs', key);
  return { key, items };
})"#;

/// 返回容器标记和按出现顺序去重后的条目；找不到容器时返回 `None`。
async fn virtual_scroll(
    session: &Session,
    vs: &VirtualScroll,
) -> Result<Option<(String, Vec<String>)>> {
    let by = match vs.scroll_by {
        ScrollBy::ContainerHeight => json!("container"),
        ScrollBy::PageHeight => json!("page"),
        ScrollBy::Pixels(px) => json!(px),
    };
    let expr = format!(
        "{VIRTUAL_JS}({}, {}, {by}, {})",
        js_str(&vs.container_selector),
        vs.scroll_count,
        vs.wait_after_scroll.as_millis()
    );
    let v = eval_value(session, &expr).await?;
    if v.is_null() {
        return Ok(None);
    }
    let key = v["key"].as_str().unwrap_or_default().to_owned();
    let items = v["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| i.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Ok(Some((key, items)))
}

async fn eval_value(session: &Session, expr: &str) -> Result<Value> {
    match eval(session, expr).await? {
        Ok(v) => Ok(v.value),
        Err(e) => Err(Error::UnexpectedResponse(e)),
    }
}

fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

async fn run_action(
    ctx: &PageCtx<'_>,
    action: &Action,
    out: &mut ActionOutputs,
) -> Result<(), ActionError> {
    let session = ctx.session;
    match action {
        Action::Wait {
            milliseconds: Some(ms),
            ..
        } => {
            tokio::time::sleep(Duration::from_millis(*ms)).await;
        }
        Action::Wait {
            selector: Some(sel),
            ..
        } => {
            // 页面里用 MutationObserver 等，最长 1 秒一轮；跳转后上下文失效时重来。
            let expr = format!(
                "new Promise(r => {{ const q = () => !!document.querySelector({}); \
                 if (q()) return r(true); const ob = new MutationObserver(() => {{ if (q()) {{ ob.disconnect(); r(true); }} }}); \
                 ob.observe(document, {{ childList: true, subtree: true, attributes: true }}); \
                 setTimeout(() => {{ ob.disconnect(); r(q()); }}, 1000); }})",
                js_str(sel)
            );
            while eval_ok(session, &expr).await? != Value::Bool(true) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        Action::Wait { .. } => {
            return Err(ActionError::Failed(
                "wait needs milliseconds or selector".into(),
            ));
        }
        Action::Click { selector, all } => {
            let sel = js_str(selector);
            let count = eval_ok(session, &format!("document.querySelectorAll({sel}).length"))
                .await?
                .as_u64()
                .unwrap_or(0);
            if count == 0 {
                return Err(ActionError::Failed(format!(
                    "no element matches selector {selector}"
                )));
            }
            let n = if *all { count } else { 1 };
            for i in 0..n {
                click_nth(session, &sel, i).await?;
            }
            ctx.settle().await;
        }
        Action::Write { text } => {
            session
                .call("Input.insertText", json!({ "text": text }))
                .await?;
        }
        Action::Press { key } => {
            let k = key_def(key)
                .ok_or_else(|| ActionError::Failed(format!("unsupported key `{key}`")))?;
            let mut down = json!({
                "type": "keyDown", "key": k.key, "code": k.code,
                "windowsVirtualKeyCode": k.vk, "nativeVirtualKeyCode": k.vk,
            });
            if let Some(t) = &k.text {
                down["text"] = json!(t);
                down["unmodifiedText"] = json!(t);
            }
            session.call("Input.dispatchKeyEvent", down).await?;
            session
                .call(
                    "Input.dispatchKeyEvent",
                    json!({ "type": "keyUp", "key": k.key, "code": k.code,
                            "windowsVirtualKeyCode": k.vk, "nativeVirtualKeyCode": k.vk }),
                )
                .await?;
            ctx.settle().await;
        }
        Action::Scroll {
            direction,
            selector,
        } => {
            let sign = if *direction == ScrollDirection::Up {
                "-"
            } else {
                ""
            };
            let expr = match selector {
                Some(sel) => format!(
                    "(() => {{ const e = document.querySelector({}); if (!e) return false; \
                     e.scrollBy(0, {sign}e.clientHeight); return true; }})()",
                    js_str(sel)
                ),
                None => format!("(window.scrollBy(0, {sign}window.innerHeight), true)"),
            };
            if eval_ok(session, &expr).await? != Value::Bool(true) {
                return Err(ActionError::Failed(
                    "no element matches scroll selector".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            ctx.shared
                .wait_idle(Duration::from_millis(100), Duration::from_secs(3))
                .await;
        }
        Action::Screenshot(s) => {
            wait_load(ctx.shared).await;
            out.screenshots
                .push(capture(session, &ctx.metrics, s).await?);
        }
        Action::Scrape => {
            let html = ctx.html().await?.html;
            let url = eval_string(session, "location.href").await?;
            out.scrapes.push(ScrapeResult { url, html });
        }
        Action::ExecuteJavascript { script } => {
            let v = eval(session, script).await?.map_err(ActionError::Failed)?;
            out.javascript_returns.push(JavascriptReturn {
                kind: v.kind,
                value: v.value,
            });
        }
        Action::Pdf {
            format,
            landscape,
            scale,
        } => {
            wait_load(ctx.shared).await;
            let (pw, ph) = format.inches();
            let r = session
                .call(
                    "Page.printToPDF",
                    json!({
                        "landscape": landscape, "scale": scale.unwrap_or(1.0),
                        "paperWidth": pw, "paperHeight": ph, "printBackground": true,
                    }),
                )
                .await?;
            out.pdfs.push(decode(&r)?);
        }
    }
    Ok(())
}

async fn click_nth(session: &Session, sel: &str, i: u64) -> Result<(), ActionError> {
    let expr = format!(
        "(() => {{ const e = document.querySelectorAll({sel})[{i}]; if (!e) return null; \
         e.scrollIntoView({{block: 'center', inline: 'center'}}); const r = e.getBoundingClientRect(); \
         if (r.width === 0 && r.height === 0) {{ e.click(); return []; }} \
         return [r.x + r.width / 2, r.y + r.height / 2]; }})()"
    );
    let v = eval_ok(session, &expr).await?;
    let Some(pt) = v.as_array() else {
        return Err(ActionError::Failed(
            "element disappeared before click".into(),
        ));
    };
    let (Some(x), Some(y)) = (
        pt.first().and_then(Value::as_f64),
        pt.get(1).and_then(Value::as_f64),
    ) else {
        return Ok(());
    };
    for ty in ["mouseMoved", "mousePressed", "mouseReleased"] {
        session
            .call(
                "Input.dispatchMouseEvent",
                json!({ "type": ty, "x": x, "y": y, "button": "left", "clickCount": 1 }),
            )
            .await?;
    }
    Ok(())
}

async fn capture(
    session: &Session,
    metrics: &Metrics,
    s: &ScreenshotOptions,
) -> Result<Screenshot> {
    if s.viewport.is_some() {
        set_metrics(session, metrics, s.viewport).await?;
    }
    let shot = capture_inner(session, s).await;
    if s.viewport.is_some() {
        set_metrics(session, metrics, None).await?;
    }
    shot
}

async fn capture_inner(session: &Session, s: &ScreenshotOptions) -> Result<Screenshot> {
    let format = if s.quality.is_some() {
        ImageFormat::Jpeg
    } else {
        ImageFormat::Png
    };
    let mut params = json!({
        "format": if format == ImageFormat::Jpeg { "jpeg" } else { "png" },
        "fromSurface": true,
    });
    if let Some(q) = s.quality {
        params["quality"] = json!(q);
    }
    if s.full_page {
        let m = session.call("Page.getLayoutMetrics", json!({})).await?;
        let size = m
            .get("cssContentSize")
            .or_else(|| m.get("contentSize"))
            .cloned()
            .unwrap_or_default();
        let w = size["width"]
            .as_f64()
            .unwrap_or(0.0)
            .ceil()
            .clamp(1.0, MAX_CAPTURE_PX);
        let h = size["height"]
            .as_f64()
            .unwrap_or(0.0)
            .ceil()
            .clamp(1.0, MAX_CAPTURE_PX);
        params["captureBeyondViewport"] = json!(true);
        params["clip"] = json!({ "x": 0, "y": 0, "width": w, "height": h, "scale": 1 });
    }
    let r = session.call("Page.captureScreenshot", params).await?;
    Ok(Screenshot {
        format,
        data: decode(&r)?,
    })
}

fn decode(r: &Value) -> Result<Vec<u8>> {
    let data = str_field(r, "data")?;
    B64.decode(data.as_bytes())
        .map_err(|e| Error::UnexpectedResponse(format!("bad base64: {e}")))
}

struct KeyDef {
    key: String,
    code: String,
    vk: u32,
    text: Option<String>,
}

fn key_def(name: &str) -> Option<KeyDef> {
    let named = |key: &str, code: &str, vk: u32, text: Option<&str>| KeyDef {
        key: key.into(),
        code: code.into(),
        vk,
        text: text.map(Into::into),
    };
    Some(match name {
        "Enter" => named("Enter", "Enter", 13, Some("\r")),
        "Tab" => named("Tab", "Tab", 9, None),
        "Escape" | "Esc" => named("Escape", "Escape", 27, None),
        "Backspace" => named("Backspace", "Backspace", 8, None),
        "Delete" => named("Delete", "Delete", 46, None),
        "Space" | " " => named(" ", "Space", 32, Some(" ")),
        "ArrowUp" => named("ArrowUp", "ArrowUp", 38, None),
        "ArrowDown" => named("ArrowDown", "ArrowDown", 40, None),
        "ArrowLeft" => named("ArrowLeft", "ArrowLeft", 37, None),
        "ArrowRight" => named("ArrowRight", "ArrowRight", 39, None),
        "Home" => named("Home", "Home", 36, None),
        "End" => named("End", "End", 35, None),
        "PageUp" => named("PageUp", "PageUp", 33, None),
        "PageDown" => named("PageDown", "PageDown", 34, None),
        _ => {
            let mut chars = name.chars();
            let c = chars.next()?;
            if chars.next().is_some() || !c.is_ascii_alphanumeric() {
                return None;
            }
            let up = c.to_ascii_uppercase();
            let code = if c.is_ascii_digit() {
                format!("Digit{c}")
            } else {
                format!("Key{up}")
            };
            KeyDef {
                key: c.to_string(),
                code,
                vk: up as u32,
                text: Some(c.to_string()),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(key_def("Enter").unwrap().vk, 13);
        let a = key_def("a").unwrap();
        assert_eq!(
            (a.code.as_str(), a.vk, a.text.as_deref()),
            ("KeyA", 65, Some("a"))
        );
        assert_eq!(key_def("7").unwrap().code, "Digit7");
        assert!(key_def("F13").is_none());
        assert!(key_def("ä").is_none());
    }
}
