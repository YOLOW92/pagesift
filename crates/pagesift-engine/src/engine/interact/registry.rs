//! 会话表：scrapeId → 抓取的回放信息和保留的页面；空闲回收。
//!
//! **留多久**：上限是 Firecrawl 文档的默认值（空闲 5 分钟、总时长 10 分钟），实际空闲上限由池的内存准入推出：
//! - 内存放得下几个：`(可用 − 余量) / 每标签内存 + 已开的页面数`（已开页面的内存已经算在“用掉”里），
//!   再和标签名额（池的标签上限 − 1，至少给渲染留一个）取小，见 [`session_cap`]；
//! - 占得越满留得越短：`空闲上限 = 先验 × (1 − (已开页面数 − 1) / 放得下的数)`，见 [`idle_limit`]。
//!   只开一个时等于先验；刚好放满时还有 `先验 / 放得下的数`；超出（别的进程吃掉了内存）时是 0，
//!   从最久没用的开始关，关到放得下为止。可用内存、余量、每标签内存都是池实测的同一组数。
//!
//! **记录表**：没有保留页面的记录（回放信息、已关闭的说明）按字节数限制，总量不超过多开一个
//! 标签页的内存，满了淘汰最早的；开着页面的记录不淘汰（它们受上面的上限约束）。

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use pagesift_api::types::{ScrapeOptions, instance_tag, issued_elsewhere, scrape_id, session_gone};
use pagesift_api::{ApiError, ErrorCode};
use pagesift_cdp::{BrowserPool, KeptPage, SessionRoom};
use pagesift_policy::RobotsMode;
use tokio::task::JoinHandle;

/// 保留会话的时长上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractLimits {
    /// 内存宽裕时的空闲上限。
    pub idle: Duration,
    /// 一个会话从打开起的总时长上限。
    pub ttl: Duration,
}

impl Default for InteractLimits {
    fn default() -> Self {
        Self {
            // [协议] Firecrawl 文档 features/interact#session-lifecycle：inactivity timeout 默认 5 分钟
            idle: Duration::from_secs(5 * 60),
            // [协议] 同上：TTL 默认 10 分钟
            ttl: Duration::from_secs(10 * 60),
        }
    }
}

/// 这台机器现在放得下多少个保留页面（含已开的 `open` 个）。可以小于 `open`：内存被别的进程吃掉了。
pub(super) fn session_cap(room: &SessionRoom, open: usize) -> usize {
    let tabs = room.max_tabs.saturating_sub(1);
    let Some(available) = room.available_mb else {
        return tabs;
    };
    let spare = available as i64 - room.reserve_mb as i64;
    let tab = room.tab_mb.max(1) as i64;
    // 向下取整：差一点放不下就是放不下
    let fit = spare.div_euclid(tab) + open as i64;
    (fit.max(0) as usize).min(tabs)
}

/// 已开 `open` 个、放得下 `cap` 个时，一个页面能空闲多久。
pub(super) fn idle_limit(prior: Duration, open: usize, cap: usize) -> Duration {
    if cap == 0 || open > cap {
        return Duration::ZERO;
    }
    // 即 1 − (open − 1) / cap；写成一个分数，免得 1 减小数带来的舍入
    prior.mul_f64((cap + 1 - open.max(1)) as f64 / cap as f64)
}

/// `idle` 按空闲时长从长到短排好：前多少个该关。`others` 是这之外还占名额的数（正在用的、要新开的）。
pub(super) fn reclaim_count(
    idle: &[Duration],
    others: usize,
    prior: Duration,
    cap: usize,
) -> usize {
    let mut open = idle.len() + others;
    let mut n = 0;
    for d in idle {
        if *d < idle_limit(prior, open, cap) {
            break;
        }
        n += 1;
        open -= 1;
    }
    n
}

pub(super) struct OpenPage {
    pub page: KeptPage,
    pub pool: Arc<BrowserPool>,
    pub opened: Instant,
    pub last_used: Instant,
    /// 当前页面的网址：下一次动作前查 robots、动作后看有没有跳走。
    pub url: String,
    /// 这个会话用过的 `{{secret:NAME}}`：它们的值不进模型，也不进响应。
    pub secrets: BTreeSet<String>,
}

