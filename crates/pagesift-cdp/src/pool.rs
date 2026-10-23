//! 有上限的浏览器池：进程数、标签页数、按页数回收、健康检查。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
    /// 空闲超过这么久的浏览器关掉（保留 `min_browsers` 个）；0 表示一直留着。
    pub idle_timeout: Duration,
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
            idle_timeout: Duration::from_secs(60),
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
    /// 最近一次变成空闲的时间。
    idle_since: Instant,
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
    let by_cpu = (cores * fp.browsers_per_4_cores / 4).max(1);
    let per_browser = (fp.browser_mb + fp.tab_mb * tabs_per_browser as u64).max(1);
    let by_memory = mem_available_mb().map_or(by_cpu, |avail| {
        (avail.saturating_sub(reserve_mb) / per_browser) as usize
    });
    by_memory.clamp(1, by_cpu)
}

/// Linux `/proc/stat` 的（空闲 + iowait，总计）jiffies；读不到时返回 `None`。
fn cpu_times() -> Option<(u64, u64)> {
    let s = std::fs::read_to_string("/proc/stat").ok()?;
    let line = s.lines().next()?.strip_prefix("cpu ")?;
    let v: Vec<u64> = line
        .split_whitespace()
        .filter_map(|x| x.parse().ok())
        .collect();
    let idle = v.get(3)? + v.get(4).copied().unwrap_or(0);
    Some((idle, v.iter().sum()))
}

/// 系统可用内存（MB）；读不到时返回 `None`，不做限制。
/// Linux 读 MemAvailable，macOS 用 `vm_stat` 的 free + inactive + speculative 页。
fn mem_available_mb() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("vm_stat").output().ok()?;
        parse_vm_stat(&String::from_utf8_lossy(&out.stdout))
    }
    #[cfg(not(target_os = "macos"))]
    {
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
}

