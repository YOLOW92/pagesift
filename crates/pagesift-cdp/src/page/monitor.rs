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

/// A cookie imported for `origin` can only be sent to the same host. Browsers
/// ignore ports and (for non-Secure cookies) the scheme, so any same-host
/// request outside the exact origin is rejected except the site's own standard
/// http:80 -> https:443 upgrade.
fn cookie_scope_violation(origin: &url::Url, target: &url::Url) -> bool {
    let target_scheme = match target.scheme() {
        "ws" => "http",
        "wss" => "https",
        other => other,
    };
    if target.host_str() != origin.host_str() {
        return false;
    }
    if target_scheme == origin.scheme()
        && target.port_or_known_default() == origin.port_or_known_default()
    {
        return false;
    }
    let standard_upgrade = origin.scheme() == "http"
        && target_scheme == "https"
        && origin.port_or_known_default() == Some(80)
        && target.port_or_known_default() == Some(443);
    !standard_upgrade
}

fn is_network_budget_scheme(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "ws" | "wss")
}

fn host_allowed_by_list(target: &url::Url, allowed_hosts: &[String]) -> bool {
    if allowed_hosts.is_empty() {
        return true;
    }
    let Some(raw_host) = target.host_str() else {
        return false;
    };
    let h = raw_host
        .trim_matches(['[', ']'])
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if h.is_empty() {
        return false;
    }
    let mut has_any = false;
    for entry in allowed_hosts {
        let ah = entry
            .trim()
            .trim_matches(['[', ']'])
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if ah.is_empty() {
            continue;
        }
        has_any = true;
        if h == ah {
            return true;
        }
        if ah.parse::<std::net::IpAddr>().is_err()
            && h.len() > ah.len()
            && h.ends_with(&ah)
            && h.as_bytes()[h.len() - ah.len() - 1] == b'.'
        {
            return true;
        }
    }
    !has_any
}

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

pub(super) fn is_method_not_found(err: &Error) -> bool {
    // [协议] JSON-RPC 2.0 Method not found 错误码
    matches!(err, Error::Protocol { code: -32601, .. })
}

async fn block_child_websockets(child: &Session, kind: &str, shared: &Shared) -> bool {
    if let Err(e) = enable_network(child).await {
        if shared.strict_security || !is_method_not_found(&e) {
            shared.fail_interception(format!("{kind} target Network.enable: {e}"));
            return false;
        }
        tracing::warn!(target = kind, error = %e, "child Network.enable unsupported in non-strict mode");
        return true;
    }
    if let Err(e) = child
        .call(
            "Network.setBlockedURLs",
            json!({ "urls": ["ws://*", "wss://*"] }),
        )
        .await
    {
        if shared.strict_security || !is_method_not_found(&e) {
            shared.fail_interception(format!("{kind} target Network.setBlockedURLs: {e}"));
            return false;
        }
        tracing::warn!(target = kind, error = %e, "child Network.setBlockedURLs unsupported in non-strict mode");
    }
    true
}

