//! 有上限的浏览器池：进程数、标签页数、按页数回收、健康检查。

use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;

use crate::conn::Connection;
use crate::engine::{EngineKind, Footprint};
use crate::error::{Error, Result};
use crate::guard::RequestGuard;
use crate::launch::{self, BrowserProcess, LaunchSpec};
use crate::options::{RenderOptions, RenderedPage, Timings};
use crate::page::{PageEnv, PageResources, render_page};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserSource {
    /// 在 `PATH` 和常见安装位置探测 Chrome/Chromium。
    Auto,
    /// 指定可执行文件（Chrome、Chromium 或兼容 CDP 的浏览器）。
    Executable(PathBuf),
    /// 连接已经在运行的浏览器：`ws://…` 或 `http://host:port`。
    Remote(String),
}

#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub source: BrowserSource,
    /// 完整替换默认启动参数，`{user_data_dir}` 会被替换成临时目录。
    /// 浏览器需要在 stdout/stderr 打印 `ws://` 开头的 DevTools 地址。
    pub args_override: Option<Vec<String>>,
    pub extra_args: Vec<String>,
    /// 0 表示按可用内存和 CPU 核数自动决定（见 [`auto_browsers`]）。
    /// 一个浏览器的吞吐受它的 UI 线程限制，几个标签页就能占满，要更高的并发得多开浏览器。
    pub max_browsers: usize,
    pub max_tabs_per_browser: usize,
    /// 每个浏览器处理这么多页面后回收；0 表示不回收。
    pub recycle_after_pages: usize,
    /// 健康检查时保持至少这么多个浏览器在运行。
    pub min_browsers: usize,
    /// 0 表示不做后台健康检查。
    pub health_check_interval: Duration,
    pub launch_timeout: Duration,
    pub allow_execute_javascript: bool,
    /// 开新标签页或启动浏览器前，系统至少要剩这么多可用内存（Linux 的 MemAvailable，MB），
    /// 不够就等别的页面结束。池里没有页面在渲染时不检查，保证总能前进。0 表示不检查。
    pub min_available_memory_mb: u64,
    /// 内核自己挡内网地址（Lightpanda、moli 的 `--block-private-networks` / `--block-cidrs`），
    /// 不再用 CDP 的 Fetch 拦截逐个检查子请求：moli 处理 Fetch 时会和导航互相等待卡死，
    /// Lightpanda 每个请求也能少一次往返。开启时启动参数里必须带这两个参数之一，否则拒绝启动。
    pub engine_network_guard: bool,
    /// 内核种类；`None` 时按可执行文件名判断。决定自动进程数的估算，见 [`EngineKind::configure`]。
    pub engine: Option<EngineKind>,
    /// 测试和压测用：浏览器只能访问本机（源站在 localhost），外网子资源一律不发。
    /// 由 [`EngineKind::configure`] 换成各内核自己的参数。
    pub loopback_only: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        PoolConfig {
            source: BrowserSource::Auto,
            args_override: None,
            extra_args: Vec::new(),
            max_browsers: 0,
            max_tabs_per_browser: 4,
            recycle_after_pages: 100,
            min_browsers: 0,
            health_check_interval: Duration::from_secs(30),
            launch_timeout: Duration::from_secs(20),
            allow_execute_javascript: false,
            min_available_memory_mb: 1024,
            engine_network_guard: false,
            engine: None,
            loopback_only: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserStats {
    pub id: u64,
    /// 本地子进程的 pid（也是它的进程组 id）；远程端点为 `None`。
    pub pid: Option<u32>,
    pub tabs_in_use: usize,
    pub pages_served: usize,
    pub retiring: bool,
}

struct Slot {
    id: u64,
    conn: Connection,
    process: Option<BrowserProcess>,
    user_agent: String,
    in_use: usize,
    served: usize,
    retiring: bool,
    dead: bool,
}

impl Slot {
    fn alive(&mut self) -> bool {
        if !self.dead
            && (self.conn.is_closed() || self.process.as_mut().is_some_and(|p| !p.is_alive()))
        {
            self.dead = true;
        }
        !self.dead
    }

    async fn shutdown(self) {
        drop(self.conn);
        if let Some(p) = self.process {
            p.shutdown().await;
        }
    }
}

#[derive(Default)]
struct PoolState {
    slots: Vec<Slot>,
    /// 正在锁外启动的浏览器数。
    launching: usize,
    next_id: u64,
    closed: bool,
}

/// 因为内存不够在等时，隔多久重新看一次（内存可能被别的进程释放，不一定有归还事件）。
const MEMORY_RECHECK: Duration = Duration::from_millis(250);

/// 自动的浏览器数：可用内存（扣掉 `reserve_mb`）够开几个满载的浏览器，最多每 4 个核一个
/// （每个浏览器的 UI 线程、渲染进程和 GPU 进程大约要占满一个多核）。读不到内存时按核数算。
pub fn auto_browsers(tabs_per_browser: usize, reserve_mb: u64) -> usize {
    auto_browsers_for(EngineKind::Chrome.footprint(), tabs_per_browser, reserve_mb)
}

/// 同 [`auto_browsers`]，按给定内核的资源占用估算。
pub fn auto_browsers_for(fp: Footprint, tabs_per_browser: usize, reserve_mb: u64) -> usize {
    let cores = std::thread::available_parallelism().map_or(1, usize::from);
    let by_cpu = (cores / fp.cores_per_browser.max(1)).max(1);
    let per_browser = (fp.browser_mb + fp.tab_mb * tabs_per_browser as u64).max(1);
    let by_memory = mem_available_mb().map_or(by_cpu, |avail| {
        (avail.saturating_sub(reserve_mb) / per_browser) as usize
    });
    by_memory.clamp(1, by_cpu)
}

/// Linux 的 MemAvailable（MB）；读不到时返回 `None`，不做限制。
fn mem_available_mb() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kb: u64 = s
        .lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    Some(kb / 1024)
}