pub(super) enum PageState {
    /// 抓取时没有要求保留：第一次 interact 时重新打开。
    NotOpened,
    Open(Box<OpenPage>),
    /// 已经结束，附原因。
    Gone(String),
}

pub(super) struct Record {
    pub owner: Option<String>,
    pub url: String,
    pub opts: ScrapeOptions,
    pub mode: RobotsMode,
    pub state: tokio::sync::Mutex<PageState>,
    /// 记录占的字节数（估计）。
    bytes: usize,
}

#[derive(Default)]
struct Table {
    map: HashMap<String, Arc<Record>>,
    /// 可以淘汰的记录，最早的在前；可能有已经开了页面或已被淘汰的旧项，取出时再看。
    order: VecDeque<String>,
    open: HashSet<String>,
    bytes: usize,
}

impl Table {
    fn evict(&mut self, max_bytes: usize) {
        while self.bytes > max_bytes {
            let Some(id) = self.order.pop_front() else {
                return;
            };
            // 开着页面的不淘汰：关闭时重新排队
            if self.open.contains(&id) {
                continue;
            }
            if let Some(r) = self.map.remove(&id) {
                self.bytes -= r.bytes;
            }
        }
    }
}

pub(crate) struct Sessions {
    tag: String,
    pub limits: InteractLimits,
    max_record_bytes: usize,
    table: Mutex<Table>,
    reaper: Mutex<Option<JoinHandle<()>>>,
}

impl Sessions {
    pub(crate) fn new(limits: InteractLimits, max_record_bytes: usize) -> Self {
        Self {
            tag: instance_tag(),
            limits,
            max_record_bytes,
            table: Mutex::default(),
            reaper: Mutex::new(None),
        }
    }

    fn table(&self) -> std::sync::MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 新记录，返回 scrapeId。`state` 是 [`PageState::Open`] 时同时登记成开着的。
    pub(super) fn insert(
        &self,
        id: Option<&str>,
        owner: Option<&str>,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
        state: PageState,
    ) -> String {
        let id = id.map_or_else(|| scrape_id(&self.tag), str::to_string);
        let opened = matches!(state, PageState::Open(_));
        // 选项里可能带调用方给的 schema，按序列化后的长度算；再算上键和记录本身
        let bytes = serde_json::to_string(opts).map_or(0, |s| s.len())
            + url.len()
            + id.len()
            + std::mem::size_of::<Record>();
        let record = Arc::new(Record {
            owner: owner.map(str::to_string),
            url: url.to_string(),
            opts: opts.clone(),
            mode,
            state: tokio::sync::Mutex::new(state),
            bytes,
        });
        let mut t = self.table();
        if let Some(old) = t.map.insert(id.clone(), record) {
            t.bytes -= old.bytes;
        }
        t.bytes += bytes;
        t.order.push_back(id.clone());
        if opened {
            t.open.insert(id.clone());
        }
        t.evict(self.max_record_bytes);
        id
    }

    /// 查记录。查不到时分清“上一个进程发的”和“没有这个 id”。
    pub(super) fn get(&self, id: &str, owner: Option<&str>) -> Result<Arc<Record>, ApiError> {
        let found = self.table().map.get(id).cloned();
        let Some(r) = found else {
            if issued_elsewhere(id, &self.tag) {
                return Err(session_gone(format!(
                    "scrape {id} was not issued by this server process: the server restarted or the id belongs to another machine, so its browser session is gone; run the scrape again"
                )));
            }
            return Err(ApiError::new(
                ErrorCode::NotFound,
                format!("no scrape {id}: it never existed or its record has expired"),
            ));
        };
        if r.owner.as_deref() != owner {
            return Err(ApiError::new(
                ErrorCode::Forbidden,
                format!("scrape {id} belongs to a different account"),
            ));
        }
        Ok(r)
    }

    /// 记录从“没开页面”变成开着。
    pub(super) fn mark_open(&self, id: &str) {
        self.table().open.insert(id.to_string());
    }

    /// 页面关了：记录重新变成可淘汰的。
    pub(super) fn mark_closed(&self, id: &str) {
        let mut t = self.table();
        if t.open.remove(id) {
            t.order.push_back(id.to_string());
        }
        t.evict(self.max_record_bytes);
    }

