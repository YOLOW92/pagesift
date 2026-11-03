//! `Scraper`：抓取在 napi 自带的 tokio 运行时上进行，批量抓取的调度全部在 Rust 侧。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use napi::bindgen_prelude::*;
use napi_derive::napi;
use pagesift::api::{ApiError, ErrorCode};
use serde_json::Value;
use tokio::sync::{Mutex, Semaphore, mpsc};
use tokio::task::{Id, JoinError, JoinHandle, JoinSet};

use crate::crawl::{self, CrawlOptions, CrawlStream, MapLink, MapOptions};
use crate::error::{self, BatchError, Fallible};
use crate::options::{BatchOptions, ParseOptions, ScrapeOptions, ScraperConfig, check_url};
use crate::page::Page;
use crate::parse::{self, FileInput, ParseFile};

const DEFAULT_CONCURRENCY: u32 = 16;

type ApiOptions = pagesift::api::types::ScrapeOptions;
type Core = Arc<pagesift::Scraper>;

/// 批量抓取（或批量解析）里一项的结果：`page` 和 `error` 恰好有一个。
#[napi(object, object_from_js = false)]
pub struct BatchItem {
    /// 在输入数组里的下标。
    pub index: u32,
    /// 抓取时是 URL，解析时是文件名。
    pub url: String,
    pub page: Option<Page>,
    pub error: Option<BatchError>,
}

impl BatchItem {
    fn new(index: usize, url: String, out: Fallible<Page>) -> Self {
        let (page, error) = match out {
            Ok(p) => (Some(p), None),
            Err(e) => (None, Some(e.into())),
        };
        Self {
            index: index as u32,
            url,
            page,
            error,
        }
    }
}

/// 抓取器：连接池、robots.txt 缓存、限速状态和浏览器池都在它里面，长期复用一个实例最快。
#[napi]
pub struct Scraper {
    inner: Core,
    closed: AtomicBool,
}

