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

mod inner;
mod resources;

use inner::*;
use resources::*;
pub use resources::{auto_browsers, auto_browsers_for};

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
