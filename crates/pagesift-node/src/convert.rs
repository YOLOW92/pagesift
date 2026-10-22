//! `convert` / `convertAsync` / `convertMany`：把已有的 HTML 转成 Markdown，不发网络请求。
//!
//! 异步版本共用有上限的 Rust CPU 名额，并为 Node 主线程留出一个逻辑 CPU（不受 libuv 线程池默认 4 个线程的限制）。

use std::borrow::Cow;
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use napi::bindgen_prelude::*;
use napi_derive::napi;
use pagesift::api::{ApiError, ErrorCode};
use serde_json::{Map, Value};
use tokio::sync::Semaphore;

use crate::error::{self, Fallible};
use crate::options::{ConvertOptions, ConvertPlan};
use crate::page::Page;

/// HTML 可以是字符串，也可以是 UTF-8 字节（比如 `fs.readFile` 的 Buffer，省一次解码）。
pub type Html = Either<String, Uint8Array>;

fn text(html: &Html) -> Cow<'_, str> {
    match html {
        Either::A(s) => Cow::Borrowed(s),
        Either::B(b) => String::from_utf8_lossy(b),
    }
}

/// 所有异步转换共用的 CPU 名额。
fn cpu_slots() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(cores())))
        .clone()
}

pub fn cores() -> usize {
    std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .saturating_sub(1)
        .max(1)
}

fn run(html: &str, plan: &ConvertPlan) -> Page {
    let doc = pagesift_content::process(html, &plan.opts);
    let m = doc.metadata;
    let mut meta = Map::new();
    for (k, v) in [
        ("title", m.title),
        ("description", m.description),
        ("language", m.language),
        ("keywords", m.keywords),
        ("robots", m.robots),
        ("favicon", m.favicon),
        ("publishedTime", m.published_time),
        ("modifiedTime", m.modified_time),
        ("url", plan.url.clone()),
        ("sourceURL", plan.url.clone()),
    ] {
        if let Some(v) = v {
            meta.insert(k.into(), v.into());
        }
    }
    for (k, v) in m.extra {
        meta.entry(meta_key(&k)).or_insert(v.into());
    }
    Page::new(
        plan.markdown.then_some(doc.markdown),
        doc.html,
        plan.raw_html.then(|| html.to_string()),
        doc.links.map(|l| l.into_iter().map(|l| l.url).collect()),
        doc.images.map(|i| i.into_iter().map(|i| i.src).collect()),
        Value::Object(meta),
    )
}

/// `og:image` → `ogImage`，和 `scrape` 的键名一致。
fn meta_key(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut upper = false;
    for c in raw.chars() {
        if matches!(c, ':' | '_' | '-' | '.') {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn plan(options: Option<ConvertOptions>) -> Fallible<ConvertPlan> {
    options.unwrap_or_default().plan()
}

/// 把一段 HTML 转成 Markdown，在当前线程上同步执行（会阻塞事件循环，适合脚本和小页面）。
#[napi(ts_args_type = "html: string | Uint8Array, options?: ConvertOptions")]
pub fn convert(env: &Env, html: Html, options: Option<ConvertOptions>) -> Result<Page> {
    let plan = plan(options).map_err(|e| error::to_js(env, &e))?;
    Ok(run(&text(&html), &plan))
}

/// 和 `convert` 相同，但在后台线程上执行，不阻塞事件循环。
#[napi(
    ts_args_type = "html: string | Uint8Array, options?: ConvertOptions",
    ts_return_type = "Promise<Page>"
)]
pub fn convert_async<'e>(
    env: &'e Env,
    html: Html,
    options: Option<ConvertOptions>,
) -> Result<PromiseRaw<'e, Page>> {
    let plan = plan(options);
    let fut = async move {
        let plan = match plan {
            Ok(p) => p,
            Err(e) => return Ok(Err(e)),
        };
        let _slot = cpu_slots().acquire_owned().await;
        Ok(
            tokio::task::spawn_blocking(move || run(&text(&html), &plan))
                .await
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string())),
        )
    };
    env.spawn_future_with_callback(fut, |env, out: Fallible<Page>| {
        out.map_err(|e| error::to_js(env, &e))
    })
}

/// 批量转换：使用共享的 Rust CPU 名额，结果顺序和输入一致。
/// 先完成主线程上的输入校验和字符串复制，再启动 CPU 工作，避免输入复制与解析争用 CPU。
/// `Uint8Array` 省去 UTF-8 字符串复制；调用完成前不得修改它。
#[napi(
    ts_args_type = "htmls: Array<string | Uint8Array>, options?: ConvertOptions",
    ts_return_type = "Promise<Page[]>"
)]
pub fn convert_many<'e>(
    env: &'e Env,
    htmls: Array<'e>,
    options: Option<ConvertOptions>,
) -> Result<PromiseRaw<'e, Vec<Page>>> {
    let reject = |e: ApiError| {
        env.spawn_future_with_callback(async { Ok(()) }, move |env, ()| -> Result<Vec<Page>> {
            Err(error::to_js(env, &e))
        })
    };
    let plan = match plan(options) {
        Ok(p) => Arc::new(p),
        Err(e) => return reject(e),
    };
    let n = htmls.len() as usize;
    let (tx, rx) = mpsc::channel::<(usize, Html)>();
    for i in 0..n {
        let html: Html = htmls
            .get(i as u32)?
            .ok_or_else(|| Error::from_reason(format!("htmls[{i}] is missing")))?;
        tx.send((i, html))
            .map_err(|_| Error::from_reason("conversion input queue closed"))?;
    }
    drop(tx);
    let rx = Arc::new(Mutex::new(rx));
    let workers: Vec<_> = (0..cores().min(n))
        .map(|_| {
            let (rx, plan) = (rx.clone(), plan.clone());
            napi::bindgen_prelude::within_runtime_if_available(|| {
                tokio::spawn(async move {
                    let _slot = cpu_slots().acquire_owned().await;
                    tokio::task::spawn_blocking(move || {
                        let mut done = Vec::new();
                        loop {
                            let next = rx
                                .lock()
                                .map_err(|_| ())
                                .and_then(|r| r.recv().map_err(|_| ()));
                            match next {
                                Ok((i, h)) => done.push((i, run(&text(&h), &plan))),
                                Err(()) => break done,
                            }
                        }
                    })
                    .await
                })
            })
        })
        .collect();
    let fut = async move {
        let mut pages: Vec<Option<Page>> = (0..n).map(|_| None).collect();
        for w in workers {
            match w.await {
                Ok(Ok(done)) => done.into_iter().for_each(|(i, p)| pages[i] = Some(p)),
                Ok(Err(e)) | Err(e) => {
                    return Ok(Err(ApiError::new(ErrorCode::Internal, e.to_string())));
                }
            }
        }
        Ok(Ok(pages.into_iter().flatten().collect::<Vec<_>>()))
    };
    env.spawn_future_with_callback(fut, |env, out: Fallible<Vec<Page>>| {
        out.map_err(|e| error::to_js(env, &e))
    })
}

#[cfg(test)]
mod tests {
    use super::meta_key;

    #[test]
    fn meta_keys_follow_firecrawl_style() {
        assert_eq!(meta_key("og:site_name"), "ogSiteName");
        assert_eq!(meta_key("dc.title"), "dcTitle");
    }
}
