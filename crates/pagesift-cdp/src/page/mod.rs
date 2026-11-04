//! 单个页面的渲染流程：建 target、开拦截、导航、执行 actions、取结果。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

mod actions;
mod live;
mod monitor;
mod scroll;
mod settle;

use actions::{ActionError, capture, eval_value, run_action};
use monitor::{
    check_interception, enable_interception, enable_network, monitor, set_metrics, wait_load,
    wait_ready,
};
use scroll::{scan_full_page, virtual_scroll};

pub use live::AxNode;
pub(crate) use live::LivePage;

use crate::capture::{CaptureOptions, Captured, capture_html};
use crate::conn::{Connection, Session};
use crate::error::{Error, Result};
use crate::guard::RequestGuard;
use crate::learn::PageWaits;
use crate::options::*;

const DESKTOP: (u32, u32, f64) = (1920, 1080, 1.0);
const MOBILE: (u32, u32, f64) = (390, 844, 3.0);
pub(super) const MAX_CAPTURE_PX: f64 = 16384.0;
/// 没指定 `languages` 时浏览器发的 Accept-Language：与 pagesift-net 的 HTTP 抓取（`client/send.rs`）一致。
pub const DEFAULT_ACCEPT_LANGUAGE: &str = "en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7";
/// 网络静默多久算就绪。
pub(super) const NET_QUIET: Duration = Duration::from_millis(100);
/// 等网络静默时每隔多久看一次正文：正文已经够多、而且不再变化，就不必等广告和统计脚本结束。
pub(super) const CONTENT_POLL: Duration = Duration::from_millis(250);
/// 页面文字（`innerText`）到这么多字符就算正文已经渲染出来了。
pub(super) const CONTENT_READY_CHARS: u64 = 1500;
/// 网络静默时页面文字少于这么多字符，就像是还没渲染的壳（"loading…"）：脚本可能在用定时器
/// 延后渲染，不发请求，网络静默判断看不出来。
pub(super) const SHELL_MAX_CHARS: u64 = 200;
pub(super) const TEXT_LEN: &str = "(() => { const b = document.body; return b ? (b.innerText || b.textContent || '').length : 0; })()";
/// 这些类型的请求没结束，页面内容就可能还在变；图片、字体、样式不影响 HTML。
pub(super) const TRACKED: &[&str] = &["Document", "Script", "XHR", "Fetch"];
/// 同一个 GET URL 已经请求过这么多次，再请求就当作轮询或心跳，不再等它。
const POLL_REPEATS: u32 = 2;
/// [策略] 一个页面最多记这么多条被拦的请求：够看出页面在拦什么，保留页面上也不会无限涨
const MAX_BLOCKED: usize = 256;
/// [策略] 记录的网址长度上限：浏览器的网址能到 2 MB，诊断用不着（常见网址远短于它）
const MAX_TRACKED_URL: usize = 2048;
/// [策略] 轮询判断最多跟踪这么多个不同的网址：真在轮询的页面只有几个
const MAX_TRACKED_GETS: usize = 4096;

