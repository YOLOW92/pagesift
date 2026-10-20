//! `convert` / `convertAsync`：把已有的 HTML 转成 Markdown，不发网络请求。

use napi::bindgen_prelude::*;
use napi_derive::napi;
use pagesift::api::ApiError;
use serde_json::{Map, Value, json};

use crate::error::{self, Fallible};

#[napi(object)]
#[derive(Default)]
pub struct ConvertOptions {
    /// 页面地址，用来把相对链接转成绝对地址，也会写进 `metadata.url`。
    pub url: Option<String>,
    /// 只保留正文（去掉导航、页眉页脚、侧边栏等），默认 true。
    pub only_main_content: Option<bool>,
}

struct Prepared {
    html: String,
    url: Option<String>,
    opts: pagesift_content::Options,
}

fn prepare(html: String, options: Option<ConvertOptions>) -> Fallible<Prepared> {
    let o = options.unwrap_or_default();
    let base_url = match &o.url {
        Some(u) => Some(
            url::Url::parse(u).map_err(|e| ApiError::invalid(format!("invalid url {u:?}: {e}")))?,
        ),
        None => None,
    };
    Ok(Prepared {
        html,
        url: o.url,
        opts: pagesift_content::Options {
            base_url,
            only_main_content: o.only_main_content.unwrap_or(true),
            detect_barrier: false,
            ..Default::default()
        },
    })
}

fn run(p: Prepared) -> Value {
    document(pagesift_content::process(&p.html, &p.opts), p.url)
}

/// 结果和 `scrape` 的 Document 同形（只有 `markdown` 和 `metadata`）。
fn document(doc: pagesift_content::Document, url: Option<String>) -> Value {
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
        ("url", url.clone()),
        ("sourceURL", url),
    ] {
        if let Some(v) = v {
            meta.insert(k.into(), v.into());
        }
    }
    for (k, v) in m.extra {
        meta.entry(firecrawl_meta_key(&k)).or_insert(v.into());
    }
    json!({ "markdown": doc.markdown, "metadata": meta })
}

/// `og:image` → `ogImage`，和 `scrape` 的键名一致。
fn firecrawl_meta_key(raw: &str) -> String {
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

/// 把一段 HTML 转成干净的 Markdown（在当前线程上同步执行）。
#[napi(ts_return_type = "Document")]
pub fn convert<'e>(
    env: &'e Env,
    html: String,
    options: Option<ConvertOptions>,
) -> Result<Unknown<'e>> {
    let p = prepare(html, options).map_err(|e| error::to_js(env, &e))?;
    env.to_js_value(&run(p))
}

pub struct ConvertTask(Option<Fallible<Prepared>>);

impl<'t> napi::ScopedTask<'t> for ConvertTask {
    type Output = Fallible<Value>;
    type JsValue = Unknown<'t>;

    fn compute(&mut self) -> Result<Fallible<Value>> {
        let p = self
            .0
            .take()
            .ok_or_else(|| Error::from_reason("convert task ran twice"))?;
        Ok(p.map(run))
    }

    fn resolve(&mut self, env: &'t Env, output: Fallible<Value>) -> Result<Unknown<'t>> {
        match output {
            Ok(v) => env.to_js_value(&v),
            Err(e) => Err(error::to_js(env, &e)),
        }
    }
}

/// 和 `convert` 相同，但在 libuv 线程池上执行，不阻塞事件循环。
#[napi(ts_return_type = "Promise<Document>")]
pub fn convert_async(html: String, options: Option<ConvertOptions>) -> AsyncTask<ConvertTask> {
    AsyncTask::new(ConvertTask(Some(prepare(html, options))))
}

#[cfg(test)]
mod tests {
    use super::firecrawl_meta_key;

    #[test]
    fn meta_keys_follow_firecrawl_style() {
        assert_eq!(firecrawl_meta_key("og:site_name"), "ogSiteName");
        assert_eq!(firecrawl_meta_key("dc.title"), "dcTitle");
    }
}
