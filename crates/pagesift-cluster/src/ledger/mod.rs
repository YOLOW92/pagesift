//! 账本：任务、租约、栅栏、成员表、统计。
//!
//! [`LocalCoordinator`](crate::LocalCoordinator) 与 [`GatewayCoordinator`](crate::GatewayCoordinator)
//! 共用同一个账本，差别只在结果怎么交付、调用来自进程内还是帧协议。
//!
//! 状态按站点分片（分片数随核数），每个分片一把锁，热路径上不碰全局锁（192 vCPU 的经验）。
//! 任务编号的低 [`SLOT_BITS`] 位是站点槽位，所以只凭编号就能找到分片，分片数变化后仍然一致。
//! 每个改变状态的操作先写日志（[`crate::journal`]）再应用，日志里每条记录都带任期。

mod app;
mod cluster;
mod drr;
mod duration;
mod quota;
mod shard;
pub(crate) mod snapshot;

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pagesift_stat::Delta;
use tokio::sync::Notify;

pub use app::AppState;
use drr::{Acct, Registry};
use duration::Durations;
pub use duration::renew_after;
pub(crate) use duration::{upper, upper_or_max};
use quota::QuotaShard;
pub use quota::{AccountLimits, Hold, Refusal};
use shard::Shard;
pub use shard::{Commit, Expired};

use crate::clock::{Clock, ms};
use crate::hrw::HostMap;
use crate::journal::{Entry, Journal, Record};
use crate::stats::StatsHub;
use crate::types::{
    Deadline, Error, Fence, Lease, Owner, Renewal, Resources, SiteKey, Task, TaskId, WorkerId, lock,
};
use pagesift_stat::rng::splitmix64;

/// [推导] 任务编号低 16 位存站点槽位，分片数上限 2^16（远大于单机核数）；高 48 位是分片内序号
pub(crate) const SLOT_BITS: u32 = 16;
const SLOT_MASK: u64 = (1 << SLOT_BITS) - 1;

/// 每个分片独占缓存行：x86 的相邻行预取会把两条 64 字节缓存行成对取入，按 128 字节对齐才真正独占
/// （0.70 在 192 vCPU 上的实测经验，docs/history/0.70_p2_step4_3_numa_dispatch.md）。
#[repr(align(128))]
struct Slot {
    shard: Mutex<Shard>,
    /// 分片里排队的任务数（锁内更新，锁外只读），拉取时跳过空分片不必加锁。
    queued: AtomicU64,
    /// 分片里最早的租约截止时刻，`u64::MAX` 表示没有。
    next_deadline: AtomicU64,
    /// 按账户分到这个槽位的配额状态（与任务分片无关，只共用槽位规则）。
    quota: Mutex<QuotaShard>,
}

/// 账本的配置。
pub struct LedgerConfig {
    pub delta: Delta,
    pub clock: Arc<dyn Clock>,
    pub journal: Option<Journal>,
    /// 分片数；0 表示按可用并行度取（向上取 2 的幂）。
    pub shards: usize,
    /// 单主机：所有站点归本进程。
    pub local: bool,
}

impl LedgerConfig {
    pub fn new(clock: Arc<dyn Clock>, local: bool) -> Self {
        Self {
            delta: Delta::DEFAULT,
            clock,
            journal: None,
            shards: 0,
            local,
        }
    }

    pub fn with_journal(mut self, j: Journal) -> Self {
        self.journal = Some(j);
        self
    }
}

/// 一次到期检查的结果。
#[derive(Debug, Default)]
pub struct Tick {
    pub expired: Vec<Expired>,
    /// 下一次需要检查的时刻。
    pub next: Option<Deadline>,
}

#[derive(Default)]
struct Members {
    weights: BTreeMap<WorkerId, f64>,
    rtt: HashMap<WorkerId, Duration>,
    /// 工人最近一次报告的准入容量（拉取时上报）；只算在成员表里的工人。
    capacity: HashMap<WorkerId, f64>,
    map: Arc<HostMap>,
    changes: u64,
}