#[napi]
impl Scraper {
    #[napi(
        constructor,
        ts_args_type = "config?: ScraperConfig | undefined | null"
    )]
    pub fn new(env: &Env, config: Option<Value>) -> Result<Self> {
        let config = crate::error::strict::<ScraperConfig>(config)
            .and_then(ScraperConfig::into_core)
            .map_err(|e| error::to_js(env, &e))?;
        let inner = napi::bindgen_prelude::within_runtime_if_available(|| {
            pagesift::Scraper::new(config)
        })
        .map_err(|e| error::to_js(env, &ApiError::new(ErrorCode::Internal, e.to_string())))?;
        Ok(Self {
            inner: Arc::new(inner),
            closed: AtomicBool::new(false),
        })
    }

    /// 抓取一个 URL。失败时 Promise 以 `PagesiftError` 拒绝。
    #[napi(
        ts_args_type = "url: string, options?: ScrapeOptions | undefined | null",
        ts_return_type = "Promise<Page>"
    )]
    pub fn scrape<'e>(
        &self,
        env: &'e Env,
        url: String,
        options: Option<Value>,
    ) -> Result<PromiseRaw<'e, Page>> {
        let ready = self.live().and_then(|core| {
            let opts = crate::error::strict::<ScrapeOptions>(options)?.to_api()?;
            Ok((core, opts))
        });
        let fut = async move {
            Ok(match ready {
                Ok((core, opts)) => scrape_one(&core, &url, &opts).await,
                Err(e) => Err(e),
            })
        };
        env.spawn_future_with_callback(fut, |env, out: Fallible<Page>| {
            out.map_err(|e| error::to_js(env, &e))
        })
    }

    /// 抓取一批 URL，全部完成后一次返回，顺序和输入一致；单个失败只影响它自己的那一项。
    #[napi(
        ts_args_type = "urls: Array<string>, options?: BatchOptions | undefined | null",
        ts_return_type = "Promise<BatchItem[]>"
    )]
    pub fn scrape_many<'e>(
        &self,
        env: &'e Env,
        urls: Vec<String>,
        options: Option<Value>,
    ) -> Result<PromiseRaw<'e, Vec<BatchItem>>> {
        let ready = self.batch(urls, options);
        let fut = async move {
            let (core, opts, urls, concurrency) = match ready {
                Ok(r) => r,
                Err(e) => return Ok(Err(e)),
            };
            let n = urls.len();
            let (tx, mut rx) = mpsc::channel(concurrency);
            let _driver = AbortOnDrop(tokio::spawn(drive_scrape(
                core,
                opts,
                urls,
                concurrency,
                tx,
            )));
            let mut items: Vec<Option<BatchItem>> = (0..n).map(|_| None).collect();
            while let Some(item) = rx.recv().await {
                let i = item.index as usize;
                items[i] = Some(item);
            }
            Ok(Ok(items.into_iter().flatten().collect::<Vec<_>>()))
        };
        env.spawn_future_with_callback(fut, |env, out: Fallible<Vec<BatchItem>>| {
            out.map_err(|e| error::to_js(env, &e))
        })
    }

    /// 按 CSS schema 批量抽取，全部完成后按输入顺序返回；结果在 `page.extract`。
    /// 等同于 `scrapeMany(urls, { ...options, extractSchema: schema })`。
    #[napi(
        ts_args_type = "urls: Array<string>, schema: Record<string, unknown>, options?: BatchOptions | undefined | null",
        ts_return_type = "Promise<BatchItem[]>"
    )]
    pub fn extract<'e>(
        &self,
        env: &'e Env,
        urls: Vec<String>,
        schema: Value,
        options: Option<Value>,
    ) -> Result<PromiseRaw<'e, Vec<BatchItem>>> {
        self.scrape_many(env, urls, Some(with_schema(options, schema)))
    }

    /// 按 CSS schema 批量抽取，按完成顺序逐个产出（同 `scrapeStream`）。
    #[napi(
        ts_args_type = "urls: Array<string>, schema: Record<string, unknown>, options?: BatchOptions | undefined | null"
    )]
    pub fn extract_stream(
        &self,
        env: &Env,
        urls: Vec<String>,
        schema: Value,
        options: Option<Value>,
    ) -> Result<ScrapeStream> {
        self.scrape_stream(env, urls, Some(with_schema(options, schema)))
    }

    /// 抓取一批 URL，按完成顺序逐个产出（`for await`）。提前 `break` 会取消还没完成的抓取。
    #[napi(ts_args_type = "urls: Array<string>, options?: BatchOptions | undefined | null")]
    pub fn scrape_stream(
        &self,
        env: &Env,
        urls: Vec<String>,
        options: Option<Value>,
    ) -> Result<ScrapeStream> {
        let (core, opts, urls, concurrency) = self
            .batch(urls, options)
            .map_err(|e| error::to_js(env, &e))?;
        let (tx, rx) = mpsc::channel(concurrency);
        let driver = napi::bindgen_prelude::spawn(drive_scrape(core, opts, urls, concurrency, tx));
        Ok(ScrapeStream {
            rx: Arc::new(Mutex::new(rx)),
            _driver: AbortOnDrop(driver),
        })
    }

    /// 整站爬取，每完成一页产出一项（`for await`）；提前 `break` 或 `cancel()` 停止爬取。
    #[napi(ts_args_type = "url: string, options?: CrawlOptions | undefined | null")]
    pub fn crawl(&self, env: &Env, url: String, options: Option<Value>) -> Result<CrawlStream> {
        self.live()
            .and_then(|core| {
                let o = crate::error::strict::<CrawlOptions>(options)?;
                crawl::start(core, url, o)
            })
            .map_err(|e| error::to_js(env, &e))
    }

    /// 站点的链接列表（sitemap 加页面上的链接）。失败时 Promise 以 `PagesiftError` 拒绝。
    #[napi(
        ts_args_type = "url: string, options?: MapOptions | undefined | null",
        ts_return_type = "Promise<MapLink[]>"
    )]
    pub fn map<'e>(
        &self,
        env: &'e Env,
        url: String,
        options: Option<Value>,
    ) -> Result<PromiseRaw<'e, Vec<MapLink>>> {
        let ready = self.live().and_then(|core| {
            let o = crate::error::strict::<MapOptions>(options)?;
            Ok((core, crawl::map_request(url, o)?))
        });
        let fut = async move {
            Ok(match ready {
                Ok((core, req)) => core
                    .map(&req)
                    .await
                    .map(|links| links.into_iter().map(MapLink::from).collect()),
                Err(e) => Err(e),
            })
        };
        env.spawn_future_with_callback(fut, |env, out: Fallible<Vec<MapLink>>| {
            out.map_err(|e| error::to_js(env, &e))
        })
    }

    /// 解析一个文件（PDF、HTML、Word、Excel、PowerPoint、EPUB、CSV），结果和 `scrape` 一样。
    /// 类型按内容识别，认不出时再看文件名的扩展名和 `contentType`。失败时 Promise 以 `PagesiftError` 拒绝。
    #[napi(
        ts_args_type = "data: Uint8Array, filename: string, options?: ParseOptions | undefined | null",
        ts_return_type = "Promise<Page>"
    )]
    pub fn parse<'e>(
        &self,
        env: &'e Env,
        data: Uint8Array,
        filename: String,
        options: Option<Value>,
    ) -> Result<PromiseRaw<'e, Page>> {
        let options = match crate::error::strict::<ParseOptions>(options) {
            Ok(o) => o,
            Err(e) => return Err(error::to_js(env, &e)),
        };
        let file = FileInput {
            bytes: data.to_vec(),
            filename,
            content_type: options.content_type.clone(),
            url: options.url.clone(),
        };
        let ready = self.live().and_then(|core| {
            let opts = options.to_api()?;
            parse::check_options(&core, &opts)?;
            Ok((core, opts))
        });
        let fut = async move {
            Ok(match ready {
                Ok((core, opts)) => parse::parse_one(&core, file, &opts).await,
                Err(e) => Err(e),
            })
        };
        env.spawn_future_with_callback(fut, |env, out: Fallible<Page>| {
            out.map_err(|e| error::to_js(env, &e))
        })
    }

    /// 解析一批文件，按完成顺序逐个产出（`for await`），`url` 是文件名；并发和取消语义同 `scrapeStream`。
    #[napi(ts_args_type = "files: Array<ParseFile>, options?: BatchOptions | undefined | null")]
    pub fn parse_stream(
        &self,
        env: &Env,
        files: Vec<ParseFile>,
        options: Option<Value>,
    ) -> Result<ScrapeStream> {
        let files: Vec<(String, FileInput)> = files
            .into_iter()
            .map(|f| (f.filename.clone(), FileInput::from(f)))
            .collect();
        let (core, opts, concurrency) = self
            .batch(Vec::new(), options)
            .and_then(|(core, opts, _, concurrency)| {
                parse::check_options(&core, &opts)?;
                Ok((core, opts, concurrency))
            })
            .map_err(|e| error::to_js(env, &e))?;
        let (tx, rx) = mpsc::channel(concurrency);
        let job = move |_: String, f: FileInput| {
            let (core, opts) = (core.clone(), opts.clone());
            async move { parse::parse_one(&core, f, &opts).await }
        };
        let driver = napi::bindgen_prelude::spawn(drive(files, concurrency, tx, job));
        Ok(ScrapeStream {
            rx: Arc::new(Mutex::new(rx)),
            _driver: AbortOnDrop(driver),
        })
    }

    /// 关闭浏览器进程。之后再调用抓取方法会报错。
    #[napi]
    pub async fn close(&self) -> Result<()> {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.inner.shutdown().await;
        }
        Ok(())
    }
}

