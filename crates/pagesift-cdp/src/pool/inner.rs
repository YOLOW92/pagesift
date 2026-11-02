//! 池的内部调度：借还标签页、启动和回收浏览器、健康检查、按吞吐自适应调整浏览器数。

use super::*;

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
pub(super) async fn adapt_loop(pool: Weak<PoolInner>) {
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

pub(super) async fn health_loop(pool: Weak<PoolInner>, interval: Duration) {
    let mut tick = tokio::time::interval(interval);
    tick.tick().await;
    loop {
        tick.tick().await;
        let Some(pool) = pool.upgrade() else { return };
        pool.health_check().await;
    }
}

impl PoolInner {
    pub(super) async fn launch_slot(&self, id: u64) -> Result<Slot> {
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

    pub(super) async fn acquire(self: Arc<Self>) -> Result<Lease> {
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

pub(super) struct Lease {
    pool: Arc<PoolInner>,
    slot_id: u64,
    pub(super) conn: Connection,
    pub(super) user_agent: String,
    pub(super) res: PageResources,
    done: bool,
}

impl Lease {
    pub(super) async fn finish(mut self) {
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