/// 解析 macOS `vm_stat` 输出，返回可用内存（MB）。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_vm_stat(s: &str) -> Option<u64> {
    let page: u64 = s
        .lines()
        .next()?
        .split("page size of")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |key: &str| -> u64 {
        s.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.trim().trim_end_matches('.').parse().ok())
            .unwrap_or(0)
    };
    let free = pages("Pages free:") + pages("Pages inactive:") + pages("Pages speculative:");
    Some(free * page / (1024 * 1024))
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
    /// 健康检查和自适应调整的后台任务。
    health: std::sync::Mutex<Vec<JoinHandle<()>>>,
    /// 当前允许的浏览器数：固定配置时等于 `max_browsers`，自适应时在 1..=`max_browsers` 里调。
    target: AtomicUsize,
    /// 归还的标签页数（自适应用来算吞吐）。
    completed: AtomicU64,
    /// 这个窗口里有没有请求因为没有空位在排队。
    waited: AtomicBool,
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
        // 自动决定数量的本地池按实测吞吐自适应：从按资源估的数开始，上限是估计的两倍（同样受内存限制）
        let mut target = cfg.max_browsers;
        let adaptive = cfg.max_browsers == 0 && !matches!(cfg.source, BrowserSource::Remote(_));
        if cfg.max_browsers == 0 {
            let fp = cfg.engine.unwrap_or(EngineKind::Chrome).footprint();
            let (tabs, reserve) = (cfg.max_tabs_per_browser, cfg.min_available_memory_mb);
            target = match cfg.source {
                // 远程端点的资源不归本机管，保持保守。
                BrowserSource::Remote(_) => 2,
                _ => auto_browsers_for(fp, tabs, reserve),
            };
            cfg.max_browsers = if adaptive {
                let wide = Footprint {
                    browsers_per_4_cores: fp.browsers_per_4_cores * 2,
                    ..fp
                };
                auto_browsers_for(wide, tabs, reserve).max(target)
            } else {
                target
            };
            tracing::debug!(
                browsers = target,
                max = cfg.max_browsers,
                adaptive,
                "browser pool size"
            );
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
        // 有空闲回收时检查得勤一些，空闲的浏览器最多多留半个超时
        let mut interval = cfg.health_check_interval;
        if !cfg.idle_timeout.is_zero() {
            let half = cfg.idle_timeout / 2;
            interval = if interval.is_zero() {
                half
            } else {
                interval.min(half)
            };
        }
        let inner = Arc::new(PoolInner {
            cfg,
            source,
            guard,
            state: Mutex::new(PoolState::default()),
            notify: Notify::new(),
            health: std::sync::Mutex::new(Vec::new()),
            target: AtomicUsize::new(target.max(1)),
            completed: AtomicU64::new(0),
            waited: AtomicBool::new(false),
        });
        if inner.cfg.min_browsers > 0 {
            inner.health_check().await;
        }
        if !interval.is_zero() {
            let weak = Arc::downgrade(&inner);
            inner
                .health
                .lock()
                .unwrap()
                .push(tokio::spawn(health_loop(weak, interval)));
        }
        if adaptive && inner.cfg.max_browsers > 1 {
            let weak = Arc::downgrade(&inner);
            inner
                .health
                .lock()
                .unwrap()
                .push(tokio::spawn(adapt_loop(weak)));
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
        let acquired = lease.is_some();
        if let Some(l) = lease {
            l.finish().await;
        }
        // 超时时还没拿到标签页：是池子太忙，不是页面慢
        let mut page = r.map_err(|_| {
            if acquired {
                Error::Timeout
            } else {
                Error::Busy
            }
        })??;
        page.timings.close = close_start.elapsed();
        page.timings.total = start.elapsed();
        Ok(page)
    }

    /// 池用的内核种类。
    pub fn engine(&self) -> EngineKind {
        self.inner.cfg.engine.unwrap_or(EngineKind::Chrome)
    }

    /// 启动一个浏览器试试内核能不能用（比如容器里沙箱不可用时 Chrome 起不来）。
    /// 成功的浏览器留在池里，第一个请求不用再等启动。
    pub async fn probe(&self) -> Result<()> {
        let id = {
            let mut st = self.inner.state.lock().await;
            if st.closed {
                return Err(Error::PoolClosed);
            }
            if !st.slots.is_empty() {
                return Ok(());
            }
            st.next_id += 1;
            st.launching += 1;
            st.next_id
        };
        let r = self.inner.launch_slot(id).await;
        let mut st = self.inner.state.lock().await;
        st.launching -= 1;
        match r {
            Ok(slot) if st.closed => {
                drop(st);
                slot.shutdown().await;
                Err(Error::PoolClosed)
            }
            Ok(slot) => {
                st.slots.push(slot);
                drop(st);
                self.inner.notify.notify_waiters();
                Ok(())
            }
            Err(e) => Err(e),
        }
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
        for h in self.inner.health.lock().unwrap().drain(..) {
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

/// 自适应调整的窗口。太短会被单页耗时的抖动带偏，太长对负载变化反应慢。
const ADAPT_WINDOW: Duration = Duration::from_secs(3);
/// 吞吐至少提高这么多才算这一步有用，否则掉头。
const ADAPT_GAIN: f64 = 1.03;

/// 系统 CPU 空闲比例低于这个值时不再加浏览器（加了只会更挤），低于 [`CPU_SHRINK_IDLE`] 时减。
const CPU_GROW_IDLE: f64 = 0.10;
const CPU_SHRINK_IDLE: f64 = 0.05;

/// 爬山法调浏览器数：只在池子满载（有请求在排队）的窗口里调，按上一步的吞吐变化决定继续还是掉头，
/// 步长是当前数的八分之一（至少 1）。不满载时吞吐由请求量决定，不调。系统 CPU 已经吃满时
/// 吞吐的变化多半是噪声（别的进程在抢），这时只减不加。
async fn adapt_loop(pool: Weak<PoolInner>) {
    let mut tick = tokio::time::interval(ADAPT_WINDOW);
    tick.tick().await;
    let (mut last_done, mut last_rate, mut up) = (0u64, 0.0f64, true);
    let mut cpu = cpu_times();
    loop {
        tick.tick().await;
        let Some(pool) = pool.upgrade() else { return };
        let done = pool.completed.load(Ordering::Relaxed);
        let rate = (done - last_done) as f64 / ADAPT_WINDOW.as_secs_f64();
        last_done = done;
        let now = cpu_times();
        let idle = match (cpu, now) {
            (Some((i0, t0)), Some((i1, t1))) if t1 > t0 => {
                Some((i1 - i0) as f64 / (t1 - t0) as f64)
            }
            _ => None,
        };
        cpu = now;
        if !pool.waited.swap(false, Ordering::Relaxed) {
            last_rate = 0.0;
            continue;
        }
        if last_rate > 0.0 && rate < last_rate * ADAPT_GAIN {
            up = !up;
        }
        last_rate = rate;
        match idle {
            Some(i) if i < CPU_SHRINK_IDLE => up = false,
            Some(i) if i < CPU_GROW_IDLE && up => continue,
            _ => {}
        }
        let cur = pool.target.load(Ordering::Relaxed);
        let step = (cur / 8).max(1);
        let next = if up {
            (cur + step).min(pool.cfg.max_browsers)
        } else {
            cur.saturating_sub(step).max(1)
        };
        if next != cur {
            tracing::debug!(from = cur, to = next, rate, "browser pool resized");
            pool.target.store(next, Ordering::Relaxed);
            pool.notify.notify_waiters();
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
            idle_since: Instant::now(),
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
                    None if st.slots.len() + st.launching < self.target.load(Ordering::Relaxed)
                        && (idle || self.memory_allows(fp.browser_mb + fp.tab_mb)) =>
                    {
                        // 启动浏览器要几百毫秒，放到锁外，别的页面照常取用和归还标签页。
                        st.launching += 1;
                        st.next_id += 1;
                        launch = Some(st.next_id);
                        None
                    }
                    _ => {
                        self.waited.store(true, Ordering::Relaxed);
                        None
                    }
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
                // `acquire` is wrapped in the request deadline.  Keep the
                // launching count cancellation-safe so a timeout during a
                // slow browser startup cannot leave the pool permanently full.
                let launch = LaunchReservation::new(self.clone());
                let r = self.launch_slot(id).await;
                let mut st = self.state.lock().await;
                launch.finish(&mut st);
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
            self.completed.fetch_add(1, Ordering::Relaxed);
            if let Some(s) = st.slots.iter_mut().find(|s| s.id == slot_id) {
                s.in_use -= 1;
                if s.in_use == 0 {
                    s.idle_since = Instant::now();
                }
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
            let idle = self.cfg.idle_timeout;
            if !idle.is_zero() {
                let mut keep = self.cfg.min_browsers;
                for s in st.slots.iter_mut().filter(|s| !s.dead && !s.retiring) {
                    if s.in_use == 0 && s.idle_since.elapsed() >= idle && keep == 0 {
                        tracing::debug!(id = s.id, "closing idle browser");
                        s.retiring = true;
                    } else {
                        keep = keep.saturating_sub(1);
                    }
                }
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

struct LaunchReservation {
    pool: Arc<PoolInner>,
    done: bool,
}

impl LaunchReservation {
    fn new(pool: Arc<PoolInner>) -> LaunchReservation {
        LaunchReservation { pool, done: false }
    }

    fn finish(mut self, st: &mut PoolState) {
        debug_assert!(st.launching > 0);
        st.launching = st.launching.saturating_sub(1);
        self.done = true;
    }
}

impl Drop for LaunchReservation {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let pool = self.pool.clone();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let mut st = pool.state.lock().await;
                debug_assert!(st.launching > 0);
                st.launching = st.launching.saturating_sub(1);
                drop(st);
                pool.notify.notify_waiters();
            });
        }
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

#[cfg(test)]
mod vm_stat_tests {
    #[test]
    fn parses_vm_stat() {
        let s = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free:                               1000.\nPages inactive:                           2000.\nPages speculative:                        1000.\n";
        assert_eq!(super::parse_vm_stat(s), Some(4000 * 16384 / 1048576));
    }
}
