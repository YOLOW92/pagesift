//! 池的内部调度：借还标签页、启动和回收浏览器、健康检查、按吞吐自适应调整浏览器数。

use pagesift_stat::Delta;
use pagesift_stat::limit::UslLimit;

use super::*;

/// 第一个观测窗的时长（先验包 v0：0.81 的 `ADAPT_WINDOW`）；之后的窗口按样本量截。
/// [策略] 先验包 v0：0.81 的现值
const FIRST_WINDOW: Duration = Duration::from_secs(3);

/// 按通用可扩展性定律调浏览器数（`pagesift_stat::limit::UslLimit`）。
///
/// 每个观测窗给 USL 一个点：平均在用的浏览器数 `N̄`（Little 定律：标签页占用总时长 / 窗长 /
/// 每进程标签数）和吞吐 `X = 完成数 / 窗长`。窗口按样本量截：攒够 `⌈目标浏览器数 × 每进程标签数⌉`
/// 个完成（在途集合整体周转一次，UslLimit 文档的约定）；第一个窗口用 0.81 的 3 s。
/// 拟合出峰值且当前目标显著越过峰值时向 `N*` 的 `1−δ` 下界收；还看不到回落时只在满载
/// （有请求排队）的窗口里向上探 `√N` 步，不超过按内存和核数估的上限。
/// 取代 0.81 的爬山法（`ADAPT_GAIN 1.03`、`CPU_GROW_IDLE / CPU_SHRINK_IDLE`、步长 `cur/8`）：
/// 系统 CPU 吃满时吞吐不再随 `N` 增长，USL 的 `κ` 项会把它反映出来，不再需要单独的 CPU 门槛。
pub(super) async fn adapt_loop(pool: Weak<PoolInner>) {
    let mut limit = match pool.upgrade() {
        Some(p) => UslLimit::new(p.target.load(Ordering::Relaxed) as f64, Delta::DEFAULT),
        None => return,
    };
    let mut nap = FIRST_WINDOW;
    let mut start = Instant::now();
    let mut done0 = 0u64;
    if let Some(p) = pool.upgrade() {
        done0 = p.completed.load(Ordering::Relaxed);
        p.state.lock().await.busy = Duration::ZERO;
    }
    loop {
        tokio::time::sleep(nap).await;
        let Some(pool) = pool.upgrade() else { return };
        let cur = pool.target.load(Ordering::Relaxed);
        let want = (cur * pool.cfg.max_tabs_per_browser).max(1) as u64;
        let done = pool.completed.load(Ordering::Relaxed) - done0;
        let elapsed = start.elapsed();
        if done < want {
            // 还没攒够一个窗口：按当前吞吐估还要多久；一个完成都没有时（空闲）按先验时长再看
            nap = if done > 0 {
                elapsed.mul_f64((want - done) as f64 / done as f64)
            } else {
                FIRST_WINDOW
            };
            if done == 0 {
                // 空闲：不是观测窗，重新开始计
                start = Instant::now();
                pool.state.lock().await.busy = Duration::ZERO;
                pool.waited.store(false, Ordering::Relaxed);
            }
            continue;
        }
        let busy = std::mem::take(&mut pool.state.lock().await.busy);
        let secs = elapsed.as_secs_f64();
        let n_bar = busy.as_secs_f64() / secs / pool.cfg.max_tabs_per_browser as f64;
        let x = done as f64 / secs;
        let saturated = pool.waited.swap(false, Ordering::Relaxed);
        let room = pool.cfg.max_browsers.saturating_sub(cur) as f64;
        let next = limit
            .window(n_bar, x, saturated, room)
            .clamp(1, pool.cfg.max_browsers);
        if next != cur {
            tracing::debug!(from = cur, to = next, n_bar, x, "browser pool resized");
            pool.target.store(next, Ordering::Relaxed);
            pool.notify.notify_waiters();
        }
        done0 += done;
        start = Instant::now();
        // 下一个窗口的预计时长：按刚测到的吞吐攒够一轮
        let want = (next * pool.cfg.max_tabs_per_browser).max(1) as f64;
        nap = if x > 0.0 {
            Duration::from_secs_f64(want / x)
        } else {
            FIRST_WINDOW
        };
    }
}

/// 后台健康检查。开了空闲关闭时，间隔取配置的间隔和学到的空闲上限的一半中较小的
/// （空闲的浏览器最多多留半个上限，同 0.81）。
pub(super) async fn health_loop(pool: Weak<PoolInner>) {
    loop {
        let interval = match pool.upgrade() {
            Some(p) => p.health_interval(),
            None => return,
        };
        tokio::time::sleep(interval).await;
        let Some(pool) = pool.upgrade() else { return };
        pool.health_check().await;
        // 顺带测一次浏览器的内存（读 /proc 放到阻塞线程里）
        let slots: Vec<(u32, usize)> = {
            let st = pool.state.lock().await;
            st.slots
                .iter()
                .filter(|s| !s.dead)
                .filter_map(|s| s.process.as_ref().map(|p| (p.pid(), s.in_use)))
                .collect()
        };
        if !slots.is_empty() && pool.cfg.min_available_memory_mb > 0 {
            let p = pool.clone();
            let _ = tokio::task::spawn_blocking(move || p.mem.sample(&slots)).await;
        }
    }
}