/// 自动附加上来的子 target 在调试器放行前不会发出任何请求。
pub(super) async fn attach_child(child: &Session, kind: &str, shared: &Shared) {
    if !shared.intercept {
        if kind == "iframe" {
            let _ = enable_network(child).await;
        }
        let _ = child
            .call("Runtime.runIfWaitingForDebugger", json!({}))
            .await;
        return;
    }

    if kind == "worker" {
        // 专用 worker 没有 Fetch 域，它的请求由所属页面的拦截处理；
        // 启动前必须建立嵌套子 worker 自动附加、WebSocket/WebTransport 构造器阻断及 ws/wss 封禁。
        if let Err(e) = child
            .call(
                "Target.setAutoAttach",
                json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
            )
            .await
            && (shared.strict_security || !is_method_not_found(&e))
        {
            shared.fail_interception(format!("worker target Target.setAutoAttach: {e}"));
            return;
        }
        if let Err(e) = child
            .call(
                "Runtime.evaluate",
                json!({ "expression": super::BLOCK_UNINTERCEPTED_TRANSPORT_JS }),
            )
            .await
        {
            shared.fail_interception(format!("worker target transport blocker: {e}"));
            return;
        }
        if !block_child_websockets(child, kind, shared).await {
            return;
        }
    } else {
        if let Err(e) = enable_interception(child, shared.proxy_auth.is_some()).await {
            // 开不了拦截就让它一直停着，并让本次渲染失败。
            shared.fail_interception(format!("{kind} target: {e}"));
            return;
        }
        if matches!(kind, "iframe" | "page") {
            // 跨进程 iframe 与弹窗/子 page 必须在放行前同时建立 WebSocket/WebTransport 构造器阻断、
            // ws/wss 封禁，并按需绕过 Service Worker。
            if let Err(e) = child.call("Page.enable", json!({})).await {
                shared.fail_interception(format!("{kind} target Page.enable: {e}"));
                return;
            }
            if let Err(e) = child
                .call(
                    "Page.addScriptToEvaluateOnNewDocument",
                    json!({ "source": super::BLOCK_UNINTERCEPTED_TRANSPORT_JS }),
                )
                .await
            {
                shared.fail_interception(format!(
                    "{kind} target Page.addScriptToEvaluateOnNewDocument: {e}"
                ));
                return;
            }
            if !block_child_websockets(child, kind, shared).await {
                return;
            }
            if shared.bypass_service_worker
                && let Err(e) = child
                    .call("Network.setBypassServiceWorker", json!({ "bypass": true }))
                    .await
            {
                shared.fail_interception(format!(
                    "{kind} target Network.setBypassServiceWorker: {e}"
                ));
                return;
            }
        } else if matches!(kind, "shared_worker" | "service_worker") {
            // Chromium 的 ServiceWorker / SharedWorker 在 waitForDebuggerOnStart 暂停态仅立即响应
            // Browser 侧域（Fetch / Target）；Network / Runtime 域命令需等 Runtime.runIfWaitingForDebugger
            // 启动 Worker 线程后才会完成，因此在此通过同一 session 管道化并发发出，避免启动互锁并检查结果。
            let (net_en, block_ws, eval_js, _resume) = tokio::join!(
                enable_network(child),
                child.call(
                    "Network.setBlockedURLs",
                    json!({ "urls": ["ws://*", "wss://*"] }),
                ),
                child.call(
                    "Runtime.evaluate",
                    json!({ "expression": super::BLOCK_UNINTERCEPTED_TRANSPORT_JS }),
                ),
                child.call("Runtime.runIfWaitingForDebugger", json!({})),
            );
            if let Err(e) = net_en
                && (shared.strict_security || !is_method_not_found(&e))
            {
                shared.fail_interception(format!("{kind} target Network.enable: {e}"));
                return;
            }
            if let Err(e) = block_ws
                && (shared.strict_security || !is_method_not_found(&e))
            {
                shared.fail_interception(format!("{kind} target Network.setBlockedURLs: {e}"));
                return;
            }
            if let Err(e) = eval_js
                && shared.strict_security
            {
                shared.fail_interception(format!("{kind} target transport blocker: {e}"));
            }
            return;
        } else {
            // 其他可执行脚本的子 target（worklet/webview 等）：
            // 放行前先执行网络层 ws/wss 封禁与传输层构造器阻断，失败即 fail-closed 且绝不放行。
            if !block_child_websockets(child, kind, shared).await {
                return;
            }
            if let Err(e) = child
                .call(
                    "Runtime.evaluate",
                    json!({ "expression": super::BLOCK_UNINTERCEPTED_TRANSPORT_JS }),
                )
                .await
                && (shared.strict_security || !is_method_not_found(&e))
            {
                shared.fail_interception(format!("{kind} target transport blocker: {e}"));
                return;
            }
            if shared.bypass_service_worker
                && let Err(e) = child
                    .call("Network.setBypassServiceWorker", json!({ "bypass": true }))
                    .await
                && (shared.strict_security || !is_method_not_found(&e))
            {
                shared.fail_interception(format!(
                    "{kind} target Network.setBypassServiceWorker: {e}"
                ));
                return;
            }
        }
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
    let parsed_url = url::Url::parse(&req.url).ok();
    let is_network_url = parsed_url
        .as_ref()
        .is_some_and(|u| is_network_budget_scheme(u.scheme()));
    // Imported cookies are host-only, so another host cannot receive them.
    // Within the same host they are not port- or scheme-isolated; only an
    // origin-initiated standard upgrade may move them from http to https.
    let cross_origin_cookie_host = shared.cookie_origin.as_ref().is_some_and(|origin| {
        parsed_url
            .as_ref()
            .is_some_and(|u| cookie_scope_violation(origin, u))
    });
    let host_outside_allowlist = !shared.allowed_hosts.is_empty()
        && is_network_url
        && parsed_url
            .as_ref()
            .is_none_or(|u| !host_allowed_by_list(u, &shared.allowed_hosts));
    let verdict = match block.reason(&req) {
        Some(r) => Verdict::Block(r.to_owned()),
        None if host_outside_allowlist => Verdict::Block("host outside allowedHosts policy".into()),
        None if cross_origin_cookie_host => Verdict::Block("cookie origin isolation".into()),
        None => match guard.check(&req).await {
            Verdict::Allow
                if is_network_url
                    && shared
                        .network_budget
                        .as_ref()
                        .is_some_and(|b| !b.reserve(req.is_main_document)) =>
            {
                shared.fail_interception("network request budget exhausted".into());
                Verdict::Block("network request budget exhausted".into())
            }
            other => other,
        },
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
        if shared.has_intercept_error() {
            break;
        }
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
            if shared.has_intercept_error() {
                break;
            }
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
