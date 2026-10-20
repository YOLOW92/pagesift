//! `Scraper`：抓取在 napi 自带的 tokio 运行时上进行，批量抓取的调度全部在 Rust 侧。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use napi::bindgen_prelude::*;
use napi_derive::napi;
use pagesift::api::{ApiError, ErrorCode};
use tokio::sync::{Mutex, Semaphore, mpsc};
use tokio::task::{Id, JoinError, JoinHandle, JoinSet};

use crate::error::{self, BatchError, Fallible};
use crate::options::{BatchOptions, ScrapeOptions, ScraperConfig, check_url};
use crate::page::Page;

const DEFAULT_CONCURRENCY: u32 = 16;

type ApiOptions = pagesift::api::types::ScrapeOptions;
type Core = Arc<pagesift::Scraper>;

/// 批量抓取里一个 URL 的结果：`page` 和 `error` 恰好有一个。
#[napi(object, object_from_js = false)]
pub struct BatchItem {
    /// 在输入数组里的下标。
    pub index: u32,
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
    #[napi(constructor)]
    pub fn new(env: &Env, config: Option<ScraperConfig>) -> Result<Self> {
        let config = config.unwrap_or_default().into_core();
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
    #[napi(ts_return_type = "Promise<Page>")]
    pub fn scrape<'e>(
        &self,
        env: &'e Env,
        url: String,
        options: Option<ScrapeOptions>,
    ) -> Result<PromiseRaw<'e, Page>> {
        let ready = self
            .live()
            .and_then(|core| Ok((core, options.unwrap_or_default().to_api()?)));
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
    #[napi(ts_return_type = "Promise<BatchItem[]>")]
    pub fn scrape_many<'e>(
        &self,
        env: &'e Env,
        urls: Vec<String>,
        options: Option<BatchOptions>,
    ) -> Result<PromiseRaw<'e, Vec<BatchItem>>> {
        let ready = self.batch(urls, options);
        let fut = async move {
            let (core, opts, urls, concurrency) = match ready {
                Ok(r) => r,
                Err(e) => return Ok(Err(e)),
            };
            let n = urls.len();
            let (tx, mut rx) = mpsc::channel(concurrency);
            let _driver = AbortOnDrop(tokio::spawn(drive(core, opts, urls, concurrency, tx)));
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

    /// 抓取一批 URL，按完成顺序逐个产出（`for await`）。提前 `break` 会取消还没完成的抓取。
    #[napi]
    pub fn scrape_stream(
        &self,
        env: &Env,
        urls: Vec<String>,
        options: Option<BatchOptions>,
    ) -> Result<ScrapeStream> {
        let (core, opts, urls, concurrency) = self
            .batch(urls, options)
            .map_err(|e| error::to_js(env, &e))?;
        let (tx, rx) = mpsc::channel(concurrency);
        let driver = napi::bindgen_prelude::spawn(drive(core, opts, urls, concurrency, tx));
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
        options: Option<BatchOptions>,
    ) -> Fallible<(Core, Arc<ApiOptions>, Vec<String>, usize)> {
        let core = self.live()?;
        let options = options.unwrap_or_default();
        let concurrency = options.concurrency.unwrap_or(DEFAULT_CONCURRENCY).max(1) as usize;
        Ok((core, Arc::new(options.to_api()?), urls, concurrency))
    }
}

/// 按完成顺序产出 `BatchItem` 的异步迭代器。
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

/// 按输入顺序开始抓取，最多 `concurrency` 个同时进行，每完成一个就发到 `tx`。
/// 消费方不要了（`tx` 关闭）时停止；整个任务被取消时，正在进行的抓取一起取消。
async fn drive(
    core: Core,
    opts: Arc<ApiOptions>,
    urls: Vec<String>,
    concurrency: usize,
    tx: mpsc::Sender<BatchItem>,
) {
    let slots = Arc::new(Semaphore::new(concurrency));
    let mut tasks = JoinSet::new();
    // 某一页 panic 时按任务 id 找回它的 URL，照样产出一条错误结果
    let mut pending = HashMap::new();
    for (i, url) in urls.into_iter().enumerate() {
        let Ok(slot) = slots.clone().acquire_owned().await else {
            break;
        };
        while let Some(done) = tasks.try_join_next_with_id() {
            report(done, &mut pending, &tx).await;
        }
        if tx.is_closed() {
            return;
        }
        let (core, opts, tx) = (core.clone(), opts.clone(), tx.clone());
        let target = url.clone();
        let handle = tasks.spawn(async move {
            let _slot = slot;
            let out = scrape_one(&core, &target, &opts).await;
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
