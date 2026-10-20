//! 取 HTML：在每个 frame 的隔离 world 里序列化，iframe 递归内联，open shadow root 按组合树展开。

use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasher, Hasher};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use serde_json::{Value, json};

use crate::conn::{Connection, Session};
use crate::error::{Error, Result};

const WORLD: &str = "pagesift";
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// 收集 iframe、shadow host 和 slot，放到隔离 world 的全局变量里，返回 iframe 数组。
const PREPARE_JS: &str = r#"function (shadow, wantFrames) {
  const frames = [], hosts = [], slots = [];
  const scan = (root) => {
    if (wantFrames) for (const e of root.querySelectorAll('iframe,frame')) frames.push(e);
    if (!shadow) return;
    for (const e of root.querySelectorAll('*')) {
      const sr = e.shadowRoot;
      if (sr) { hosts.push(e); for (const s of sr.querySelectorAll('slot')) slots.push(s); scan(sr); }
    }
  };
  scan(document);
  globalThis.__pagesift = { frames, hosts: new Set(hosts), slots: new Set(slots) };
  return frames;
}"#;

/// HTML 片段序列化。只有通往特殊节点的路径逐个节点走，其余子树直接用 outerHTML。
const SERIALIZE_JS: &str = r#"function (o) {
  const P = globalThis.__pagesift || { frames: [], hosts: new Set(), slots: new Set() };
  const d = document;
  if (o.bodyOnly && location.protocol === 'chrome-error:') return null;
  const HTML = 'http://www.w3.org/1999/xhtml';
  const inline = new Map();
  for (const i of o.inline) if (P.frames[i]) inline.set(P.frames[i], i);
  const virt = new Map();
  for (const [k, items] of Object.entries(o.virtual)) {
    const e = d.querySelector('[data-pagesift-vs="' + k + '"]');
    if (e) virt.set(e, items);
  }
  const path = new Set();
  for (const s of [inline.keys(), P.hosts, P.slots, virt.keys()]) for (let n of s) {
    while (n && !path.has(n)) { path.add(n); n = n.nodeType === 11 ? n.host : n.parentNode; }
  }
  const VOID = new Set(['area','base','basefont','bgsound','br','col','embed','frame','hr','img','input','keygen','link','meta','param','source','track','wbr']);
  const RAW = new Set(['style','script','xmp','iframe','noembed','noframes','plaintext','noscript']);
  const ea = s => s.replace(/&/g, '&amp;').replace(/ /g, '&nbsp;').replace(/"/g, '&quot;');
  const et = s => s.replace(/&/g, '&amp;').replace(/ /g, '&nbsp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  const out = [], frames = {};
  const kids = p => { for (let c = p.firstChild; c; c = c.nextSibling) node(c); };
  const node = n => {
    const t = n.nodeType;
    if (t === 1) return el(n);
    if (t === 3) {
      const p = n.parentNode;
      out.push(p && p.nodeType === 1 && p.namespaceURI === HTML && RAW.has(p.localName) ? n.data : et(n.data));
    } else if (t === 8) out.push('<!--' + n.data + '-->');
    else if (t === 11) kids(n);
  };
  const el = e => {
    if (!path.has(e)) { out.push(e.outerHTML); return; }
    const i = inline.get(e);
    if (i !== undefined) {
      const src = e.getAttribute('src') || (e.hasAttribute('srcdoc') ? 'about:srcdoc' : '');
      frames[i] = { src, original: e.outerHTML };
      out.push('<!--' + o.nonce + ':' + i + '-->');
      return;
    }
    if (P.slots.has(e)) {
      const a = e.assignedNodes({ flatten: true });
      if (a.length) a.forEach(node); else kids(e);
      return;
    }
    const html = e.namespaceURI === HTML;
    const tag = html ? e.localName : e.tagName;
    const name = html && tag === 'frameset' ? 'div' : tag;
    let s = '<' + name;
    for (const a of e.attributes) if (a.name !== 'data-pagesift-vs') s += ' ' + a.name + '="' + ea(a.value) + '"';
    out.push(s + '>');
    if (html && VOID.has(tag)) return;
    const items = virt.get(e);
    if (items) out.push(items.join(''));
    else if (P.hosts.has(e)) kids(e.shadowRoot);
    else if (html && tag === 'template') kids(e.content);
    else kids(e);
    out.push('</' + name + '>');
  };
  if (o.bodyOnly) {
    const b = d.body || d.documentElement;
    if (b) { if (d.body && b.localName !== 'frameset') kids(b); else node(b); }
  } else {
    if (d.doctype) out.push(new XMLSerializer().serializeToString(d.doctype) + '\n');
    if (d.documentElement) node(d.documentElement);
  }
  return { html: out.join(''), frames, url: location.href };
}"#;

pub(crate) struct CaptureOptions<'a> {
    pub iframes: bool,
    pub max_depth: usize,
    pub max_iframes: usize,
    pub shadow: bool,
    /// 虚拟滚动收集到的条目：容器标记 -> 条目 HTML。
    pub virtual_items: &'a BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Default)]
pub(crate) struct Captured {
    pub html: String,
    /// 主框架的 `location.href`，和 HTML 一次取回，省一次往返。
    pub url: String,
    pub inlined: usize,
    pub skipped: usize,
    pub iframe_time: Duration,
}

struct Ctx<'a> {
    conn: &'a Connection,
    /// 跨进程 iframe：target id（等于 frame id） -> session id。
    oopif: &'a Mutex<HashMap<String, String>>,
    opts: &'a CaptureOptions<'a>,
    nonce: String,
    inlined: AtomicUsize,
    skipped: AtomicUsize,
}

