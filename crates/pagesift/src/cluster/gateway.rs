//! 网关一侧：HTTP API 照旧，页面交给工人执行。
//!
//! - [`Dispatch`]：把一个页面编码成任务提交给 [`GatewayCoordinator`]，等工人的结果经
//!   [`Deliver`] 回来；请求被丢弃（任务取消、客户端断开）时同时取消账本里的任务。
//! - [`ClusterEngine`]：实现 `pagesift_api::Engine`。单页抓取和批量任务的每一页走 [`Dispatch`]；
//!   整站爬取的前沿、去重、陷阱学习、聚焦打分留在网关的 `Scraper` 里（单写者），只有逐页抓取经
//!   [`crate::Scraper::route_pages`] 交给 [`Dispatch`]，工人带回的文档里有发现的链接。
//!   所以任务状态、流式输出和 webhook 走的仍是 API 层原来的路径，行为不变。
//! - [`serve_workers`]：接受工人的连接（h2c 或 TLS），交给 `GatewayCoordinator::serve`。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use pagesift_api::types::{
    CrawlRequest, Document, ExtractRequest, MapLink, MapRequest, ScrapeOptions, SearchRequest,
    Upload,
};
use pagesift_api::{ApiError, BoxFuture, CrawlSink, Engine, ErrorCode};
use pagesift_cluster::{
    Coordinator, Deliver, GatewayConfig, GatewayCoordinator, Ledger, Resources, Task, TaskId,
    TokenStore,
};
use pagesift_policy::{RequestKind, RobotsMode};
use pagesift_stat::rng::splitmix64;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_rustls::rustls::ServerConfig;

use super::PageRunner;
use super::task::{class_of, decode_outcome, encode_task, nonce_of, site_of};
use crate::Scraper;

type Waiting = HashMap<u64, oneshot::Sender<Vec<u8>>>;

/// 等待中的请求，按请求号分片（热路径不碰全局锁）。
struct Waiters {
    shards: Box<[Shard]>,
    mask: u64,
}

/// 独占缓存行（[推导] 128 字节：x86 相邻行预取成对取入两条 64 字节缓存行）。
#[repr(align(128))]
#[derive(Default)]
struct Shard(Mutex<Waiting>);

thread_local! {
    /// 每个线程的请求号计数器，起点取自系统随机源（`RandomState` 的键）：每次启动、每个线程都不同，
    /// 上一次运行留下的任务带回的结果认不出来，直接丢弃。
    static NEXT: std::cell::Cell<u64> = std::cell::Cell::new({
        use std::hash::BuildHasher;
        std::collections::hash_map::RandomState::new().hash_one(0u8)
    });
}

impl Waiters {
    fn new() -> Self {
        let n = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .next_power_of_two();
        Self {
            shards: (0..n).map(|_| Shard::default()).collect(),
            mask: n as u64 - 1,
        }
    }

    fn shard(&self, nonce: u64) -> MutexGuard<'_, Waiting> {
        self.shards[(splitmix64(nonce) & self.mask) as usize]
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// 登记一个请求号。号码由线程本地的计数器经 SplitMix64 打散得到（不在共享原子量上计数；
    /// SplitMix64 是 2^64 上的双射，同一线程不会重号），撞上别的线程还在等的号时换一个。
    fn register(&self) -> (u64, oneshot::Receiver<Vec<u8>>) {
        let (tx, rx) = oneshot::channel();
        let mut tx = Some(tx);
        loop {
            let k = NEXT.with(|c| {
                let k = c.get().wrapping_add(1);
                c.set(k);
                k
            });
            let nonce = splitmix64(k);
            let mut m = self.shard(nonce);
            if let std::collections::hash_map::Entry::Vacant(e) = m.entry(nonce) {
                e.insert(tx.take().expect("inserted once"));
                return (nonce, rx);
            }
        }
    }

    fn remove(&self, nonce: u64) {
        self.shard(nonce).remove(&nonce);
    }
}

impl Deliver<Vec<u8>> for Waiters {
    fn deliver(&self, _task: TaskId, result: Vec<u8>) {
        let Some(nonce) = nonce_of(&result) else {
            return;
        };
        let waiter = self.shard(nonce).remove(&nonce);
        if let Some(tx) = waiter {
            // 接收端已丢弃说明请求刚被取消，结果无人要
            let _ = tx.send(result);
        }
    }
}

/// 网关看到的每个工人的计数（`/metrics` 按工人加标签输出）。
#[repr(align(128))]
#[derive(Default)]
struct WorkerCounters {
    pages: std::sync::atomic::AtomicU64,
    errors: std::sync::atomic::AtomicU64,
}