pub struct Ledger {
    slots: Box<[Slot]>,
    mask: u64,
    term: AtomicU64,
    journal: Option<Journal>,
    clock: Arc<dyn Clock>,
    delta: Delta,
    durations: Durations,
    members: Mutex<Members>,
    stats: Mutex<StatsHub>,
    local: bool,
    /// 温备：只回放主网关发来的日志，不接受在线写入。
    follower: AtomicBool,
    /// 计时器当前等到的时刻；新租约的截止更早时唤醒它。
    scheduled: AtomicU64,
    wake: Notify,
    /// 账户与权重的全局登记（轮转本身在各线程上，见 `drr`）；`generation` 随登记变化加一。
    registry: Mutex<Registry>,
    generation: AtomicU64,
    /// 登记过的账户数；为 0 时拉取走不带账户的无锁路径。
    accounts: AtomicUsize,
    /// 进程内唯一的账本编号（线程局部的轮转状态按它区分账本）。
    id: u64,
    /// 还没应用过任何状态记录（快照只在这时载入）。
    fresh: AtomicBool,
    /// 回放到快照中间时：0 不在快照里，1 正在载入，2 跳过（状态已经是最新）。
    snap_mode: AtomicU8,
    snap_begin: AtomicU64,
    snapshotting: AtomicBool,
    /// 上层状态（API 任务记录）：随日志复制到温备，随快照一起压缩。
    app: std::sync::OnceLock<Arc<dyn AppState>>,
    /// 回放上层写入失败的次数（见 [`Ledger::app_failures`]）。
    app_failures: AtomicU64,
}

static LEDGER_IDS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static HINT: Cell<usize> = const { Cell::new(0) };
}

impl Ledger {
    fn build(cfg: LedgerConfig) -> Self {
        let n = if cfg.shards == 0 {
            std::thread::available_parallelism().map_or(1, |n| n.get())
        } else {
            cfg.shards
        }
        .next_power_of_two()
        .min(1 << SLOT_BITS);
        let map = Arc::new(HostMap::default());
        let slots = (0..n)
            .map(|_| Slot {
                shard: Mutex::new(Shard::new(cfg.local, Arc::clone(&map))),
                queued: AtomicU64::new(0),
                next_deadline: AtomicU64::new(u64::MAX),
                quota: Mutex::default(),
            })
            .collect();
        Self {
            slots,
            mask: n as u64 - 1,
            term: AtomicU64::new(0),
            journal: cfg.journal,
            clock: cfg.clock,
            delta: cfg.delta,
            durations: Durations::default(),
            members: Mutex::new(Members::default()),
            stats: Mutex::new(StatsHub::default()),
            local: cfg.local,
            follower: AtomicBool::new(false),
            scheduled: AtomicU64::new(u64::MAX),
            wake: Notify::new(),
            registry: Mutex::default(),
            generation: AtomicU64::new(0),
            accounts: AtomicUsize::new(0),
            id: LEDGER_IDS.fetch_add(1, Ordering::Relaxed),
            fresh: AtomicBool::new(true),
            snap_mode: AtomicU8::new(0),
            snap_begin: AtomicU64::new(0),
            snapshotting: AtomicBool::new(false),
            app: std::sync::OnceLock::new(),
            app_failures: AtomicU64::new(0),
        }
    }

    /// 打开账本：回放已有日志，然后以新任期开始（单主机重启、网关启动）。
    pub fn open(cfg: LedgerConfig) -> Result<Self, Error> {
        let l = Self::build(cfg);
        l.replay()?;
        l.start_term()?;
        Ok(l)
    }

    /// 以温备身份打开：回放本地日志，之后只接受 [`apply_shipped`](Self::apply_shipped)。
    pub fn follower(cfg: LedgerConfig) -> Result<Self, Error> {
        let l = Self::build(cfg);
        l.replay()?;
        l.follower.store(true, Ordering::SeqCst);
        Ok(l)
    }

    /// 新任期：任期加一并写日志；未完成的租约按新时钟重新计时。
    fn start_term(&self) -> Result<u64, Error> {
        let term = self.term.load(Ordering::SeqCst) + 1;
        self.log(term, &Record::Term {})?;
        self.term.store(term, Ordering::SeqCst);
        let now = self.clock.now_ms();
        for slot in self.slots.iter() {
            let mut s = lock(&slot.shard);
            s.restart_leases(now, &self.durations, self.delta);
            self.publish(slot, &s);
        }
        self.close_orphan_holds(term)?;
        Ok(term)
    }

    /// 温备接管：停止回放，以 `term+1` 成为主网关。旧任期的写入此后一律被栅栏拒绝。
    pub fn take_over(&self) -> Result<u64, Error> {
        self.follower.store(false, Ordering::SeqCst);
        self.start_term()
    }

    pub fn term(&self) -> u64 {
        self.term.load(Ordering::SeqCst)
    }

    pub fn is_follower(&self) -> bool {
        self.follower.load(Ordering::SeqCst)
    }

