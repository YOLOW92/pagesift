//! 滚动：整页滚动触发懒加载（`scan_full_page`）和虚拟列表收集（`virtual_scroll`）。

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::conn::Session;
use crate::error::Result;
use crate::options::*;

use super::PageCtx;
use super::actions::{eval_value, js_str};

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

/// 每次滚动后先等这次滚动的 `scroll` 事件派发出来，再等页面改完 DOM（静默 50ms，最多 `waitMs`）才收集。
/// 滚动事件要等浏览器下一次出帧才派发，机器忙时会晚几百毫秒；只按固定时间等的话，会在旧内容上收集，
/// 接下来的几次滚动又被浏览器合并成一个事件，中间几屏就整屏丢了。滚动用 `instant`，
/// 页面设了平滑滚动时位置也立即生效；位置没动说明到底了，再静等一轮收集迟到的内容后结束。
pub(super) const VIRTUAL_JS: &str = r#"(async (sel, count, by, waitMs) => {
  const c = document.querySelector(sel);
  if (!c) return null;
  const own = c.scrollHeight > c.clientHeight + 1 && getComputedStyle(c).overflowY !== 'visible';
  const target = own ? c : window;
  const pos = () => own ? c.scrollTop : scrollY;
  const watch = { childList: true, subtree: true, characterData: true };
  const quiet = max => new Promise(res => {
    let t = null;
    const done = () => { ob.disconnect(); clearTimeout(cap); clearTimeout(t); res(); };
    const ob = new MutationObserver(() => { clearTimeout(t); t = setTimeout(done, 50); });
    ob.observe(c, watch);
    const cap = setTimeout(done, max);
  });
  // 位置已经变了才调用：滚动事件一定会来，兜底上限只防页面卡死。
  const settle = () => new Promise(res => {
    let fired = false, changed = false, t = null, cap = null;
    const done = () => {
      ob.disconnect(); target.removeEventListener('scroll', onScroll);
      clearTimeout(t); clearTimeout(cap); res();
    };
    const soon = () => { clearTimeout(t); t = setTimeout(done, 50); };
    const ob = new MutationObserver(() => { if (fired) soon(); else changed = true; });
    const onScroll = () => {
      if (fired) return;
      fired = true;
      clearTimeout(cap); cap = setTimeout(done, waitMs);
      if (changed) soon();
    };
    ob.observe(c, watch);
    target.addEventListener('scroll', onScroll, { passive: true });
    cap = setTimeout(done, Math.max(waitMs, 3000));
  });
  const seen = new Set(), items = [];
  const grab = () => {
    for (const ch of c.children) {
      const k = (ch.textContent || '').replace(/\s+/g, ' ').trim() || ch.outerHTML;
      if (!seen.has(k)) { seen.add(k); items.push(ch.outerHTML); }
    }
  };
  grab();
  for (let i = 0; i < count; i++) {
    const step = by === 'container' ? (own ? c.clientHeight : innerHeight) : by === 'page' ? innerHeight : by;
    const before = pos();
    (own ? c : window).scrollTo({ top: before + step, behavior: 'instant' });
    if (pos() === before) { await quiet(waitMs); grab(); break; }
    await settle();
    grab();
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
