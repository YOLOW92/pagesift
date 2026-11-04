use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::conn::{Connection, EventStream, Session, str_field};
use crate::error::{Error, Result};
use crate::guard::{BlockResources, RequestGuard, SubRequest, Verdict};
use crate::options::*;

use super::actions::{eval, eval_value};
use super::{
    CONTENT_POLL, CONTENT_READY_CHARS, Metrics, NET_QUIET, PageCtx, SHELL_MAX_CHARS, Shared,
    TEXT_LEN, TRACKED,
};

/// 只用网络事件判断就绪，不读响应体：缓冲区设到最小，浏览器不必替每个请求保留一份内容。
pub(super) async fn enable_network(session: &Session) -> Result<Value> {
    session
        .call(
            "Network.enable",
            json!({ "maxTotalBufferSize": 0, "maxResourceBufferSize": 0 }),
        )
        .await
}

/// `proxy_auth`：上下文走的代理要认证时打开 `handleAuthRequests`，由 [`monitor`] 回答。
pub(super) async fn enable_interception(session: &Session, proxy_auth: bool) -> Result<()> {
    // 两条互不依赖，一起发；都完成之前不会放行导航或子 target。
    tokio::try_join!(
        session.call(
            "Fetch.enable",
            json!({
                "patterns": [{ "urlPattern": "*", "requestStage": "Request" }],
                "handleAuthRequests": proxy_auth,
            }),
        ),
        // 跨站 iframe 和 worker 是单独的 target，要自动附加上去分别开拦截。
        session.call(
            "Target.setAutoAttach",
            json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
        ),
    )?;
    Ok(())
}

pub(super) fn check_interception(shared: &Shared) -> Result<()> {
    match shared.intercept_error.lock().unwrap().clone() {
        Some(e) => Err(Error::InterceptionUnavailable(e)),
        None => Ok(()),
    }
}

