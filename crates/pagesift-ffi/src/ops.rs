//! 引擎操作：scrape、parse、convert、batch、map。入参 JSON 进，结果 `Value` 出，错误不抛出。

use std::sync::Arc;

use pagesift_core::Scraper;
use pagesift_core::api::types::{MapRequest, ScrapeOptions, Upload, UploadKind};
use pagesift_core::api::{ApiError, MAX_MAP_LIMIT, MAX_URLS, check_parse_options, check_url};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::runtime::Runtime;
use tokio::sync::Semaphore;

use crate::args;
use crate::doc::{Outcome, document_value, failure_value};

const DEFAULT_CONCURRENCY: usize = 16;

fn failed(url: Option<String>, e: ApiError) -> Value {
    document_value(&Outcome {
        index: None,
        url,
        result: Err(e),
    })
}

async fn scrape_one(
    scraper: &Scraper,
    index: Option<usize>,
    url: String,
    opts: &ScrapeOptions,
) -> Outcome {
    let result = match check_url(&url) {
        Ok(()) => scraper.scrape(&url, opts).await,
        Err(e) => Err(e),
    };
    Outcome {
        index,
        url: Some(url),
        result,
    }
}

pub fn scrape(rt: &Runtime, s: &Arc<Scraper>, url: String, options: Option<&str>) -> Value {
    let opts = match args::object(options).and_then(args::scrape_options) {
        Ok(o) => o,
        Err(e) => return failed(Some(url), e),
    };
    document_value(&rt.block_on(scrape_one(s, None, url, &opts)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ParseArgs {
    content_type: Option<String>,
    url: Option<String>,
}

pub fn parse(
    rt: &Runtime,
    s: &Arc<Scraper>,
    data: &[u8],
    filename: String,
    options: Option<&str>,
) -> Value {
    let prepared = args::object(options).and_then(|mut m| {
        let a: ParseArgs = args::take(&mut m, &["content_type", "url"])?;
        let opts = args::scrape_options(m)?;
        check_parse_options(&opts, s.as_ref())?;
        let url = match a.url {
            Some(u) => Some(
                url::Url::parse(&u)
                    .map_err(|e| ApiError::invalid(format!("invalid url {u:?}: {e}")))?,
            ),
            None => None,
        };
        // 按内容、文件名和 Content-Type 识别类型；认不出的类型不猜，直接拒绝
        let Some(kind) = UploadKind::detect(data, &filename, a.content_type.as_deref()) else {
            return Err(ApiError::unsupported(format!(
                "unsupported file type: {filename}"
            )));
        };
        let upload = Upload {
            bytes: data.to_vec(),
            filename: filename.clone(),
            content_type: a.content_type,
            kind,
            url,
        };
        Ok((upload, opts))
    });
    let (upload, opts) = match prepared {
        Ok(p) => p,
        Err(e) => return failed(Some(filename), e),
    };
    let result = rt.block_on(s.parse(upload, &opts));
    document_value(&Outcome {
        index: None,
        url: Some(filename),
        result,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConvertArgs {
    url: Option<String>,
    only_main_content: Option<bool>,
}

/// 把一段 HTML 转成 Markdown：结果只有 markdown 和元数据，键名和抓取结果一样。
pub fn convert(html: &str, options: Option<&str>) -> Value {
    let prepared = args::object(options).and_then(args::parse::<ConvertArgs>);
    let a = match prepared {
        Ok(a) => a,
        Err(e) => return failed(None, e),
    };
    let base_url = match a.url {
        Some(u) => match url::Url::parse(&u) {
            Ok(p) => Some(p),
            Err(e) => return failed(None, ApiError::invalid(format!("invalid url {u:?}: {e}"))),
        },
        None => None,
    };
    let opts = pagesift_content::Options {
        base_url,
        only_main_content: a.only_main_content.unwrap_or(true),
        ..Default::default()
    };
    let out = pagesift_content::process(html, &opts);
    let mut metadata = pagesift_core::api::types::Metadata::default();
    pagesift_sink::meta::fill(&mut metadata, &out.metadata);
    if let Some(c) = out.metadata.canonical_url {
        metadata
            .extra
            .insert("canonicalUrl".into(), Value::String(c));
    }
    document_value(&Outcome {
        index: None,
        url: None,
        result: Ok(pagesift_core::api::types::Document {
            markdown: Some(out.markdown),
            metadata,
            ..Default::default()
        }),
    })
}

/// 批量抓取，全部完成后按输入顺序返回；单个失败不影响其他项（对应项 `success` 为 false）。
pub fn batch(rt: &Runtime, s: &Arc<Scraper>, urls: &str, options: Option<&str>) -> Value {
    match batch_inner(rt, s, urls, options) {
        Ok(v) => v,
        Err(e) => failure_value(&e),
    }
}

fn batch_inner(
    rt: &Runtime,
    s: &Arc<Scraper>,
    urls: &str,
    options: Option<&str>,
) -> Result<Value, ApiError> {
    let urls: Vec<String> = serde_json::from_str(urls)
        .map_err(|e| ApiError::invalid(format!("urls must be a JSON array of strings: {e}")))?;
    if urls.len() > MAX_URLS {
        return Err(ApiError::invalid(format!(
            "at most {MAX_URLS} URLs per batch"
        )));
    }
    let mut m = args::object(options)?;
    let concurrency = match m.remove("concurrency") {
        None | Some(Value::Null) => DEFAULT_CONCURRENCY,
        Some(v) => {
            serde_json::from_value(v).map_err(|e| ApiError::invalid(format!("concurrency: {e}")))?
        }
    };
    if concurrency == 0 {
        return Err(ApiError::invalid("concurrency must be at least 1"));
    }
    let opts = args::scrape_options(m)?;
    let sem = Arc::new(Semaphore::new(concurrency));
    let documents: Vec<Value> = rt.block_on(async {
        let handles: Vec<_> = urls
            .into_iter()
            .enumerate()
            .map(|(i, url)| {
                let (s, opts, sem) = (s.clone(), opts.clone(), sem.clone());
                let shown = url.clone();
                let task = tokio::spawn(async move {
                    let _permit = sem.acquire_owned().await;
                    scrape_one(&s, Some(i), url, &opts).await
                });
                (i, shown, task)
            })
            .collect();
        let mut out = Vec::with_capacity(handles.len());
        for (i, url, task) in handles {
            // 按输入顺序等；任务 panic 只算这一项失败
            let outcome = task.await.unwrap_or_else(|e| Outcome {
                index: Some(i),
                url: Some(url),
                result: Err(ApiError::new(
                    pagesift_core::api::ErrorCode::Internal,
                    format!("internal error: {e}"),
                )),
            });
            out.push(document_value(&outcome));
        }
        out
    });
    Ok(json!({"success": true, "documents": documents}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MapArgs {
    limit: Option<usize>,
    search: Option<String>,
    sitemap: Option<String>,
    #[serde(default)]
    ignore_robots_txt: bool,
}

pub fn check_sitemap(s: &str) -> Result<(), ApiError> {
    if matches!(s, "include" | "skip" | "only") {
        Ok(())
    } else {
        Err(ApiError::invalid(format!(
            "sitemap must be include, skip or only, got {s:?}"
        )))
    }
}

fn map_request(url: &str, options: Option<&str>) -> Result<MapRequest, ApiError> {
    let a: MapArgs = args::object(options).and_then(args::parse)?;
    check_url(url)?;
    let sitemap = a.sitemap.unwrap_or_else(|| "include".into());
    check_sitemap(&sitemap)?;
    let mut body = json!({
        "url": url,
        "search": a.search,
        "sitemap": sitemap,
        "ignoreRobotsTxt": a.ignore_robots_txt,
    });
    if let (Some(l), Value::Object(m)) = (a.limit, &mut body) {
        m.insert("limit".into(), l.into());
    }
    let req: MapRequest =
        serde_json::from_value(body).map_err(|e| ApiError::invalid(e.to_string()))?;
    if req.limit > MAX_MAP_LIMIT {
        return Err(ApiError::invalid(format!(
            "limit must not exceed {MAX_MAP_LIMIT}"
        )));
    }
    Ok(req)
}

/// 站点的链接列表：`links` 每项是 `{"url", "title", "description"}`。
pub fn map(rt: &Runtime, s: &Arc<Scraper>, url: &str, options: Option<&str>) -> Value {
    let req = match map_request(url, options) {
        Ok(r) => r,
        Err(e) => return failure_value(&e),
    };
    match rt.block_on(s.map(&req)) {
        Ok(links) => {
            let links: Vec<Value> = links
                .into_iter()
                .map(|l| json!({"url": l.url, "title": l.title, "description": l.description}))
                .collect();
            json!({"success": true, "links": links})
        }
        Err(e) => failure_value(&e),
    }
}
