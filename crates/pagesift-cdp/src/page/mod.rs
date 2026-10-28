//! 单个页面的渲染流程：建 target、开拦截、导航、执行 actions、取结果。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

mod actions;
mod monitor;

use actions::{ActionError, capture, run_action};
use monitor::{
    check_interception, enable_interception, enable_network, monitor, scan_full_page, set_metrics,
    virtual_scroll, wait_load, wait_ready,
};

use crate::capture::{CaptureOptions, Captured, capture_html};
use crate::conn::{Connection, Session};
use crate::error::{Error, Result};
use crate::guard::RequestGuard;
use crate::options::*;

const DESKTOP: (u32, u32, f64) = (1920, 1080, 1.0);
const MOBILE: (u32, u32, f64) = (390, 844, 3.0);
pub(super) const MAX_CAPTURE_PX: f64 = 16384.0;
/// 网络静默多久算就绪。
pub(super) const NET_QUIET: Duration = Duration::from_millis(100);
/// DOMContentLoaded 之后最多等多久网络静默（长轮询之类的页面永远等不到）。
const NET_IDLE_CAP: Duration = Duration::from_secs(5);
/// 等网络静默时每隔多久看一次正文：正文已经够多、而且不再变化，就不必等广告和统计脚本结束。
pub(super) const CONTENT_POLL: Duration = Duration::from_millis(250);
/// 页面文字（`innerText`）到这么多字符就算正文已经渲染出来了。
pub(super) const CONTENT_READY_CHARS: u64 = 1500;
/// 网络静默时页面文字少于这么多字符，就像是还没渲染的壳（"loading…"）：脚本可能在用定时器
/// 延后渲染，不发请求，网络静默判断看不出来。
pub(super) const SHELL_MAX_CHARS: u64 = 200;
/// 壳页面在网络静默之后再等多久，期间每隔 [`CONTENT_POLL`] 看一次文字；正常页面不受影响。
pub(super) const SHELL_GRACE: Duration = Duration::from_millis(1500);
pub(super) const TEXT_LEN: &str = "(() => { const b = document.body; return b ? (b.innerText || b.textContent || '').length : 0; })()";
/// 截图前等 `load` 的上限。
pub(super) const LOAD_CAP: Duration = Duration::from_secs(10);
/// 这些类型的请求没结束，页面内容就可能还在变；图片、字体、样式不影响 HTML。
pub(super) const TRACKED: &[&str] = &["Document", "Script", "XHR", "Fetch"];
/// 同一个 GET URL 已经请求过这么多次，再请求就当作轮询或心跳，不再等它。
const POLL_REPEATS: u32 = 2;

pub(crate) struct PageEnv {
    pub guard: Arc<dyn RequestGuard>,
    pub user_agent: String,
    /// 用 CDP 的 Fetch 拦截逐个检查子请求。内核自己按 CIDR 挡内网时（见
    /// [`crate::PoolConfig::engine_network_guard`]）为 false。
    pub intercept: bool,
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
    /// (session, requestId) -> URL
    inflight: HashMap<(String, String), String>,
    last_change: Instant,
    /// 每个 GET URL 发过几次，用来认出轮询。
    gets: HashMap<String, u32>,
}

struct Shared {
    intercept: bool,
    main_frame: String,
    page_session: String,
    status: Mutex<Option<u16>>,
    content_type: Mutex<Option<String>>,
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

    pub(super) async fn wait_event(&self, loader_id: &str, name: &str) {
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

    /// 等导航到达 `name`。页面在 DOMContentLoaded 之前就用脚本跳走时，原来的文档永远等不到这个事件，
    /// 这时改等主框架当前的文档。
    async fn wait_navigation(&self, loader_id: &str, name: &str) {
        loop {
            let notified = self.load_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let current = self.main_loader.lock().unwrap().clone();
            if self.has_event(loader_id, name)
                || current.is_some_and(|c| c != loader_id && self.has_event(&c, name))
            {
                return;
            }
            notified.await;
        }
    }

    /// 等到跟踪的请求全部结束并静默 `quiet`；超过 `cap` 返回 false。
    pub(super) async fn wait_idle(&self, quiet: Duration, cap: Duration) -> bool {
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

    /// 记一次 GET；这个 URL 之前已经请求过 `POLL_REPEATS` 次时认定是轮询，返回 true，
    /// 同时不再等它之前还没结束的几次（长轮询会一直挂着）。
    pub(super) fn is_polling(&self, url: &str) -> bool {
        let mut net = self.net.lock().unwrap();
        let n = net.gets.entry(url.to_owned()).or_default();
        *n += 1;
        if *n <= POLL_REPEATS {
            return false;
        }
        let before = net.inflight.len();
        net.inflight.retain(|_, u| u != url);
        if net.inflight.len() != before {
            net.last_change = Instant::now();
        }
        true
    }

    pub(super) fn net_update(
        &self,
        f: impl FnOnce(&mut HashMap<(String, String), String>) -> bool,
    ) {
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
    pub(super) async fn html(&self) -> Result<Captured> {
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
    pub(super) async fn settle(&self) {
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
    let events = conn.subscribe_tree(session.id());

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
        async {
            if env.intercept {
                enable_interception(&session).await
            } else {
                // 不拦截也要自动附加子 target，好跟踪它们的网络和 iframe
                session
                    .call(
                        "Target.setAutoAttach",
                        json!({ "autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true }),
                    )
                    .await
                    .map(|_| ())
            }
        },
        session.call("Page.setLifecycleEventsEnabled", json!({ "enabled": true })),
        enable_network(&session),
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
        intercept: env.intercept,
        main_frame,
        page_session: session.id().to_owned(),
        status: Mutex::new(None),
        content_type: Mutex::new(None),
        blocked: Mutex::new(Vec::new()),
        intercept_error: Mutex::new(None),
        lifecycle: Mutex::new(HashSet::new()),
        main_loader: Mutex::new(None),
        load_notify: Notify::new(),
        net: Mutex::new(Net {
            inflight: HashMap::new(),
            last_change: Instant::now(),
            gets: HashMap::new(),
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
    tracing::trace!(?loader, first_event, "render: waiting for navigation");
    if let Some(l) = &loader {
        shared.wait_navigation(l, first_event).await;
    }
    tracing::trace!("render: navigation done");
    timings.navigation = nav_start.elapsed();
    check_interception(&shared)?;

    let wait_start = Instant::now();
    if opts.wait_until == WaitUntil::NetworkIdle {
        wait_ready(&session, &shared, NET_IDLE_CAP).await;
    }
    tracing::trace!("render: ready");
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
    tracing::trace!("render: capturing html");
    let captured = ctx.html().await?;
    tracing::trace!("render: html captured");
    timings.html = html_start.elapsed();
    timings.iframes = captured.iframe_time;
    let final_url = captured.url;
    // 页面用脚本跳到了打不开的地址：剩下的是浏览器自己的错误页，不是正文。
    if final_url.starts_with("chrome-error:") {
        return Err(Error::Navigation(
            "the page navigated to an address that could not be loaded".into(),
        ));
    }
    check_interception(&shared)?;
    let status = *shared.status.lock().unwrap();
    let content_type = shared.content_type.lock().unwrap().clone();
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
        content_type,
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