    pub fn is_local(&self) -> bool {
        self.local
    }

    pub fn delta(&self) -> Delta {
        self.delta
    }

    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    pub fn journal(&self) -> Option<&Journal> {
        self.journal.as_ref()
    }

    fn slot_of_site(&self, site: SiteKey) -> (u64, &Slot) {
        let slot = splitmix64(site.0) & SLOT_MASK;
        (slot, &self.slots[(slot & self.mask) as usize])
    }

    fn slot_of_id(&self, id: TaskId) -> &Slot {
        &self.slots[(id.0 & SLOT_MASK & self.mask) as usize]
    }

    fn slot_of_record(&self, rec: &Record) -> Option<&Slot> {
        match rec {
            Record::Submit { task, .. } => Some(self.slot_of_site(task.site).1),
            Record::Grant { id, .. } | Record::Done { id, .. } | Record::Cancel { id, .. } => {
                Some(self.slot_of_id(*id))
            }
            _ => None,
        }
    }

    fn publish(&self, slot: &Slot, s: &Shard) {
        slot.queued.store(s.queued, Ordering::Release);
        let next = s.next_deadline().unwrap_or(u64::MAX);
        slot.next_deadline.store(next, Ordering::Release);
        if next < self.scheduled.load(Ordering::Acquire) {
            self.wake.notify_one();
        }
    }

