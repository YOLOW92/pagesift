use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};

use crate::capture::Captured;
use crate::conn::{Session, str_field};
use crate::error::{Error, Result};
use crate::options::*;

use super::monitor::{set_metrics, wait_load};
use super::{MAX_CAPTURE_PX, Metrics, PageCtx};

pub(super) enum ActionError {
    Cdp(Error),
    Failed(String),
}

impl From<Error> for ActionError {
    fn from(e: Error) -> Self {
        ActionError::Cdp(e)
    }
}

pub(super) struct RemoteValue {
    kind: String,
    value: Value,
}

/// 执行一段表达式。外层错误是协议错误，内层是页面里的 JS 异常。
pub(super) async fn eval(session: &Session, expr: &str) -> Result<Result<RemoteValue, String>> {
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
                if (message.contains("context") || message.contains("navigated or closed"))
                    && attempt < 50 =>
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

pub(super) async fn eval_ok(session: &Session, expr: &str) -> Result<Value, ActionError> {
    eval(session, expr)
        .await?
        .map(|v| v.value)
        .map_err(ActionError::Failed)
}

pub(super) async fn eval_value(session: &Session, expr: &str) -> Result<Value> {
    match eval(session, expr).await? {
        Ok(v) => Ok(v.value),
        Err(e) => Err(Error::UnexpectedResponse(e)),
    }
}

pub(super) fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

pub(super) async fn run_action(
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
            let Captured { html, url, .. } = ctx.html().await?;
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

pub(super) async fn click_nth(session: &Session, sel: &str, i: u64) -> Result<(), ActionError> {
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

pub(super) async fn capture(
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

pub(super) async fn capture_inner(session: &Session, s: &ScreenshotOptions) -> Result<Screenshot> {
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

pub(super) fn decode(r: &Value) -> Result<Vec<u8>> {
    let data = str_field(r, "data")?;
    B64.decode(data.as_bytes())
        .map_err(|e| Error::UnexpectedResponse(format!("bad base64: {e}")))
}

pub(super) struct KeyDef {
    key: String,
    code: String,
    vk: u32,
    text: Option<String>,
}

pub(super) fn key_def(name: &str) -> Option<KeyDef> {
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