/// 选项对象里加上 `extractSchema`（没给选项时新建一个）。
fn with_schema(options: Option<Value>, schema: Value) -> Value {
    let mut o = match options {
        Some(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    o.insert("extractSchema".into(), schema);
    Value::Object(o)
}

impl Scraper {
    fn live(&self) -> Fallible<Core> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(ApiError::invalid("this Scraper has been closed"));
        }
        Ok(self.inner.clone())
    }

    fn batch(
        &self,
        urls: Vec<String>,
        options: Option<Value>,
    ) -> Fallible<(Core, Arc<ApiOptions>, Vec<String>, usize)> {
        let core = self.live()?;
        let options = crate::error::strict::<BatchOptions>(options)?;
        let concurrency = options.concurrency.unwrap_or(DEFAULT_CONCURRENCY).max(1) as usize;
        Ok((core, Arc::new(options.to_api()?), urls, concurrency))
    }
}

/// 按完成顺序产出 `BatchItem` 的异步迭代器（`scrapeStream`、`parseStream`）。
#[napi(async_iterator)]
pub struct ScrapeStream {
    rx: Arc<Mutex<mpsc::Receiver<BatchItem>>>,
    _driver: AbortOnDrop,
}

#[napi]
impl AsyncGenerator for ScrapeStream {
    type Yield = BatchItem;
    type Next = ();
    type Return = ();