/// 把页面交给工人执行。
pub struct Dispatch {
    gw: GatewayCoordinator,
    waiters: Arc<Waiters>,
    /// 网关的审计日志：工人交回的审计记录在这里统一追加，带工人 id。
    audit: std::sync::OnceLock<Arc<crate::audit::Audit>>,
    workers: std::sync::RwLock<HashMap<Arc<str>, Arc<WorkerCounters>>>,
    /// 请求没给 `timeout` 时任务的总预算（冷启动的租约时长），取本机抓取的总时限。
    default_timeout: Duration,
}

/// 一个已提交、还在等结果的页面；提前丢弃时取消账本里的任务。
struct Pending<'a> {
    d: &'a Dispatch,
    nonce: u64,
    task: Option<TaskId>,
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.d.waiters.remove(self.nonce);
        if let Some(t) = self.task {
            // 已完成或已不存在都没关系：取消只影响还在排队或执行的任务
            let _ = self.d.gw.ledger().cancel(t);
        }
    }
}

impl Dispatch {
    /// `ledger` 以多服务器方式打开（`LedgerConfig::new(clock, false)`），网关和温备接管后都用它。
    pub fn new(
        ledger: Arc<Ledger>,
        tokens: TokenStore,
        cfg: GatewayConfig,
        default_timeout: Duration,
    ) -> Arc<Self> {
        let waiters = Arc::new(Waiters::new());
        let sink: Arc<dyn Deliver<Vec<u8>>> = waiters.clone();
        super::stats::register_stats(&ledger);
        Arc::new(Self {
            gw: GatewayCoordinator::new(ledger, sink, tokens, cfg),
            waiters,
            audit: std::sync::OnceLock::new(),
            workers: std::sync::RwLock::default(),
            default_timeout,
        })
    }

    pub fn coordinator(&self) -> &GatewayCoordinator {
        &self.gw
    }

    /// 在线工人的准入容量之和：工人每次拉取都报告自己统一准入的当前值，
    /// 掉线（断开或被判失联）的工人移出成员表，从和里减掉。
    pub fn capacity(&self) -> usize {
        self.gw.ledger().capacity().round() as usize
    }

    async fn page(
        &self,
        kind: &str,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
    ) -> Result<Document, ApiError> {
        let (nonce, rx) = self.waiters.register();
        let mut pending = Pending {
            d: self,
            nonce,
            task: None,
        };
        let task = Task {
            site: site_of(url),
            class: class_of(opts),
            timeout: opts
                .timeout
                .map_or(self.default_timeout, Duration::from_millis),
            // 工人按自己的准入决定什么时候拉取，任务本身不声明资源需要量
            need: Resources::default(),
            body: encode_task(nonce, kind, url, opts, mode),
        };
        let id = self
            .gw
            .submit(task)
            .await
            .map_err(|e| ApiError::new(ErrorCode::Internal, format!("cluster submit: {e}")))?;
        pending.task = Some(id);
        let bytes = rx
            .await
            .map_err(|_| ApiError::new(ErrorCode::Internal, "gateway is shutting down"))?;
        // 结果已交付：不再取消
        pending.task = None;
        drop(pending);
        let reply = decode_outcome(&bytes);
        self.note(&reply);
        reply.result
    }

    /// 记下工人交回的结果：按工人计数，审计记录追加进网关的审计日志。
    fn note(&self, reply: &super::task::Reply) {
        use std::sync::atomic::Ordering::Relaxed;
        if let Some(audit) = self.audit.get() {
            for rec in &reply.audit {
                audit.append_from(rec.clone(), &reply.worker);
            }
        }
        if reply.worker.is_empty() {
            return;
        }
        let known = self
            .workers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(reply.worker.as_str())
            .cloned();
        let c = known.unwrap_or_else(|| {
            self.workers
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .entry(reply.worker.as_str().into())
                .or_default()
                .clone()
        });
        match reply.result {
            Ok(_) => c.pages.fetch_add(1, Relaxed),
            Err(_) => c.errors.fetch_add(1, Relaxed),
        };
    }

    /// `/metrics` 里按工人加标签的部分（Prometheus 文本格式）：网关收到的每个工人的页面数、错误数，
    /// 在线工人报告的准入容量。
    pub fn metrics(&self) -> String {
        use std::fmt::Write;
        use std::sync::atomic::Ordering::Relaxed;
        let mut out = String::new();
        let workers = self.workers.read().unwrap_or_else(|e| e.into_inner());
        let mut ids: Vec<_> = workers.keys().cloned().collect();
        ids.sort();
        // Prometheus 文本格式的标签值转义（反斜杠、双引号）
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let _ = writeln!(out, "# TYPE pagesift_cluster_worker_pages_total counter");
        for id in &ids {
            let n = workers[id].pages.load(Relaxed);
            let id = esc(id);
            let _ = writeln!(
                out,
                "pagesift_cluster_worker_pages_total{{worker=\"{id}\"}} {n}"
            );
        }
        let _ = writeln!(out, "# TYPE pagesift_cluster_worker_errors_total counter");
        for id in &ids {
            let n = workers[id].errors.load(Relaxed);
            let id = esc(id);
            let _ = writeln!(
                out,
                "pagesift_cluster_worker_errors_total{{worker=\"{id}\"}} {n}"
            );
        }
        let _ = writeln!(out, "# TYPE pagesift_cluster_worker_capacity gauge");
        for (id, c) in self.gw.ledger().capacities() {
            let id = esc(id.as_str());
            let _ = writeln!(
                out,
                "pagesift_cluster_worker_capacity{{worker=\"{id}\"}} {c}"
            );
        }
        out
    }
}