enum Source {
    Local(LaunchSpec),
    Remote(String),
}

struct PoolInner {
    cfg: PoolConfig,
    source: Source,
    guard: Arc<dyn RequestGuard>,
    state: Mutex<PoolState>,
    notify: Notify,
    health: std::sync::Mutex<Option<JoinHandle<()>>>,
}

/// 浏览器池。克隆后共享同一个池。
#[derive(Clone)]
pub struct BrowserPool {
    inner: Arc<PoolInner>,
}

impl BrowserPool {
    /// 需要在 tokio 运行时里调用。不会立即启动浏览器，除非 `min_browsers > 0`。
    pub async fn new(cfg: PoolConfig, guard: Arc<dyn RequestGuard>) -> Result<BrowserPool> {
        if cfg.engine_network_guard
            && !cfg.args_override.iter().flatten().any(|a| {
                a == "--block-private-networks"
                    || a == "--block-cidrs"
                    || a.starts_with("--block-cidrs=")
            })
        {
            return Err(Error::InvalidOptions(
                "engine_network_guard needs --block-private-networks or --block-cidrs in the browser arguments"
                    .into(),
            ));
        }
        if cfg.max_tabs_per_browser == 0 {
            return Err(Error::InvalidOptions(
                "max_tabs_per_browser must be > 0".into(),
            ));
        }
        let mut cfg = cfg;
        if cfg.engine.is_none() {
            cfg.engine = Some(match &cfg.source {
                BrowserSource::Executable(p) => EngineKind::from_path(p),
                _ => EngineKind::Chrome,
            });
        }
        if cfg.max_browsers == 0 {
            cfg.max_browsers = match cfg.source {
                // 远程端点的资源不归本机管，保持保守。
                BrowserSource::Remote(_) => 2,
                _ => auto_browsers_for(
                    cfg.engine.unwrap_or(EngineKind::Chrome).footprint(),
                    cfg.max_tabs_per_browser,
                    cfg.min_available_memory_mb,
                ),
            };
            tracing::debug!(browsers = cfg.max_browsers, "browser pool size");
        }
        let source = match &cfg.source {
            BrowserSource::Remote(url) => Source::Remote(url.clone()),
            local => {
                launch::ensure_not_root()?;
                let mut all = cfg.args_override.clone().unwrap_or_default();
                all.extend(cfg.extra_args.iter().cloned());
                launch::check_args(&all)?;
                let executable = match local {
                    BrowserSource::Executable(p) => p.clone(),
                    _ => launch::detect_browser().ok_or(Error::NoBrowserFound)?,
                };
                Source::Local(LaunchSpec {
                    executable,
                    args_override: cfg.args_override.clone(),
                    extra_args: cfg.extra_args.clone(),
                    timeout: cfg.launch_timeout,
                })
            }
        };
        let interval = cfg.health_check_interval;
        let inner = Arc::new(PoolInner {
            cfg,
            source,
            guard,
            state: Mutex::new(PoolState::default()),
            notify: Notify::new(),
            health: std::sync::Mutex::new(None),
        });
        if inner.cfg.min_browsers > 0 {
            inner.health_check().await;
        }
        if !interval.is_zero() {
            let weak = Arc::downgrade(&inner);
            *inner.health.lock().unwrap() = Some(tokio::spawn(health_loop(weak, interval)));
        }
        Ok(BrowserPool { inner })
    }