    fn next(
        &mut self,
        _value: Option<()>,
    ) -> impl Future<Output = Result<Option<BatchItem>>> + Send + 'static {
        let rx = self.rx.clone();
        async move { Ok(rx.lock().await.recv().await) }
    }

    fn complete(
        &mut self,
        _value: Option<()>,
    ) -> impl Future<Output = Result<Option<BatchItem>>> + Send + 'static {
        self._driver.0.abort();
        async { Ok(None) }
    }
}

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn scrape_one(core: &pagesift::Scraper, url: &str, opts: &ApiOptions) -> Fallible<Page> {
    check_url(url)?;
    core.scrape(url, opts).await.map(Page::from_scrape)
}

async fn drive_scrape(
    core: Core,
    opts: Arc<ApiOptions>,
    urls: Vec<String>,
    concurrency: usize,
    tx: mpsc::Sender<BatchItem>,
) {
    let items = urls.into_iter().map(|u| (u, ())).collect();
    let job = move |url: String, ()| {
        let (core, opts) = (core.clone(), opts.clone());
        async move { scrape_one(&core, &url, &opts).await }
    };
    drive(items, concurrency, tx, job).await;
}

/// 按输入顺序开始处理（第 `i` 项交给 `job(名字, 数据)`），最多 `concurrency` 个同时进行，每完成一个就发到 `tx`。
/// 消费方不要了（`tx` 关闭）时停止；整个任务被取消时，正在进行的处理一起取消。
async fn drive<T, F, Fut>(
    items: Vec<(String, T)>,
    concurrency: usize,
    tx: mpsc::Sender<BatchItem>,
    job: F,
) where
    T: Send + 'static,
    F: Fn(String, T) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Fallible<Page>> + Send + 'static,
{
    let job = Arc::new(job);
    let slots = Arc::new(Semaphore::new(concurrency));
    let mut tasks = JoinSet::new();
    // 某一项 panic 时按任务 id 找回它的名字，照样产出一条错误结果
    let mut pending = HashMap::new();
    for (i, (url, item)) in items.into_iter().enumerate() {
        let Ok(slot) = slots.clone().acquire_owned().await else {
            break;
        };
        while let Some(done) = tasks.try_join_next_with_id() {
            report(done, &mut pending, &tx).await;
        }
        if tx.is_closed() {
            return;
        }
        let (job, tx) = (job.clone(), tx.clone());
        let target = url.clone();
        let handle = tasks.spawn(async move {
            let _slot = slot;
            let out = job(target.clone(), item).await;
            let _ = tx.send(BatchItem::new(i, target, out)).await;
        });
        pending.insert(handle.id(), (i, url));
    }
    while let Some(done) = tasks.join_next_with_id().await {
        report(done, &mut pending, &tx).await;
    }
}

async fn report(
    done: std::result::Result<(Id, ()), JoinError>,
    pending: &mut HashMap<Id, (usize, String)>,
    tx: &mpsc::Sender<BatchItem>,
) {
    match done {
        Ok((id, ())) => {
            pending.remove(&id);
        }
        Err(e) => {
            if let Some((i, url)) = pending.remove(&e.id()) {
                let err = ApiError::new(ErrorCode::Internal, e.to_string());
                let _ = tx.send(BatchItem::new(i, url, Err(err))).await;
            }
        }
    }
}