impl PageRunner for Dispatch {
    fn run<'a>(
        &'a self,
        kind: &'static str,
        url: &'a str,
        opts: &'a ScrapeOptions,
        mode: RobotsMode,
    ) -> BoxFuture<'a, Result<Document, ApiError>> {
        Box::pin(self.page(kind, url, opts, mode))
    }

    fn capacity(&self) -> Option<usize> {
        Some(self.capacity())
    }
}

/// 网关的抓取引擎：页面交给工人，其余（爬取前沿、map、search、extract、上传文件解析）在网关本机。
pub struct ClusterEngine {
    scraper: Arc<Scraper>,
    dispatch: Arc<Dispatch>,
}

impl ClusterEngine {
    /// 同时把 `scraper` 的爬取页面接到 `dispatch` 上。
    pub fn new(scraper: Arc<Scraper>, dispatch: Arc<Dispatch>) -> Self {
        scraper.route_pages(dispatch.clone());
        if let Some(a) = scraper.audit() {
            let _ = dispatch.audit.set(a.clone());
        }
        Self { scraper, dispatch }
    }
}

impl Engine for ClusterEngine {
    fn scrape<'a>(
        &'a self,
        url: &'a str,
        opts: &'a ScrapeOptions,
    ) -> BoxFuture<'a, Result<Document, ApiError>> {
        Box::pin(async move {
            let mode = self
                .scraper
                .policy()
                .robots_mode(RequestKind::Scrape, opts.ignore_robots_txt)
                .map_err(crate::engine::policy_error)?;
            self.dispatch.page("scrape", url, opts, mode).await
        })
    }

    fn parse<'a>(
        &'a self,
        file: Upload,
        opts: &'a ScrapeOptions,
    ) -> BoxFuture<'a, Result<Document, ApiError>> {
        Engine::parse(&*self.scraper, file, opts)
    }

    fn map<'a>(&'a self, req: &'a MapRequest) -> BoxFuture<'a, Result<Vec<MapLink>, ApiError>> {
        Engine::map(&*self.scraper, req)
    }

    fn crawl<'a>(
        &'a self,
        req: &'a CrawlRequest,
        sink: Arc<dyn CrawlSink>,
    ) -> BoxFuture<'a, Result<(), ApiError>> {
        Engine::crawl(&*self.scraper, req, sink)
    }

    fn search<'a>(
        &'a self,
        req: &'a SearchRequest,
    ) -> BoxFuture<'a, Result<serde_json::Value, ApiError>> {
        Engine::search(&*self.scraper, req)
    }

    fn extract<'a>(
        &'a self,
        req: &'a ExtractRequest,
    ) -> BoxFuture<'a, Result<serde_json::Value, ApiError>> {
        Engine::extract(&*self.scraper, req)
    }

    fn supported_formats(&self) -> &'static [&'static str] {
        Engine::supported_formats(&*self.scraper)
    }

    fn supports_actions(&self) -> bool {
        Engine::supports_actions(&*self.scraper)
    }

    /// 在线工人准入值之和（至少 1：没有工人在线时任务照常排队等工人）。
    fn admitted_concurrency(&self) -> Option<usize> {
        Some(self.dispatch.capacity().max(1))
    }
}

/// 接受工人（和温备）的连接，直到监听出错。每条连接一个任务；TLS 握手失败的连接直接关掉。
pub async fn serve_workers(
    gw: GatewayCoordinator,
    listener: TcpListener,
    tls: Option<Arc<ServerConfig>>,
) -> std::io::Result<()> {
    loop {
        let (tcp, peer) = listener.accept().await?;
        let _ = tcp.set_nodelay(true);
        let gw = gw.clone();
        let tls = tls.clone();
        tokio::spawn(async move {
            let r = match tls {
                Some(cfg) => match pagesift_cluster::transport::tls::accept(cfg, tcp).await {
                    Ok(s) => gw.serve(s).await,
                    Err(e) => Err(e),
                },
                None => gw.serve(tcp).await,
            };
            if let Err(e) = r {
                tracing::debug!("cluster connection from {peer} ended: {e}");
            }
        });
    }
}
