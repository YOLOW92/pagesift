use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::conn::{Connection, EventStream, Session, str_field};
use crate::error::{Error, Result};
use crate::guard::{BlockResources, RequestGuard, SubRequest, Verdict};
use crate::options::*;

use super::actions::{eval, eval_value, js_str};
use super::{
    CONTENT_POLL, CONTENT_READY_CHARS, LOAD_CAP, Metrics, NET_QUIET, PageCtx, Shared, TEXT_LEN,
    TRACKED,
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

pub(super) async fn enable_interception(session: &Session) -> Result<()> {
    // 两条互不依赖，一起发；都完成之前不会放行导航或子 target。
    tokio::try_join!(
        session.call(
            "Fetch.enable",
            json!({ "patterns": [{ "urlPattern": "*", "requestStage": "Request" }] }),
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
    } else if let Err(e) = enable_interception(child).await {
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
/// 剩下的请求多是广告和统计）。最多等 `cap`。
pub(super) async fn wait_ready(session: &Session, shared: &Shared, cap: Duration) {
    let deadline = Instant::now() + cap;
    let mut last_len = None;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        if !shared.wait_idle(NET_QUIET, left.min(CONTENT_POLL)).await {
            let len = tokio::time::timeout(left, eval_value(session, TEXT_LEN))
                .await
                .ok()
                .and_then(Result::ok)
                .and_then(|v| v.as_u64());
            if let Some(n) = len
                && n >= CONTENT_READY_CHARS
                && last_len == Some(n)
            {
                tracing::trace!(
                    chars = n,
                    "render: content stable, not waiting for network idle"
                );
                return;
            }
            last_len = len;
            continue;
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
pub(super) async fn wait_load(shared: &Shared) {
    let loader = shared.main_loader.lock().unwrap().clone();
    if let Some(l) = loader {
        let _ = tokio::time::timeout(LOAD_CAP, shared.wait_event(&l, "load")).await;
    }
}

pub(super) async fn scan_full_page(ctx: &PageCtx<'_>, scan: &FullPageScan) -> Result<()> {
    const STEP: &str = "(() => { window.scrollBy(0, window.innerHeight); \
        const se = document.scrollingElement || document.documentElement; \
        return [window.scrollY + window.innerHeight, se.scrollHeight]; })()";
    const POS: &str = "(() => { const se = document.scrollingElement || document.documentElement; \
        return [window.scrollY + window.innerHeight, se.scrollHeight]; })()";
    let pos =
        |v: Value| -> (f64, f64) { (v[0].as_f64().unwrap_or(0.0), v[1].as_f64().unwrap_or(0.0)) };
    let start = Instant::now();
    let mut last_height = pos(eval_value(ctx.session, POS).await?).1;
    let mut stable_bottom = 0;
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
            stable_bottom = 0;
            continue;
        }
        if bottom >= height - 2.0 {
            // IntersectionObserver and scroll handlers may start a fetch after
            // network idle is observed. Require several quiet bottom scans, and
            // give the last one a whole scroll_delay: on a slow machine the
            // handler's fetch can trail the scroll by more than the quiet window.
            stable_bottom += 1;
            if stable_bottom >= 3 {
                break;
            }
            if stable_bottom == 2 {
                tokio::time::sleep(scan.scroll_delay).await;
            }
        } else {
            stable_bottom = 0;
        }
    }
    eval_value(ctx.session, "window.scrollTo(0, 0)").await?;
    Ok(())
}

pub(super) const VIRTUAL_JS: &str = r#"(async (sel, count, by, waitMs) => {
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
pub(super) async fn virtual_scroll(
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