pub(crate) async fn capture_html(
    conn: &Connection,
    page: &Session,
    main_frame: &str,
    oopif: &Mutex<HashMap<String, String>>,
    opts: &CaptureOptions<'_>,
) -> Result<Captured> {
    let ctx = Ctx {
        conn,
        oopif,
        opts,
        nonce: format!(
            "pagesift-{:016x}",
            RandomState::new().build_hasher().finish()
        ),
        inlined: AtomicUsize::new(0),
        skipped: AtomicUsize::new(0),
    };
    let mut attempt = 0;
    let (html, url, child_time) = loop {
        match capture_frame(&ctx, page.clone(), main_frame.to_owned(), 0).await {
            // 页面刚跳转时执行上下文会失效，稍等再试。
            Err(Error::Protocol { ref message, .. })
                if message.contains("context") && attempt < 50 =>
            {
                attempt += 1;
                ctx.inlined.store(0, Ordering::Relaxed);
                ctx.skipped.store(0, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            r => {
                break r?.ok_or_else(|| {
                    Error::UnexpectedResponse("main frame has no document".into())
                })?;
            }
        }
    };
    Ok(Captured {
        html,
        url,
        inlined: ctx.inlined.into_inner(),
        skipped: ctx.skipped.into_inner(),
        iframe_time: child_time,
    })
}

/// 返回 frame 的 HTML（子 frame 只取 body 内容）、它的 URL 和花在子 frame 上的时间。
fn capture_frame<'a>(
    ctx: &'a Ctx<'a>,
    session: Session,
    frame_id: String,
    depth: usize,
) -> crate::guard::BoxFuture<'a, Result<Option<(String, String, Duration)>>> {
    Box::pin(async move {
        let world = session
            .call(
                "Page.createIsolatedWorld",
                json!({ "frameId": frame_id, "worldName": WORLD, "grantUniveralAccess": false }),
            )
            .await?;
        let context = world
            .get("executionContextId")
            .and_then(Value::as_i64)
            .ok_or_else(|| Error::UnexpectedResponse("missing executionContextId".into()))?;
        let want_frames = ctx.opts.iframes;
        let prepared = call_fn(
            &session,
            context,
            PREPARE_JS,
            json!([{ "value": ctx.opts.shadow }, { "value": want_frames }]),
            false,
        )
        .await?;

        let child_start = Instant::now();
        let mut inline: Vec<(usize, String)> = Vec::new();
        if want_frames && depth >= ctx.opts.max_depth {
            // 到了深度上限：只计数，不再往下展开。
            let n = prepared["description"]
                .as_str()
                .and_then(|d| d.strip_prefix("Array(")?.strip_suffix(')')?.parse().ok())
                .unwrap_or(0usize);
            ctx.skipped.fetch_add(n, Ordering::Relaxed);
        } else if want_frames && let Some(array) = prepared.get("objectId").and_then(Value::as_str)
        {
            for (i, fid) in child_frames(&session, array).await? {
                let Some(fid) = fid else { continue };
                if ctx.inlined.fetch_add(1, Ordering::Relaxed) < ctx.opts.max_iframes {
                    inline.push((i, fid));
                } else {
                    ctx.inlined.fetch_sub(1, Ordering::Relaxed);
                    ctx.skipped.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        let mut child_time = child_start.elapsed();
        let virtual_items = if depth == 0 {
            json!(ctx.opts.virtual_items)
        } else {
            json!({})
        };
        let r = call_fn(
            &session,
            context,
            SERIALIZE_JS,
            json!([{ "value": {
                "nonce": ctx.nonce,
                "inline": inline.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
                "bodyOnly": depth > 0,
                "virtual": virtual_items,
            } }]),
            true,
        )
        .await;
        let release = session.clone();
        tokio::spawn(async move {
            let _ = release
                .call(
                    "Runtime.releaseObjectGroup",
                    json!({ "objectGroup": WORLD }),
                )
                .await;
        });
        let v = r?;
        let Some(out) = v.get("value").filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        let mut html = out["html"].as_str().unwrap_or_default().to_owned();
        let url = out["url"].as_str().unwrap_or_default().to_owned();
        if inline.is_empty() {
            return Ok(Some((html, url, child_time)));
        }

        let child_start = Instant::now();
        let children = join_all(inline.iter().map(|(_, fid)| {
            let s = match ctx.oopif.lock().unwrap().get(fid) {
                Some(sid) => ctx.conn.session(sid.clone()),
                None => session.clone(),
            };
            let fid = fid.clone();
            async move {
                tokio::time::timeout(FRAME_TIMEOUT, capture_frame(ctx, s, fid, depth + 1))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .flatten()
            }
        }))
        .await;
        child_time += child_start.elapsed();
        for ((i, _), child) in inline.iter().zip(children) {
            let marker = format!("<!--{}:{i}-->", ctx.nonce);
            let info = &out["frames"][i.to_string()];
            let replacement = match child {
                Some((body, ..)) => format!(
                    "<div data-pagesift-iframe=\"{}\">{body}</div>",
                    escape_attr(info["src"].as_str().unwrap_or_default())
                ),
                None => {
                    ctx.inlined.fetch_sub(1, Ordering::Relaxed);
                    ctx.skipped.fetch_add(1, Ordering::Relaxed);
                    info["original"].as_str().unwrap_or_default().to_owned()
                }
            };
            html = html.replacen(&marker, &replacement, 1);
        }
        Ok(Some((html, url, child_time)))
    })
}

async fn call_fn(
    session: &Session,
    context: i64,
    func: &str,
    args: Value,
    by_value: bool,
) -> Result<Value> {
    let r = session
        .call(
            "Runtime.callFunctionOn",
            json!({
                "functionDeclaration": func, "executionContextId": context, "arguments": args,
                "returnByValue": by_value, "objectGroup": WORLD,
            }),
        )
        .await?;
    if let Some(ex) = r.get("exceptionDetails") {
        return Err(Error::UnexpectedResponse(format!(
            "serializer failed: {}",
            ex.pointer("/exception/description")
                .or_else(|| ex.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("exception")
        )));
    }
    Ok(r.get("result").cloned().unwrap_or(Value::Null))
}

/// 数组里每个 iframe 元素对应的 frame id（还没有 frame 时为 `None`）。
async fn child_frames(session: &Session, array: &str) -> Result<Vec<(usize, Option<String>)>> {
    let props = session
        .call(
            "Runtime.getProperties",
            json!({ "objectId": array, "ownProperties": true }),
        )
        .await?;
    let elems: Vec<(usize, String)> = props["result"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|p| {
            let i = p["name"].as_str()?.parse().ok()?;
            Some((i, p.pointer("/value/objectId")?.as_str()?.to_owned()))
        })
        .collect();
    let ids = join_all(elems.iter().map(|(_, obj)| async move {
        session
            .call("DOM.describeNode", json!({ "objectId": obj }))
            .await
            .ok()
            .and_then(|d| d.pointer("/node/frameId")?.as_str().map(str::to_owned))
    }))
    .await;
    let mut out: Vec<_> = elems.into_iter().map(|(i, _)| i).zip(ids).collect();
    out.sort_by_key(|(i, _)| *i);
    Ok(out)
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}