impl PoolInner {
    /// 启动一个浏览器，耗时记进启动上限的学习：成功 = 完成，超时 = 截尾，别的失败不计。
    pub(super) async fn launch_slot(&self, id: u64) -> Result<Slot> {
        let (prior, cap) = (self.launch.prior(), self.launch.cap());
        let started = Instant::now();
        let r = self.launch_slot_within(id, cap).await;
        match &r {
            Ok(_) => self.launch.done(started.elapsed(), prior),
            Err(Error::Launch(m)) if m.contains("timed out") => {
                self.launch.cut(started.elapsed(), prior)
            }
            Err(_) => {}
        }
        r
    }

    async fn launch_slot_within(&self, id: u64, timeout: Duration) -> Result<Slot> {
        let (conn, process) = match &self.source {
            Source::Remote(endpoint) => {
                let ws = launch::resolve_endpoint(endpoint).await?;
                (Connection::connect(&ws).await?, None)
            }
            Source::Local(spec) => {
                let spec = LaunchSpec {
                    timeout,
                    ..spec.clone()
                };
                let p = BrowserProcess::launch(&spec).await?;
                match Connection::connect(p.ws_url()).await {
                    Ok(c) => (c, Some(p)),
                    Err(e) => {
                        p.shutdown().await;
                        return Err(e);
                    }
                }
            }
        };
        let version =
            tokio::time::timeout(timeout, conn.call("Browser.getVersion", json!({}), None))
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
            pinned: 0,
            served: 0,
            retiring: false,
            dead: false,
            idle_since: Instant::now(),
            idle_cap: self.idle.prior(),
            idle_open: false,
        })
    }

    /// 浏览器从现在起空闲：取这段空闲的上限（学到的值，含探索），开始记这段空闲的样本。
    pub(super) fn open_idle(&self, s: &mut Slot) {
        s.idle_since = Instant::now();
        s.idle_cap = self.idle.cap();
        s.idle_open = !self.idle.prior().is_zero();
    }

    /// 健康检查的间隔：开了空闲关闭时不超过学到的空闲上限的一半。
    pub(super) fn health_interval(&self) -> Duration {
        let cfg = self.cfg.health_check_interval;
        if self.cfg.idle_timeout.is_zero() {
            return cfg;
        }
        let half = self.idle.prior().mul_f64(self.idle.fraction()) / 2;
        let interval = if cfg.is_zero() { half } else { cfg.min(half) };
        // 学到的上限可以收到 0（几乎没有空闲后被再用的）；间隔不短于先验的 δ 倍，免得空转
        interval.max(self.idle.prior().mul_f64(Delta::DEFAULT.get()))
    }

    /// 内存不够在等、又没有归还事件时隔多久再看：下一次归还的期望时刻 `W / 在用标签数`
    /// （`k` 个独立占用的剩余时长取最小，均值约 `W/k`；W 是占用时长的窗口均值）。没有 `W` 时用先验。
    fn memory_recheck(st: &PoolState) -> Duration {
        let in_use: usize = st.slots.iter().map(|s| s.in_use).sum();
        match st.service_mean {
            Some(w) if in_use > 0 => Duration::from_secs_f64(w / in_use as f64),
            _ => MEMORY_RECHECK_PRIOR,
        }
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

    /// 系统可用内存够不够再开 `need_mb`：开完之后至少还剩给系统其余部分的余量
    /// （实测，先验 `min_available_memory_mb`）。配置为 0 表示不检查（用户策略）。
    fn memory_allows(&self, need_mb: u64) -> bool {
        if self.cfg.min_available_memory_mb == 0 {
            return true;
        }
        let min = self.mem.reserve_mb();
        mem_available_mb().is_none_or(|avail| avail >= min + need_mb)
    }

    fn lease(self: &Arc<Self>, s: &mut Slot) -> Lease {
        let recycle = self.cfg.recycle_after_pages;
        if s.in_use == 0 && std::mem::take(&mut s.idle_open) {
            self.idle.done(s.idle_since.elapsed(), self.idle.prior());
        }
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
            started: Instant::now(),
        }
    }

    pub(super) async fn acquire(self: Arc<Self>) -> Result<Lease> {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let mut launch = None;
            let recheck;
            let (lease, finished) = {
                let mut st = self.state.lock().await;
                if st.closed {
                    return Err(Error::PoolClosed);
                }
                let finished = Self::take_idle_finished(&mut st);
                // 保留页面不算在渲染：只有它们时也算空闲，否则内存紧张时没有归还事件，新请求永远等不到
                let idle = st.launching == 0 && st.slots.iter().all(|s| s.in_use == s.pinned);
                let max_tabs = self.cfg.max_tabs_per_browser;
                // 被保留页面占满的浏览器、等保留页面结束才能回收的浏览器，都不再提供新标签，
                // 不计入目标数，否则新请求要等到这些页面被回收
                let serving = st
                    .slots
                    .iter()
                    .filter(|s| s.pinned < max_tabs && !(s.retiring && s.pinned > 0))
                    .count();
                // 每个浏览器、每个标签的内存：实测 PSS 的窗口均值（先验是 Footprint 表）
                let (browser_mb, tab_mb) = (self.mem.browser_mb(), self.mem.tab_mb());
                let idx = st
                    .slots
                    .iter_mut()
                    .position(|s| !s.retiring && s.in_use < max_tabs && s.alive());
                let lease = match idx {
                    Some(i) if idle || self.memory_allows(tab_mb) => {
                        Some(self.lease(&mut st.slots[i]))
                    }
                    None if serving + st.launching < self.target.load(Ordering::Relaxed)
                        && (idle || self.memory_allows(browser_mb + tab_mb)) =>
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
                recheck = Self::memory_recheck(&st);
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
            let _ = tokio::time::timeout(recheck, notified).await;
        }
    }

    async fn release(&self, slot_id: u64, held: Duration) {
        let finished = {
            let mut st = self.state.lock().await;
            self.completed.fetch_add(1, Ordering::Relaxed);
            st.busy += held;
            if let Some(w) = st.service.push(held.as_secs_f64(), Delta::DEFAULT) {
                st.service_mean = Some(w);
            }
            if let Some(s) = st.slots.iter_mut().find(|s| s.id == slot_id) {
                s.in_use -= 1;
                if s.in_use == 0 {
                    self.open_idle(s);
                }
            }
            Self::take_idle_finished(&mut st)
        };
        for s in finished {
            s.shutdown().await;
        }
        self.notify.notify_waiters();
    }

    /// 标签页转成保留页面：这次渲染照常计入吞吐和占用时长（之后的空闲不算负载），名额不还。
    pub(super) async fn pin(&self, slot_id: u64, held: Duration) {
        let mut st = self.state.lock().await;
        self.completed.fetch_add(1, Ordering::Relaxed);
        st.busy += held;
        if let Some(w) = st.service.push(held.as_secs_f64(), Delta::DEFAULT) {
            st.service_mean = Some(w);
        }
        if let Some(s) = st.slots.iter_mut().find(|s| s.id == slot_id) {
            s.pinned += 1;
        }
    }

    /// 保留页面结束，名额归还。
    pub(super) async fn unpin(&self, slot_id: u64) {
        let finished = {
            let mut st = self.state.lock().await;
            if let Some(s) = st.slots.iter_mut().find(|s| s.id == slot_id) {
                s.in_use -= 1;
                s.pinned -= 1;
                if s.in_use == 0 {
                    self.open_idle(s);
                }
            }
            Self::take_idle_finished(&mut st)
        };
        for s in finished {
            s.shutdown().await;
        }
        self.notify.notify_waiters();
    }

    pub(super) async fn health_check(&self) {
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
            if !self.cfg.idle_timeout.is_zero() {
                let mut keep = self.cfg.min_browsers;
                for s in st.slots.iter_mut().filter(|s| !s.dead && !s.retiring) {
                    if s.in_use == 0 && s.idle_since.elapsed() >= s.idle_cap && keep == 0 {
                        tracing::debug!(id = s.id, "closing idle browser");
                        if std::mem::take(&mut s.idle_open) {
                            self.idle.cut(s.idle_since.elapsed(), self.idle.prior());
                        }
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
                    Ok(mut slot) => {
                        self.open_idle(&mut slot);
                        st.slots.push(slot);
                    }
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

pub(super) struct Lease {
    pool: Arc<PoolInner>,
    slot_id: u64,
    pub(super) conn: Connection,
    pub(super) user_agent: String,
    pub(super) res: PageResources,
    done: bool,
    started: Instant,
}

impl Lease {
    /// 不关页面、不还名额，把标签页交给 [`KeptPage`]。
    pub(super) async fn into_kept(mut self, live: crate::page::LivePage) -> KeptPage {
        self.done = true;
        self.pool.pin(self.slot_id, self.started.elapsed()).await;
        KeptPage::new(
            self.pool.clone(),
            self.slot_id,
            self.conn.clone(),
            std::mem::take(&mut self.res),
            live,
        )
    }

    pub(super) async fn finish(mut self) {
        self.done = true;
        let _ = tokio::time::timeout(Duration::from_secs(5), self.res.cleanup(&self.conn)).await;
        self.pool
            .release(self.slot_id, self.started.elapsed())
            .await;
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        // render 的 future 被调用方丢弃时，在后台归还标签页。
        let (pool, id, conn) = (self.pool.clone(), self.slot_id, self.conn.clone());
        let held = self.started.elapsed();
        let mut res = std::mem::take(&mut self.res);
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(5), res.cleanup(&conn)).await;
                pool.release(id, held).await;
            });
        }
    }
}