pub(crate) struct PageEnv {
    pub guard: Arc<dyn RequestGuard>,
    pub user_agent: String,
    /// 用 CDP 的 Fetch 拦截逐个检查子请求。内核自己按 CIDR 挡内网时（见
    /// [`crate::PoolConfig::engine_network_guard`]）为 false。
    pub intercept: bool,
    /// 这个内核学到的各类等待上限（见 [`crate::learn`]）。
    pub waits: Arc<PageWaits>,
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
    waits: Arc<PageWaits>,
    /// 上下文代理的认证（用户名，密码）：只用来回答代理的 `Fetch.authRequired`。
    proxy_auth: Option<(String, String)>,
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
        // 轮询的网址不会很长、也不会有几千个；再多的是页面在造请求，不记（不记就当不是轮询）
        if url.len() > MAX_TRACKED_URL
            || (net.gets.len() >= MAX_TRACKED_GETS && !net.gets.contains_key(url))
        {
            return false;
        }
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
                frame_cap: &self.shared.waits.frame,
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

#[derive(Clone, Copy)]
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

/// `keep` 给了时，渲染成功后把页面状态交给它：标签页、事件监听和子请求拦截都继续活着（`/interact`）。
pub(crate) async fn render_page(
    conn: &Connection,
    res: &mut PageResources,
    env: &PageEnv,
    url: &str,
    opts: &RenderOptions,
    timings: &mut Timings,
    keep: Option<&mut Option<LivePage>>,
) -> Result<RenderedPage> {
    let setup_start = Instant::now();
    // 每个页面一个独立的浏览器上下文，cookie 和缓存不会串到别的请求。
    // 指定了出口时上下文走它的代理；建不出来就不渲染，不能悄悄退回直连。
    let proxy = opts
        .proxy
        .as_deref()
        .map(crate::BrowserProxy::parse)
        .transpose()
        .map_err(Error::InvalidOptions)?;
    if proxy.as_ref().is_some_and(|p| p.auth.is_some()) && !env.intercept {
        return Err(Error::InvalidOptions(
            "proxy authentication needs CDP request interception".into(),
        ));
    }
    res.context_id = match &proxy {
        Some(p) => Some(conn.create_browser_context_via(Some(&p.server)).await?),
        None => conn.create_browser_context().await.ok(),
    };
    let proxy_auth = proxy.and_then(|p| p.auth);
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
        // 原样传回浏览器自己的 UA，只改 Accept-Language。没指定语言时用和 HTTP 抓取相同的值，
        // 不随浏览器所在系统的语言变（否则同一页 HTTP 拿英文、浏览器拿中文）。
        let explicit = !opts.languages.is_empty();
        if !explicit && env.user_agent.is_empty() {
            // 不知道浏览器自己的 UA 时不覆盖，免得把 UA 清空
            return Ok(());
        }
        let lang = if explicit {
            opts.languages.join(",")
        } else {
            DEFAULT_ACCEPT_LANGUAGE.to_owned()
        };
        let r = session
            .call(
                "Emulation.setUserAgentOverride",
                json!({ "userAgent": env.user_agent, "acceptLanguage": lang }),
            )
            .await;
        // 调用方指定的语言必须生效；默认值是尽力而为（有的内核不支持这条命令）
        match r {
            Err(e) if explicit => Err(e),
            _ => Ok::<_, Error>(()),
        }
    };
    // 这些命令互不依赖，一起发出去，省掉逐条往返。
    // 拦截打不开就不渲染：不能让子请求绕过检查。空白页不发请求，事件晚点消费也不会丢。
    let (_, tree, ..) = tokio::try_join!(
        session.call("Page.enable", json!({})),
        session.call("Page.getFrameTree", json!({})),
        async {
            if env.intercept {
                enable_interception(&session, proxy_auth.is_some()).await
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
        waits: env.waits.clone(),
        proxy_auth,
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
        WaitUntil::Load | WaitUntil::LoadIdle => "load",
        _ => "DOMContentLoaded",
    };
    tracing::trace!(?loader, first_event, "render: waiting for navigation");
    let mut redirect_ready = false;
    if let Some(l) = &loader {
        // HTTP 已经有正文时，自动升级只是加分项：跨页统计给出导航上界后
        // 才限制等待，没有样本就不猜（热进程里 acquire 可能接近 0，不能拿它
        // 当耐心）。HTTP 正文为 0 时渲染是唯一来源，慢页面也要等，不能为了
        // 一个挑战页把真正的 JS 应用一起砍掉。
        let redirect_page = opts.early_stop.as_ref().is_some_and(|s| s.cap_navigation);
        // 裸跳转页的正文由目标站脚本决定，和它自己的 HTTP 耗时只有松散关系；先验（0.81 的现值）
        // 用调用方的总时限做相对上限，同时不短于 HTTP 耗时的 200 倍。实际耐心按"等到内容出现"的
        // 完成时间学（占先验的比例，见 [`crate::learn`]）。内容一出现就抓，不等 DOMContentLoaded/网络静默。
        let redirect_prior = opts.early_stop.as_ref().and_then(|s| {
            (s.baseline_chars == 0 && s.cap_navigation)
                // [策略] 先验包 v0：0.81 的现值（HTTP 耗时 × 200 与时限的一半取大）
                .then(|| s.http_elapsed.saturating_mul(200).max(opts.timeout / 2))
        });
        let nav_patience = opts.early_stop.as_ref().and_then(|s| {
            if s.baseline_chars > 0 {
                opts.nav_patience
            } else {
                redirect_prior.map(|p| env.waits.redirect.cap_for(p))
            }
        });
        match nav_patience {
            Some(patience) if redirect_page => {
                let waited = Instant::now();
                let prior = redirect_prior.unwrap_or(patience);
                let deadline = waited + patience;
                loop {
                    if shared.has_event(l, first_event) {
                        if redirect_prior.is_some() {
                            env.waits.redirect.done(waited.elapsed(), prior);
                        }
                        break;
                    }
                    let len = tokio::time::timeout(
                        Duration::from_millis(500),
                        eval_value(&session, TEXT_LEN),
                    )
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                    if len > 0 {
                        if redirect_prior.is_some() {
                            env.waits.redirect.done(waited.elapsed(), prior);
                        }
                        redirect_ready = true;
                        break;
                    }
                    if Instant::now() >= deadline {
                        if redirect_prior.is_some() {
                            env.waits.redirect.cut(waited.elapsed(), prior);
                        }
                        return Err(Error::Timeout);
                    }
                    tokio::time::sleep(CONTENT_POLL).await;
                }
            }
            Some(patience) => {
                if tokio::time::timeout(patience, shared.wait_navigation(l, first_event))
                    .await
                    .is_err()
                {
                    return Err(Error::Timeout);
                }
            }
            None => shared.wait_navigation(l, first_event).await,
        }
    }
    tracing::trace!("render: navigation done");
    timings.navigation = nav_start.elapsed();
    check_interception(&shared)?;

    let mut ctx = PageCtx {
        conn,
        session: &session,
        shared: &shared,
        metrics,
        opts,
        virtual_items: BTreeMap::new(),
    };
    let wait_start = Instant::now();
    let ready = if redirect_ready {
        // 裸跳转页的正文已经出现：直接抓当前 DOM，不再等导航完成或网络静默。
        monitor::ReadyOutcome {
            early_stopped: false,
            first_content: Some(timings.navigation),
        }
    } else if matches!(
        opts.wait_until,
        WaitUntil::NetworkIdle | WaitUntil::LoadIdle
    ) {
        wait_ready(&ctx, timings.navigation).await
    } else if opts.wait_until == WaitUntil::MutationIdle {
        settle::wait_mutation_idle(&ctx).await;
        monitor::ReadyOutcome {
            early_stopped: false,
            first_content: None,
        }
    } else {
        monitor::ReadyOutcome {
            early_stopped: false,
            first_content: None,
        }
    };
    let early_stopped = ready.early_stopped;
    let first_content = ready.first_content;
    tracing::trace!("render: ready");
    if !opts.wait_for.is_zero() {
        tokio::time::sleep(opts.wait_for).await;
    }
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
    let PageCtx {
        metrics,
        virtual_items,
        ..
    } = ctx;
    if let Some(slot) = keep {
        *slot = Some(LivePage::new(
            session.clone(),
            shared.clone(),
            metrics,
            virtual_items,
            opts.clone(),
        ));
    }
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
        early_stopped,
        first_content,
        timings: timings.clone(),
    })
}