    /// 计时器：在最早的租约截止时刻调用 [`tick`](Self::tick)，把过期的交给 `on_expired`。
    /// 用真实时钟时才需要；测试用手动时钟直接调用 `tick`。
    pub async fn run_ticker(self: Arc<Self>, on_expired: impl Fn(&[Expired]) + Send) {
        loop {
            let notified = self.wake.notified();
            let t = self.tick();
            if !t.expired.is_empty() {
                on_expired(&t.expired);
            }
            let target = t.next.map_or(u64::MAX, |d| d.0);
            self.scheduled.store(target, Ordering::Release);
            if target == u64::MAX {
                notified.await;
                continue;
            }
            let wait = Duration::from_millis(target.saturating_sub(self.clock.now_ms()));
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = notified => {}
            }
        }
    }

    /// 回放一条记录（恢复、温备）。租约截止时刻在接管时统一重算。
    fn apply_entry(&self, e: &Entry) {
        if self.apply_snapshot_record(&e.rec) {
            return;
        }
        if let Record::App { op } = &e.rec {
            self.apply_app(op);
            return;
        }
        if !matches!(e.rec, Record::Term {}) {
            self.fresh.store(false, Ordering::Release);
        }
        match &e.rec {
            Record::Stats { origin, delta } => {
                lock(&self.stats).apply(origin, delta);
                return;
            }
            Record::Reserve { account, .. }
            | Record::Settle { account, .. }
            | Record::Release { account, .. }
            | Record::Request { account, .. } => {
                return lock(&self.quota_slot(account).quota).apply(&e.rec);
            }
            Record::Submit {
                account: Some(a), ..
            } => self.add_account(&Some(a.clone())),
            _ => {}
        }
        if let Some(slot) = self.slot_of_record(&e.rec) {
            let mut s = lock(&slot.shard);
            s.apply(&e.rec, self.clock.now_ms(), Duration::ZERO);
            self.publish(slot, &s);
        }
    }

    fn writable(&self) -> Result<u64, Error> {
        if self.is_follower() {
            return Err(Error::Rejected("standby does not accept writes".into()));
        }
        Ok(self.term())
    }

    fn log(&self, term: u64, rec: &Record) -> Result<(), Error> {
        if let Some(j) = &self.journal {
            j.append(term, rec)?;
        }
        Ok(())
    }

    /// 租约时长：该类任务耗时的 `q(1−δ)` 上界（冷启动为任务的 `timeout`）+ 往返上界。
    fn ttl(&self, s: &mut Shard, w: &Task, rtt_hi: Duration) -> Duration {
        s.durations
            .task_hi(w.class, w.timeout, &self.durations, self.delta)
            + rtt_hi
    }

    pub fn submit(&self, task: Task) -> Result<TaskId, Error> {
        self.submit_as(task, None)
    }

    /// 提交一个属于 `account` 的任务：参与账户间的加权公平调度，分配时自动冻结一页配额。
    pub fn submit_as(&self, task: Task, account: Option<&str>) -> Result<TaskId, Error> {
        let r = self.submit_inner(task, account);
        self.maybe_snapshot();
        r
    }

    fn submit_inner(&self, task: Task, account: Option<&str>) -> Result<TaskId, Error> {
        let term = self.writable()?;
        let account: Acct = account.map(Into::into);
        self.add_account(&account);
        let (slot_bits, slot) = self.slot_of_site(task.site);
        let mut s = lock(&slot.shard);
        let id = TaskId(((s.next_seq + 1) << SLOT_BITS) | slot_bits);
        let rec = Record::Submit { id, task, account };
        self.log(term, &rec)?;
        s.apply(&rec, self.clock.now_ms(), Duration::ZERO);
        self.publish(slot, &s);
        Ok(id)
    }

    /// 给 `worker`（单主机为 `None`）挑一个任务并发出租约；没有可做的立即返回 `None`。
    /// `rtt_hi` 是该工人往返时间的 `q(1−δ)` 上界，进程内为 0。
    pub fn pull(
        &self,
        worker: Option<&WorkerId>,
        free: &Resources,
        rtt_hi: Duration,
    ) -> Result<Option<Lease>, Error> {
        let term = self.writable()?;
        let owner = match (self.local, worker) {
            (true, _) => Owner::Local,
            (false, Some(w)) => Owner::Worker(w.clone()),
            (false, None) => return Ok(None),
        };
        let r = if self.accounts.load(Ordering::Acquire) == 0 {
            self.pull_for(term, worker, &owner, &None, free, rtt_hi)
        } else {
            self.pull_fair(term, worker, &owner, free, rtt_hi)
        };
        self.maybe_snapshot();
        r
    }

    fn pull_for(
        &self,
        term: u64,
        worker: Option<&WorkerId>,
        owner: &Owner,
        acct: &Acct,
        free: &Resources,
        rtt_hi: Duration,
    ) -> Result<Option<Lease>, Error> {
        if acct.as_ref().is_some_and(|a| self.balance(a) == Some(0)) {
            return Ok(None);
        }
        let n = self.slots.len();
        let start = HINT.with(|h| {
            let v = h.get();
            h.set(v.wrapping_add(1));
            v
        });
        for k in 0..n {
            let slot = &self.slots[(start + k) % n];
            if slot.queued.load(Ordering::Acquire) == 0 {
                continue;
            }
            let mut s = lock(&slot.shard);
            let now = self.clock.now_ms();
            let Some(id) = s.pick(owner, acct, free, now) else {
                continue;
            };
            let work = s.work(id).expect("picked task exists");
            let last = s.last_fence(id).unwrap_or_default();
            let fence = Fence {
                term,
                epoch: last.epoch + 1,
            };
            let ttl = self.ttl(&mut s, &work, rtt_hi);
            let rec = Record::Grant {
                id,
                fence,
                worker: worker.cloned(),
            };
            let held = acct
                .as_ref()
                .map_or(Ok(true), |a| self.hold_task(term, a, id));
            let logged = match held {
                Ok(true) => self.log(term, &rec).map(|()| true),
                other => other,
            };
            if !matches!(logged, Ok(true)) {
                // 撤销这次拣选：放回队头，排队计数没变（配额刚好被别处用完时也一样）
                s.enqueue(id, true);
                return logged.map(|_| None);
            }
            s.apply(&rec, now, ttl);
            let handoff = s.take_handoff(work.site);
            self.publish(slot, &s);
            return Ok(Some(Lease {
                task: id,
                epoch: fence.epoch,
                term: fence.term,
                expires: Deadline(now.saturating_add(ms(ttl))),
                received: Deadline(now),
                ttl,
                renew_after: renew_after(ttl, rtt_hi),
                work,
                handoff,
            }));
        }
        Ok(None)
    }

    /// 续约；任务已取消时返回 [`Renewal::Cancel`]。
    pub fn renew(
        &self,
        id: TaskId,
        fence: Fence,
        worker: Option<&WorkerId>,
        rtt_hi: Duration,
    ) -> Result<Renewal, Error> {
        self.writable()?;
        let slot = self.slot_of_id(id);
        let mut s = lock(&slot.shard);
        let ttl = match s.work(id) {
            Some(w) => self.ttl(&mut s, &w, rtt_hi),
            None => Duration::ZERO,
        };
        let r = s.renew(id, fence, worker, self.clock.now_ms(), ttl);
        self.publish(slot, &s);
        r
    }

    /// 提交结果。`result` 是要与完成标记写进同一条日志记录的已编码结果（单主机为 `None`），
    /// 写完日志后原样交回给 `deliver`，不多拷贝；
    /// `ran` 是执行耗时（工人上报），`None` 时按分配到提交的时间算。
    ///
    /// `deliver` 只在第一次提交时调用，把结果交给出口：结果写进日志时（网关）在写日志之后调用，
    /// 结果已经持久；不写进日志时（单主机，不序列化结果）在写完成标记之前调用，崩溃时宁可重做
    /// 一次（出口按任务编号去重）也不丢结果。
    pub fn commit(
        &self,
        id: TaskId,
        fence: Fence,
        result: Option<Vec<u8>>,
        ran: Option<Duration>,
        deliver: impl FnOnce(Option<Vec<u8>>),
    ) -> Result<Commit, Error> {
        self.commit_by(None, id, fence, result, ran, deliver)
    }

    /// 同 [`commit`](Self::commit)，`by` 是提交的工人：租约在别的工人手里时拒绝（网关收到的提交）。
    pub fn commit_by(
        &self,
        by: Option<&WorkerId>,
        id: TaskId,
        fence: Fence,
        result: Option<Vec<u8>>,
        ran: Option<Duration>,
        deliver: impl FnOnce(Option<Vec<u8>>),
    ) -> Result<Commit, Error> {
        let r = self.commit_inner(by, id, fence, result, ran, deliver);
        self.maybe_snapshot();
        r
    }

    fn commit_inner(
        &self,
        by: Option<&WorkerId>,
        id: TaskId,
        fence: Fence,
        result: Option<Vec<u8>>,
        ran: Option<Duration>,
        deliver: impl FnOnce(Option<Vec<u8>>),
    ) -> Result<Commit, Error> {
        let term = self.writable()?;
        let slot = self.slot_of_id(id);
        let mut s = lock(&slot.shard);
        if s.check_commit(id, fence, by)? == Commit::Duplicate {
            return Ok(Commit::Duplicate);
        }
        let now = self.clock.now_ms();
        let work = s.work(id).ok_or(Error::UnknownTask)?;
        let took = ran.or_else(|| s.elapsed(id, now));
        let mut deliver = Some(deliver);
        if result.is_none()
            && let Some(f) = deliver.take()
        {
            f(None);
        }
        let account = s.account(id);
        let rec = Record::Done { id, fence, result };
        self.log(term, &rec)?;
        s.apply(&rec, now, Duration::ZERO);
        self.finish_task(term, &account, id, true)?;
        if let (Some(f), Record::Done { result, .. }) = (deliver, rec) {
            f(result);
        }
        if let Some(d) = took {
            s.durations
                .observe(work.class, d.as_secs_f64(), &self.durations, self.delta);
        }
        self.publish(slot, &s);
        Ok(Commit::Fresh)
    }

    /// 取消：排队中的直接移除，执行中的在下一次续约时告知持有者。返回任务是否还在。
    pub fn cancel(&self, id: TaskId) -> Result<bool, Error> {
        let r = self.cancel_inner(id);
        self.maybe_snapshot();
        r
    }

    fn cancel_inner(&self, id: TaskId) -> Result<bool, Error> {
        let term = self.writable()?;
        let slot = self.slot_of_id(id);
        let mut s = lock(&slot.shard);
        if s.work(id).is_none() {
            return Ok(false);
        }
        let account = s.account(id);
        let rec = Record::Cancel { id };
        self.log(term, &rec)?;
        s.apply(&rec, self.clock.now_ms(), Duration::ZERO);
        self.finish_task(term, &account, id, false)?;
        self.publish(slot, &s);
        Ok(true)
    }

    /// 处理到期：过期租约放回队头，清理过了保留期的完成记录。由计时器在 `next` 时刻调用。
    pub fn tick(&self) -> Tick {
        let mut out = Tick::default();
        if self.is_follower() {
            return out;
        }
        let now = self.clock.now_ms();
        let mut next = u64::MAX;
        for slot in self.slots.iter() {
            if slot.next_deadline.load(Ordering::Acquire) <= now {
                let mut s = lock(&slot.shard);
                let expired = s.expire(now);
                for e in &expired {
                    // 释放失败（写日志出错）时冻结留着，重启时按任务状态清理
                    let _ = self.finish_task(self.term(), &e.account, e.task, false);
                }
                out.expired.extend(expired);
                let horizon = now.saturating_sub(s.max_ttl);
                s.prune_done(horizon);
                self.publish(slot, &s);
            }
            next = next.min(slot.next_deadline.load(Ordering::Acquire));
        }
        out.next = (next != u64::MAX).then_some(Deadline(next));
        self.maybe_snapshot();
        out
    }
}
