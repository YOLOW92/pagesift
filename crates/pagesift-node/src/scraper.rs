//! `Scraper` 类和模块级的 `scrape` / `scrapeMany`。抓取在 napi 自带的 tokio 运行时上进行。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use napi::bindgen_prelude::*;
use napi_derive::napi;
use pagesift::api::types::Document;
use pagesift::api::{ApiError, ErrorCode};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::error::{self, Fallible};
use crate::options::{ScrapeManyOptions, ScrapeOptions, ScraperConfig, check_url};

const DEFAULT_CONCURRENCY: u32 = 8;

type Outcome = Fallible<Document>;
type ScrapeOptionsApi = pagesift::api::types::ScrapeOptions;

/// 抓取器：内部的连接池、限速状态和浏览器池在多次抓取之间复用。
#[napi]
pub struct Scraper {
    inner: Arc<pagesift::Scraper>,
    closed: AtomicBool,
}

fn new_core(env: &Env, config: ScraperConfig) -> Result<Arc<pagesift::Scraper>> {
    napi::bindgen_prelude::within_runtime_if_available(|| {
        pagesift::Scraper::new(config.into_core())
    })
    .map(Arc::new)
    .map_err(|e| error::to_js(env, &ApiError::new(ErrorCode::Internal, e.to_string())))
}

#[napi]
impl Scraper {
    #[napi(constructor)]
    pub fn new(env: &Env, config: Option<ScraperConfig>) -> Result<Self> {
        Ok(Self {
            inner: new_core(env, config.unwrap_or_default())?,
            closed: AtomicBool::new(false),
        })
    }

    /// 抓取一个 URL。失败时 Promise 以 `PagesiftError` 拒绝。
    #[napi(ts_return_type = "Promise<Document>")]
    pub fn scrape<'e>(
        &self,
        env: &'e Env,
        url: String,
        options: Option<ScrapeOptions>,
    ) -> Result<PromiseRaw<'e, Unknown<'e>>> {
        let opts = options.unwrap_or_default().to_api();
        let core = self.live();
        spawn_one(env, core, url, opts)
    }

    /// 抓取一批 URL，结果顺序和输入一致；单个失败以 `ErrorResult` 放进数组，不影响其他 URL。
    #[napi(ts_return_type = "Promise<Array<Document | ErrorResult>>")]
    pub fn scrape_many<'e>(
        &self,
        env: &'e Env,
        urls: Vec<String>,
        options: Option<ScrapeManyOptions>,
    ) -> Result<PromiseRaw<'e, Vec<Unknown<'e>>>> {
        let core = self.live();
        spawn_many(env, core, urls, options.unwrap_or_default())
    }

    /// 关闭浏览器进程。之后再调用 `scrape` 会报错。
    #[napi]
    pub async fn close(&self) -> Result<()> {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.inner.shutdown().await;
        }
        Ok(())
    }
}

impl Scraper {
    fn live(&self) -> Fallible<Arc<pagesift::Scraper>> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(ApiError::invalid("this Scraper has been closed"));
        }
        Ok(self.inner.clone())
    }
}

fn default_core(env: &Env) -> Result<Arc<pagesift::Scraper>> {
    let state = crate::state(env)?;
    if let Some(s) = &state.default_scraper {
        return Ok(s.clone());
    }
    let s = new_core(env, ScraperConfig::default())?;
    state.default_scraper = Some(s.clone());
    Ok(s)
}

/// 用默认配置的共享 `Scraper` 抓取一个 URL（默认拒绝内网地址）。
#[napi(ts_return_type = "Promise<Document>")]
pub fn scrape<'e>(
    env: &'e Env,
    url: String,
    options: Option<ScrapeOptions>,
) -> Result<PromiseRaw<'e, Unknown<'e>>> {
    let opts = options.unwrap_or_default().to_api();
    spawn_one(env, Ok(default_core(env)?), url, opts)
}

/// 用默认配置的共享 `Scraper` 抓取一批 URL，见 `Scraper.scrapeMany`。
#[napi(ts_return_type = "Promise<Array<Document | ErrorResult>>")]
pub fn scrape_many<'e>(
    env: &'e Env,
    urls: Vec<String>,
    options: Option<ScrapeManyOptions>,
) -> Result<PromiseRaw<'e, Vec<Unknown<'e>>>> {
    spawn_many(
        env,
        Ok(default_core(env)?),
        urls,
        options.unwrap_or_default(),
    )
}

async fn scrape_one(core: Arc<pagesift::Scraper>, url: &str, opts: &ScrapeOptionsApi) -> Outcome {
    check_url(url)?;
    core.scrape(url, opts).await
}

fn spawn_one<'e>(
    env: &'e Env,
    core: Fallible<Arc<pagesift::Scraper>>,
    url: String,
    opts: Fallible<ScrapeOptionsApi>,
) -> Result<PromiseRaw<'e, Unknown<'e>>> {
    let fut = async move {
        let out = match (core, opts) {
            (Ok(core), Ok(opts)) => scrape_one(core, &url, &opts).await,
            (Err(e), _) | (_, Err(e)) => Err(e),
        };
        Ok(out)
    };
    env.spawn_future_with_callback(fut, |env, out: Outcome| match out {
        Ok(doc) => env.to_js_value(&doc),
        Err(e) => Err(error::to_js(env, &e)),
    })
}

fn spawn_many<'e>(
    env: &'e Env,
    core: Fallible<Arc<pagesift::Scraper>>,
    urls: Vec<String>,
    options: ScrapeManyOptions,
) -> Result<PromiseRaw<'e, Vec<Unknown<'e>>>> {
    let concurrency = options.concurrency.unwrap_or(DEFAULT_CONCURRENCY).max(1) as usize;
    let opts = options.to_api();
    let fut = async move {
        // 整批共用的前提（选项、Scraper 状态）出错时整个 Promise 拒绝
        let (core, opts) = match (core, opts) {
            (Ok(c), Ok(o)) => (c, Arc::new(o)),
            (Err(e), _) | (_, Err(e)) => return Ok(Err(e)),
        };
        let permits = Arc::new(Semaphore::new(concurrency));
        let mut tasks = JoinSet::new();
        for (i, url) in urls.iter().cloned().enumerate() {
            let (core, opts, permits) = (core.clone(), opts.clone(), permits.clone());
            tasks.spawn(async move {
                let _permit = permits.acquire_owned().await;
                (i, scrape_one(core, &url, &opts).await)
            });
        }
        let mut results: Vec<Option<Outcome>> = (0..urls.len()).map(|_| None).collect();
        while let Some(joined) = tasks.join_next().await {
            if let Ok((i, out)) = joined {
                results[i] = Some(out);
            }
        }
        let results = urls
            .into_iter()
            .zip(results)
            .map(|(url, r)| {
                let r = r.unwrap_or_else(|| {
                    Err(ApiError::new(ErrorCode::Internal, "scrape task panicked"))
                });
                (url, r)
            })
            .collect::<Vec<_>>();
        Ok(Ok(results))
    };
    env.spawn_future_with_callback(fut, |env, out| {
        let results: Vec<(String, Outcome)> = out.map_err(|e| error::to_js(env, &e))?;
        results
            .into_iter()
            .map(|(url, r)| match r {
                Ok(doc) => env.to_js_value(&doc),
                Err(e) => env.to_js_value(&error::result(&url, &e)),
            })
            .collect()
    })
}