    pub(super) fn open_count(&self) -> usize {
        self.table().open.len()
    }

    fn open_records(&self) -> Vec<(String, Arc<Record>)> {
        let t = self.table();
        t.open
            .iter()
            .filter_map(|id| t.map.get(id).map(|r| (id.clone(), r.clone())))
            .collect()
    }

    /// 关一个页面，记下原因。`expect_used`：决定回收之后这个会话又被用过，就不关了。
    async fn close_record(
        &self,
        id: &str,
        record: &Record,
        why: String,
        expect_used: Option<Instant>,
    ) {
        let Ok(mut state) = record.state.try_lock() else {
            return;
        };
        if let (Some(used), PageState::Open(o)) = (expect_used, &*state)
            && o.last_used != used
        {
            return;
        }
        let old = std::mem::replace(&mut *state, PageState::Gone(why));
        drop(state);
        if let PageState::Open(o) = old {
            tracing::debug!(id, "closing interact session");
            o.page.close().await;
            self.mark_closed(id);
        }
    }

    /// 回收：死掉的、超过总时长的、按内存该关的。`extra` 是马上要新开的页面数。
    /// 返回算出来放得下多少个（`room` 为 `None` 且没有开着的页面时不用算，返回 0）。
    pub(super) async fn sweep(&self, room: Option<SessionRoom>, extra: usize) -> usize {
        let mut idle = Vec::new();
        let mut busy = 0;
        let mut gone = Vec::new();
        let mut room = room;
        for (id, r) in self.open_records() {
            let Ok(state) = r.state.try_lock() else {
                busy += 1;
                continue;
            };
            let PageState::Open(o) = &*state else {
                continue;
            };
            room.get_or_insert_with(|| o.pool.session_room());
            if !o.page.is_alive() {
                gone.push((id.clone(), r.clone(), "the browser exited".to_string()));
            } else if o.opened.elapsed() >= self.limits.ttl {
                gone.push((
                    id.clone(),
                    r.clone(),
                    format!(
                        "it reached the session limit of {} s",
                        self.limits.ttl.as_secs()
                    ),
                ));
            } else {
                idle.push((id.clone(), r.clone(), o.last_used));
            }
        }
        for (id, r, why) in gone {
            self.close_record(&id, &r, why, None).await;
        }
        let Some(room) = room else {
            return 0;
        };
        idle.sort_by_key(|(_, _, used)| *used);
        let now = Instant::now();
        let waits: Vec<Duration> = idle.iter().map(|(_, _, u)| now - *u).collect();
        let open = idle.len() + busy;
        let cap = session_cap(&room, open);
        let n = reclaim_count(&waits, busy + extra, self.limits.idle, cap);
        for (id, r, used) in idle.into_iter().take(n) {
            let why = format!(
                "it was idle for {} s; {open} sessions were open and memory admits {cap}",
                (now - used).as_secs()
            );
            self.close_record(&id, &r, why, Some(used)).await;
        }
        cap
    }