pub(super) async fn monitor(
    conn: Connection,
    mut events: EventStream,
    shared: Arc<Shared>,
    guard: Arc<dyn RequestGuard>,
    block: BlockResources,
) {
    // 订阅只含这个页面和它的子 target 的 session（见 `Connection::subscribe_tree`）。
    while let Some(ev) = events.recv().await {
        let Some(sid) = ev.session_id.as_deref() else {
            continue;
        };
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
            "Fetch.authRequired" => {
                // 只回答代理的质询；网站自己的认证（401）一律取消，和没有界面的浏览器一样
                let session = conn.session(sid);
                let response = match (&shared.proxy_auth, p.pointer("/authChallenge/source")) {
                    (Some((user, pass)), Some(src)) if src == "Proxy" => json!({
                        "response": "ProvideCredentials", "username": user, "password": pass,
                    }),
                    _ => json!({ "response": "CancelAuth" }),
                };
                let request_id = p["requestId"].clone();
                tokio::spawn(async move {
                    let _ = session
                        .call(
                            "Fetch.continueWithAuth",
                            json!({ "requestId": request_id, "authChallengeResponse": response }),
                        )
                        .await;
                });
            }
            "Target.attachedToTarget" => {
                if let Some(child) = p.get("sessionId").and_then(Value::as_str) {
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
                    shared.oopif.lock().unwrap().retain(|_, s| s != child);
                    shared.net_update(|f| {
                        let n = f.len();
                        f.retain(|(s, _), _| s != child);
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
                    *shared.content_type.lock().unwrap() = p
                        .pointer("/response/mimeType")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
            }
            "Network.requestWillBeSent" => {
                let url = p.pointer("/request/url").and_then(Value::as_str);
                // blob: 和 data: 不走网络；worker 的 blob: 主脚本在页面 session 上只有开始事件，
                // 结束事件发在 worker 自己的 session 上，算进来就永远等不到静默。
                let local = url.is_some_and(|u| u.starts_with("blob:") || u.starts_with("data:"));
                // 定时重复请求同一个地址的是轮询，等它静默要一直等到上限；重定向的后续跳不算新请求。
                let polling = || {
                    p.pointer("/request/method").and_then(Value::as_str) == Some("GET")
                        && p.get("redirectResponse").is_none()
                        && url.is_some_and(|u| shared.is_polling(u))
                };
                if !local
                    && TRACKED.contains(&p["type"].as_str().unwrap_or_default())
                    && !polling()
                    && let (Some(id), Some(url)) = (p["requestId"].as_str(), url)
                {
                    let key = (sid.to_owned(), id.to_owned());
                    shared.net_update(|f| f.insert(key, url.to_owned()).is_none());
                }
            }
            "Network.loadingFinished" | "Network.loadingFailed" => {
                if let Some(id) = p["requestId"].as_str() {
                    let key = (sid.to_owned(), id.to_owned());
                    shared.net_update(|f| f.remove(&key).is_some());
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
pub(super) async fn attach_child(child: &Session, kind: &str, shared: &Shared) {
    if kind == "worker" {
        // 专用 worker 没有 Fetch 域，它的请求由所属页面的拦截处理。
        let _ = child
            .call(
                "Target.setAutoAttach",
                json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
            )
            .await;
    } else if !shared.intercept {
        if kind == "iframe" {
            let _ = enable_network(child).await;
        }
    } else if let Err(e) = enable_interception(child, shared.proxy_auth.is_some()).await {
        // 开不了拦截就让它一直停着，并让本次渲染失败。
        *shared.intercept_error.lock().unwrap() = Some(format!("{kind} target: {e}"));
        return;
    } else if kind == "iframe" {
        // 跨进程 iframe 的请求也要计入网络静默。
        let _ = enable_network(child).await;
    }
    let _ = child
        .call("Runtime.runIfWaitingForDebugger", json!({}))
        .await;
}

pub(super) async fn on_request(
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
            // 保留页面可以开很久：页面反复请求被拦的长网址，不能让这张表一直涨
            {
                let mut blocked = shared.blocked.lock().unwrap();
                if blocked.len() < super::MAX_BLOCKED {
                    let mut url = req.url;
                    if url.len() > super::MAX_TRACKED_URL {
                        let mut cut = super::MAX_TRACKED_URL;
                        while !url.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        url.truncate(cut);
                    }
                    blocked.push((
                        req.is_main_document,
                        BlockedRequest {
                            url,
                            resource_type: req.resource_type,
                            reason,
                        },
                    ));
                }
            }
            let _ = session
                .call(
                    "Fetch.failRequest",
                    json!({ "requestId": request_id, "errorReason": "BlockedByClient" }),
                )
                .await;
        }
    }
}

pub(super) async fn set_metrics(
    session: &Session,
    m: &Metrics,
    viewport: Option<Viewport>,
) -> Result<()> {
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

/// 网络静默后再在页面里跑一条空命令：它要排在正在执行的脚本之后，
/// 长脚本执行完又发出的请求就不会被误判成已经空闲。
/// 等页面就绪：网络静默，或者正文已经够多且两次检查之间没有变化（服务端渲染的页面，
/// 剩下的请求多是广告和统计）。网络静默时页面几乎没有文字的，再给 [`SHELL_GRACE`]：
/// 文字出来就返回（定时器延后渲染的页面）。最多等 `cap`。
pub(super) struct ReadyOutcome {
    pub early_stopped: bool,
    pub first_content: Option<Duration>,
}

/// 等页面就绪：网络静默，或者正文已经够多且两次检查之间没有变化（服务端渲染的页面，
/// 剩下的请求多是广告和统计）。网络静默时页面几乎没有文字的，再给一段壳页面宽限：
/// 文字出来就返回（定时器延后渲染的页面）。
///
/// 两个上限都从完成时间学（[`crate::learn`]，先验是 0.81 的 5 s 和 1.5 s）：
/// - 就绪上限：就绪（网络静默或正文稳定）= 完成；到上限、探针早停 = 截尾。
/// - 壳页面宽限：宽限内文字出现 = 完成；宽限用完或到就绪上限 = 截尾。
///
/// 自动升级（`opts.early_stop`）时，页面文字稳定或到了历史学到的探针时间，
/// 就抓一份当前 DOM 交给调用方的 `probe`；probe 认为不会比 HTTP 结果更好时提前结束。
/// 这里比较的是最终正文提取结果，不是原始 `innerText`，导航栏和广告不会被误当成增益。
pub(super) async fn wait_ready(ctx: &PageCtx<'_>, nav_elapsed: Duration) -> ReadyOutcome {
    let session = ctx.session;
    let shared = ctx.shared;
    let ready = &shared.waits.net_idle;
    let grace = &shared.waits.shell_grace;
    let (ready_prior, grace_prior) = (ready.prior(), grace.prior());
    let grace_cap = grace.cap();
    let started = Instant::now();
    let deadline = started + ready.cap();
    // 就绪等待记过样本没有；壳页面宽限开始的时刻
    let mut ready_noted = false;
    let mut note_ready = |done: bool| {
        if !std::mem::replace(&mut ready_noted, true) {
            if done {
                ready.done(started.elapsed(), ready_prior);
            } else {
                ready.cut(started.elapsed(), ready_prior);
            }
        }
    };
    let mut grace_from: Option<Instant> = None;
    let note_grace = |from: Option<Instant>, done: bool| {
        if let Some(g) = from {
            if done {
                grace.done(g.elapsed(), grace_prior);
            } else {
                grace.cut(g.elapsed(), grace_prior);
            }
        }
    };
    let mut last_len = None;
    let mut shell_until: Option<Instant> = None;
    let mut first_content = None;
    let mut first_text_at: Option<Duration> = None;
    let mut probed_len: Option<u64> = None;
    let mut early_stopped = false;
    let early_stop = ctx
        .opts
        .early_stop
        .as_ref()
        .filter(|_| ctx.opts.early_stop_probe);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            note_ready(false);
            note_grace(grace_from.take(), false);
            break;
        }
        // 页面文字一直在变（轮播、广告刷新）时，历史样本给出的探针时间到点也探一次，
        // 否则这类页面永远等不到“连续两次不变”。没有跨页样本时（例如每次请求一个
        // 独立进程），用页面自己的时间尺度：文字首次出现后，再等同样长的时间量级；
        // 一直没有文字时，用导航耗时做同样的相对判断。HTTP 已有正文时自动升级只是
        // 加分项，可以这样早停；HTTP 为空时渲染是唯一来源，仍然等到网络静默。
        let local_due = early_stop.is_some_and(|s| s.baseline_chars > 0)
            && started.elapsed() >= first_text_at.unwrap_or(nav_elapsed).saturating_mul(3);
        let due_by_time = local_due
            || early_stop
                .and_then(|s| s.after)
                .is_some_and(|after| started.elapsed() >= after);
        if !shared.wait_idle(NET_QUIET, left.min(CONTENT_POLL)).await {
            let len = tokio::time::timeout(left, eval_value(session, TEXT_LEN))
                .await
                .ok()
                .and_then(Result::ok)
                .and_then(|v| v.as_u64());
            note_first_text(&mut first_text_at, len, started);
            note_first_content(&mut first_content, len, early_stop, started);
            if let Some(n) = len
                && n >= CONTENT_READY_CHARS
                && last_len == Some(n)
            {
                // 正文已经够多且稳定：自动升级先让探针确认是否真的比 HTTP 多；
                // 没有增益就提前结束，否则按正常就绪返回。
                if early_stop.is_some()
                    && let Some(stop) = probe(ctx, n, &mut probed_len).await
                    && stop
                {
                    note_ready(false);
                    note_grace(grace_from.take(), false);
                    early_stopped = true;
                    break;
                }
                note_ready(true);
                note_grace(grace_from.take(), true);
                tracing::trace!(
                    chars = n,
                    "render: content stable, not waiting for network idle"
                );
                break;
            }
            last_len = len;
            if due_by_time
                && let Some(n) = len
                && let Some(stop) = probe(ctx, n, &mut probed_len).await
                && stop
            {
                note_ready(false);
                note_grace(grace_from.take(), false);
                early_stopped = true;
                break;
            }
            continue;
        }
        let probe_at = Instant::now();
        let left = deadline.saturating_duration_since(probe_at);
        if tokio::time::timeout(left, eval(session, "0"))
            .await
            .is_err()
        {
            note_ready(false);
            note_grace(grace_from.take(), false);
            break;
        }
        {
            let net = shared.net.lock().unwrap();
            if !(net.inflight.is_empty() && net.last_change < probe_at) {
                continue;
            }
        }
        // 网络静默了。页面已经有文字（绝大多数页面）就绪；几乎没有文字的壳在宽限期内再看几次。
        // 自动升级在返回前先探针一次：当前 DOM 的正文如果还不如 HTTP，就没必要再等。
        note_ready(true);
        let left = deadline.saturating_duration_since(Instant::now());
        let len = tokio::time::timeout(left, eval_value(session, TEXT_LEN))
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|v| v.as_u64());
        note_first_text(&mut first_text_at, len, started);
        note_first_content(&mut first_content, len, early_stop, started);
        if let Some(n) = len
            && early_stop.is_some()
            && let Some(stop) = probe(ctx, n, &mut probed_len).await
            && stop
        {
            note_grace(grace_from.take(), false);
            early_stopped = true;
            break;
        }
        if len.is_none_or(|n| n >= SHELL_MAX_CHARS) {
            note_grace(grace_from.take(), len.is_some());
            break;
        }
        let until = *shell_until.get_or_insert_with(|| {
            let now = Instant::now();
            grace_from = Some(now);
            now + grace_cap
        });
        let pause = until
            .min(deadline)
            .saturating_duration_since(Instant::now())
            .min(CONTENT_POLL);
        if pause.is_zero() {
            note_grace(grace_from.take(), false);
            tracing::trace!(chars = len, "render: page stayed nearly empty");
            break;
        }
        tokio::time::sleep(pause).await;
    }
    ReadyOutcome {
        early_stopped,
        first_content,
    }
}

fn note_first_text(first: &mut Option<Duration>, len: Option<u64>, started: Instant) {
    if first.is_none() && len.is_some_and(|n| n > 0) {
        *first = Some(started.elapsed());
    }
}

fn note_first_content(
    first: &mut Option<Duration>,
    len: Option<u64>,
    early_stop: Option<&EarlyStop>,
    started: Instant,
) {
    if first.is_none()
        && let (Some(n), Some(stop)) = (len, early_stop)
        && n > stop.baseline_chars
    {
        *first = Some(started.elapsed());
    }
}

/// 抓当前 DOM 交给调用方判断是否还值得继续等。抓取失败就当探针没结论，继续正常等待。
async fn probe(ctx: &PageCtx<'_>, len: u64, probed_len: &mut Option<u64>) -> Option<bool> {
    let stop = ctx.opts.early_stop.as_ref()?;
    if *probed_len == Some(len) {
        return None;
    }
    *probed_len = Some(len);
    match ctx.html().await {
        Ok(captured) => {
            let stop = (stop.probe)(captured.html).await;
            tracing::trace!(chars = len, stop, "render: early-stop probe");
            Some(stop)
        }
        Err(e) => {
            tracing::debug!(error = %e, "render: early-stop probe capture failed");
            None
        }
    }
}

/// 截图要等图片和字体；主框架当前文档的 `load` 最多等学到的上限（先验 0.81 的 10 s）。
pub(super) async fn wait_load(shared: &Shared) {
    let loader = shared.main_loader.lock().unwrap().clone();
    if let Some(l) = loader {
        let learn = &shared.waits.load;
        let started = Instant::now();
        match tokio::time::timeout(learn.cap(), shared.wait_event(&l, "load")).await {
            Ok(()) => learn.done(started.elapsed(), learn.prior()),
            Err(_) => learn.cut(started.elapsed(), learn.prior()),
        }
    }
}