    /// 渲染一个页面。`opts.timeout` 覆盖排队、导航、actions 在内的全部时间。
    pub async fn render(&self, url: &str, opts: &RenderOptions) -> Result<RenderedPage> {
        let start = Instant::now();
        opts.validate(self.inner.cfg.allow_execute_javascript)?;
        let mut lease: Option<Lease> = None;
        let mut timings = Timings::default();
        let r = tokio::time::timeout(opts.timeout, async {
            let l = lease.insert(self.inner.clone().acquire().await?);
            timings.acquire = start.elapsed();
            let env = PageEnv {
                guard: self.inner.guard.clone(),
                user_agent: l.user_agent.clone(),
                intercept: !self.inner.cfg.engine_network_guard,
            };
            render_page(&l.conn, &mut l.res, &env, url, opts, &mut timings).await
        })
        .await;
        let close_start = Instant::now();
        if let Some(l) = lease {
            l.finish().await;
        }
        let mut page = r.map_err(|_| Error::Timeout)??;
        page.timings.close = close_start.elapsed();
        page.timings.total = start.elapsed();
        Ok(page)
    }

    /// 池用的内核种类。
    pub fn engine(&self) -> EngineKind {
        self.inner.cfg.engine.unwrap_or(EngineKind::Chrome)
    }

    /// 立即做一次健康检查：清理崩溃的浏览器，补足 `min_browsers`。
    pub async fn health_check(&self) {
        self.inner.health_check().await;
    }

    pub async fn stats(&self) -> Vec<BrowserStats> {
        let st = self.inner.state.lock().await;
        st.slots
            .iter()
            .map(|s| BrowserStats {
                id: s.id,
                pid: s.process.as_ref().map(BrowserProcess::pid),
                tabs_in_use: s.in_use,
                pages_served: s.served,
                retiring: s.retiring,
            })
            .collect()
    }

    /// 关闭所有浏览器（整组 kill 并回收），之后的 `render` 返回 `PoolClosed`。
    pub async fn shutdown(&self) {
        if let Some(h) = self.inner.health.lock().unwrap().take() {
            h.abort();
        }
        let slots = {
            let mut st = self.inner.state.lock().await;
            st.closed = true;
            std::mem::take(&mut st.slots)
        };
        self.inner.notify.notify_waiters();
        for s in slots {
            s.shutdown().await;
        }
    }
}

async fn health_loop(pool: Weak<PoolInner>, interval: Duration) {
    let mut tick = tokio::time::interval(interval);
    tick.tick().await;
    loop {
        tick.tick().await;
        let Some(pool) = pool.upgrade() else { return };
        pool.health_check().await;
    }
}

impl PoolInner {
    async fn launch_slot(&self, id: u64) -> Result<Slot> {
        let (conn, process) = match &self.source {
            Source::Remote(endpoint) => {
                let ws = launch::resolve_endpoint(endpoint).await?;
                (Connection::connect(&ws).await?, None)
            }
            Source::Local(spec) => {
                let p = BrowserProcess::launch(spec).await?;
                match Connection::connect(p.ws_url()).await {
                    Ok(c) => (c, Some(p)),
                    Err(e) => {
                        p.shutdown().await;
                        return Err(e);
                    }
                }
            }
        };
        let version = tokio::time::timeout(
            self.cfg.launch_timeout,
            conn.call("Browser.getVersion", json!({}), None),
        )
        .await
        .map_err(|_| Error::Launch("Browser.getVersion timed out".into()))
        .and_then(|r| r);
        let version = match version {
            Ok(v) => v,
            Err(e) => {
                if let Some(p) = process {
                    p.shutdown().await;
                }
                return Err(e);
            }
        };
        tracing::debug!(id, product = %version["product"], "browser ready");
        Ok(Slot {
            id,
            conn,
            process,
            user_agent: version["userAgent"].as_str().unwrap_or_default().to_owned(),
            in_use: 0,
            served: 0,
            retiring: false,
            dead: false,
        })
    }