    /// 后台回收任务：第一个页面开起来时启动，间隔是空闲上限先验的 δ 倍（和池的健康检查的下限同一个式子）。
    pub(super) fn start_reaper(self: &Arc<Self>) {
        let mut slot = self.reaper.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return;
        }
        let weak: Weak<Self> = Arc::downgrade(self);
        let every = self
            .limits
            .idle
            .mul_f64(pagesift_stat::Delta::DEFAULT.get());
        *slot = Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let Some(s) = weak.upgrade() else { return };
                s.sweep(None, 0).await;
            }
        }));
    }

    /// 停掉回收任务，关掉全部页面（服务退出时）。
    pub(crate) async fn shutdown(&self) {
        if let Some(h) = self.reaper.lock().unwrap_or_else(|e| e.into_inner()).take() {
            h.abort();
        }
        for (id, r) in self.open_records() {
            let old = std::mem::replace(
                &mut *r.state.lock().await,
                PageState::Gone("the server shut down".into()),
            );
            if let PageState::Open(o) = old {
                o.page.close().await;
            }
            self.mark_closed(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIOR: Duration = Duration::from_secs(300);

    /// `Record` 带着页面状态，没有 `Debug`：只看错误码。
    fn code(r: Result<Arc<Record>, ApiError>) -> Option<ErrorCode> {
        r.err().map(|e| e.code)
    }

    fn room(available: Option<u64>, max_tabs: usize) -> SessionRoom {
        SessionRoom {
            available_mb: available,
            reserve_mb: 1024,
            tab_mb: 200,
            max_tabs,
        }
    }

    #[test]
    fn capacity_comes_from_memory_and_keeps_a_tab_for_rendering() {
        // 可用 3024 − 余量 1024 = 2000 MB，再放 10 个；已开 2 个时一共放得下 12 个
        assert_eq!(session_cap(&room(Some(3024), 64), 2), 12);
        // 标签名额更紧：留一个给渲染
        assert_eq!(session_cap(&room(Some(3024), 4), 2), 3);
        // 读不到内存：只看标签名额
        assert_eq!(session_cap(&room(None, 8), 0), 7);
        // 可用内存比余量少 500 MB：要关掉 3 个才够（向下取整），3 个里一个都放不下
        assert_eq!(session_cap(&room(Some(524), 64), 3), 0);
        assert_eq!(session_cap(&room(Some(524), 64), 5), 2);
    }

    #[test]
    fn the_fuller_the_shorter_the_idle_limit() {
        assert_eq!(idle_limit(PRIOR, 1, 10), PRIOR);
        assert_eq!(idle_limit(PRIOR, 1, 1), PRIOR);
        assert_eq!(idle_limit(PRIOR, 6, 10), PRIOR / 2);
        assert_eq!(idle_limit(PRIOR, 10, 10), PRIOR / 10);
        assert_eq!(idle_limit(PRIOR, 11, 10), Duration::ZERO);
        assert_eq!(idle_limit(PRIOR, 1, 0), Duration::ZERO);
    }

    #[test]
    fn reclaims_the_oldest_until_it_fits() {
        let s = |n| Duration::from_secs(n);
        // 放得下：都没到上限，不关
        assert_eq!(reclaim_count(&[s(10), s(5)], 0, PRIOR, 10), 0);
        // 只开一个、空闲超过先验：关
        assert_eq!(reclaim_count(&[s(301)], 0, PRIOR, 10), 1);
        // 刚好放满：上限是先验的 1/3，最久的 200 s 已超过，关掉后上限变长，下一个留下
        assert_eq!(reclaim_count(&[s(200), s(60), s(1)], 0, PRIOR, 3), 1);
        // 要新开一个、已经放满：只关腾出一个位置需要的数量（刚用过的也算）
        assert_eq!(reclaim_count(&[s(2), s(1)], 1, PRIOR, 2), 1);
        // 正在用的不在列表里，但占名额：放不下时关空闲的，关完还放不下就没有可关的了
        assert_eq!(reclaim_count(&[s(1)], 3, PRIOR, 2), 1);
    }

    #[test]
    fn records_are_bounded_by_bytes_and_tell_ids_apart() {
        let s = Sessions::new(InteractLimits::default(), 4096);
        let opts = ScrapeOptions::default();
        let first = s.insert(
            None,
            None,
            "https://a.test/",
            &opts,
            RobotsMode::Respect,
            PageState::NotOpened,
        );
        assert!(s.get(&first, None).is_ok());
        assert_eq!(
            code(s.get(&first, Some("other"))),
            Some(ErrorCode::Forbidden)
        );
        assert_eq!(code(s.get("nope", None)), Some(ErrorCode::NotFound));
        let elsewhere = scrape_id(&instance_tag());
        assert_eq!(code(s.get(&elsewhere, None)), Some(ErrorCode::SessionGone));
        for _ in 0..64 {
            s.insert(
                None,
                None,
                "https://a.test/",
                &opts,
                RobotsMode::Respect,
                PageState::NotOpened,
            );
        }
        assert!(s.table().bytes <= 4096);
        // 最早的被淘汰：这个进程发的 id，查不到就是过期了
        assert_eq!(code(s.get(&first, None)), Some(ErrorCode::NotFound));
    }
}