    /// 摘掉已经死掉或待回收且空闲的浏览器，交给调用方在锁外清理。
    fn take_idle_finished(st: &mut PoolState) -> Vec<Slot> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < st.slots.len() {
            let s = &mut st.slots[i];
            if s.in_use == 0 && (!s.alive() || s.retiring) {
                out.push(st.slots.swap_remove(i));
            } else {
                i += 1;
            }
        }
        out
    }

    /// 系统可用内存够不够再开 `need_mb`。
    fn memory_allows(&self, need_mb: u64) -> bool {
        let min = self.cfg.min_available_memory_mb;
        min == 0 || mem_available_mb().is_none_or(|avail| avail >= min + need_mb)
    }

    fn lease(self: &Arc<Self>, s: &mut Slot) -> Lease {
        let recycle = self.cfg.recycle_after_pages;
        s.in_use += 1;
        s.served += 1;
        if recycle > 0 && s.served >= recycle {
            s.retiring = true;
        }
        Lease {
            pool: self.clone(),
            slot_id: s.id,
            conn: s.conn.clone(),
            user_agent: s.user_agent.clone(),
            res: PageResources::default(),
            done: false,
        }
    }

    async fn acquire(self: Arc<Self>) -> Result<Lease> {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let mut launch = None;
            let (lease, finished) = {
                let mut st = self.state.lock().await;
                if st.closed {
                    return Err(Error::PoolClosed);
                }
                let finished = Self::take_idle_finished(&mut st);
                let idle = st.launching == 0 && st.slots.iter().all(|s| s.in_use == 0);
                let max_tabs = self.cfg.max_tabs_per_browser;
                let fp = self.cfg.engine.unwrap_or(EngineKind::Chrome).footprint();
                let idx = st
                    .slots
                    .iter_mut()
                    .position(|s| !s.retiring && s.in_use < max_tabs && s.alive());
                let lease = match idx {
                    Some(i) if idle || self.memory_allows(fp.tab_mb) => {
                        Some(self.lease(&mut st.slots[i]))
                    }
                    None if st.slots.len() + st.launching < self.cfg.max_browsers
                        && (idle || self.memory_allows(fp.browser_mb + fp.tab_mb)) =>
                    {
                        // 启动浏览器要几百毫秒，放到锁外，别的页面照常取用和归还标签页。
                        st.launching += 1;
                        st.next_id += 1;
                        launch = Some(st.next_id);
                        None
                    }
                    _ => None,
                };
                (lease, finished)
            };
            for s in finished {
                s.shutdown().await;
            }
            if let Some(l) = lease {
                return Ok(l);
            }
            if let Some(id) = launch {
                let r = self.launch_slot(id).await;
                let mut st = self.state.lock().await;
                st.launching -= 1;
                let r = match r {
                    Ok(slot) if st.closed => {
                        drop(st);
                        slot.shutdown().await;
                        Err(Error::PoolClosed)
                    }
                    Ok(slot) => {
                        st.slots.push(slot);
                        let l = self.lease(st.slots.last_mut().unwrap());
                        drop(st);
                        Ok(l)
                    }
                    Err(e) => {
                        drop(st);
                        Err(e)
                    }
                };
                // 新浏览器还有空余标签页，或者启动失败让出了名额：叫醒排队的请求。
                self.notify.notify_waiters();
                return r;
            }
            let _ = tokio::time::timeout(MEMORY_RECHECK, notified).await;
        }
    }

    async fn release(&self, slot_id: u64) {
        let finished = {
            let mut st = self.state.lock().await;
            if let Some(s) = st.slots.iter_mut().find(|s| s.id == slot_id) {
                s.in_use -= 1;
            }
            Self::take_idle_finished(&mut st)
        };
        for s in finished {
            s.shutdown().await;
        }
        self.notify.notify_waiters();
    }

    async fn health_check(&self) {
        let idle: Vec<(u64, Connection)> = {
            let st = self.state.lock().await;
            st.slots
                .iter()
                .filter(|s| s.in_use == 0 && !s.dead)
                .map(|s| (s.id, s.conn.clone()))
                .collect()
        };
        let mut failed = Vec::new();
        for (id, conn) in idle {
            let ping = tokio::time::timeout(
                Duration::from_secs(5),
                conn.call("Browser.getVersion", json!({}), None),
            );
            if !matches!(ping.await, Ok(Ok(_))) {
                failed.push(id);
            }
        }
        let finished = {
            let mut st = self.state.lock().await;
            for s in st.slots.iter_mut().filter(|s| failed.contains(&s.id)) {
                tracing::warn!(id = s.id, "browser failed health check");
                s.dead = true;
            }
            let finished = Self::take_idle_finished(&mut st);
            while !st.closed
                && st.slots.len() + st.launching < self.cfg.min_browsers.min(self.cfg.max_browsers)
            {
                st.next_id += 1;
                let id = st.next_id;
                match self.launch_slot(id).await {
                    Ok(slot) => st.slots.push(slot),
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to relaunch browser");
                        break;
                    }
                }
            }
            finished
        };
        for s in finished {
            s.shutdown().await;
        }
        self.notify.notify_waiters();
    }
}

struct Lease {
    pool: Arc<PoolInner>,
    slot_id: u64,
    conn: Connection,
    user_agent: String,
    res: PageResources,
    done: bool,
}

impl Lease {
    async fn finish(mut self) {
        self.done = true;
        let _ = tokio::time::timeout(Duration::from_secs(5), self.res.cleanup(&self.conn)).await;
        self.pool.release(self.slot_id).await;
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        // render 的 future 被调用方丢弃时，在后台归还标签页。
        let (pool, id, conn) = (self.pool.clone(), self.slot_id, self.conn.clone());
        let mut res = std::mem::take(&mut self.res);
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(5), res.cleanup(&conn)).await;
                pool.release(id).await;
            });
        }
    }
}
