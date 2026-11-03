//! 账本的一个分片：一部分站点的任务、队列、租约和完成记录。
//!
//! 所有状态变化都经 [`Shard::apply`] 应用一条日志记录，在线路径与回放路径逐条相同。
//! 只有到期（租约过期后放回队列）和续约不写日志：截止时刻是本机时钟上的值，
//! 接管时统一按新时钟重新给（[`Shard::restart_leases`]）。

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use pagesift_stat::Delta;

use crate::clock::ms;
use crate::hrw::HostMap;
use crate::journal::Record;
use crate::ledger::drr::Acct;
use crate::ledger::duration::{Durations, LocalDurations};
use crate::ledger::snapshot::{DoneSnap, TaskSnap};
use crate::types::{
    Deadline, Error, Fence, Owner, Renewal, Resources, SiteKey, SiteState, Task, TaskId, WorkerId,
    fits,
};

#[derive(Debug)]
enum St {
    Queued,
    Leased {
        worker: Option<WorkerId>,
        deadline: u64,
        granted: u64,
        /// 持有者最近一次就这个租约说话（分配或续约）的时刻。
        touched: u64,
    },
    Cancelled,
}

#[derive(Debug)]
struct Entry {
    work: Arc<Task>,
    acct: Acct,
    /// 最近一次分配的令牌（还没分配过时为 `Fence::default()`）。
    last: Fence,
    st: St,
}

/// 提交检查的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Commit {
    /// 第一次提交，结果已接受。
    Fresh,
    /// 同一令牌的重复提交：幂等，结果不再交付。
    Duplicate,
}

/// 一个被放回队列的过期租约。
#[derive(Debug, Clone, PartialEq)]
pub struct Expired {
    pub task: TaskId,
    pub fence: Fence,
    pub worker: Option<WorkerId>,
    /// 持有者最后一次分配或续约这个租约的时刻：此后它一直没续约，租约才会到期。
    pub since: Deadline,
    /// 任务所属账户（随分配冻结的配额在到期时释放）。
    pub account: Option<Arc<str>>,
}

#[derive(Debug, Default)]
pub(crate) struct Shard {
    entries: HashMap<TaskId, Entry>,
    /// 每个（站点，账户）的待执行任务（可能含已被分配或取消的过期编号，弹出时跳过）。
    sites: HashMap<(SiteKey, Acct), VecDeque<TaskId>>,
    /// 每个站点在途（已分配、还没完成或到期）的任务：换负责人时只看这一站。
    leased: HashMap<SiteKey, HashSet<TaskId>>,
    /// 每个（负责者，账户）轮转的站点队列：站点在这里当且仅当它在这个账户下的队列非空。
    ready: HashMap<(Owner, Acct), VecDeque<SiteKey>>,
    heap: BinaryHeap<Reverse<(u64, TaskId)>>,
    /// 已完成：令牌与完成时刻，用来识别重复提交。
    done: HashMap<TaskId, (Fence, u64)>,
    done_order: VecDeque<(u64, TaskId)>,
    /// 站点暂停到这个时刻（换负责人时的交接期）。
    paused: HashMap<SiteKey, u64>,
    /// 等新负责人下次拿到租约时一并带走的礼貌状态。
    handoffs: HashMap<SiteKey, SiteState>,
    pub(crate) map: Arc<HostMap>,
    pub(crate) local: bool,
    /// 本分片分配过的最大序号（任务编号的高位）。
    pub(crate) next_seq: u64,
    pub(crate) queued: u64,
    /// 发出过的最长租约（毫秒）：完成记录至少保留这么久，持有者在此期间的重复提交都能认出来。
    pub(crate) max_ttl: u64,
    pub(crate) durations: LocalDurations,
}

impl Shard {
    pub(crate) fn new(local: bool, map: Arc<HostMap>) -> Self {
        Self {
            local,
            map,
            ..Self::default()
        }
    }

    /// 放进站点队列（`front` 放队头）。排队计数由调用方维护。
    pub(crate) fn enqueue(&mut self, id: TaskId, front: bool) {
        let Some(e) = self.entries.get(&id) else {
            return;
        };
        let (site, acct) = (e.work.site, e.acct.clone());
        let owner = self.map.owner_of(site, self.local);
        let q = self.sites.entry((site, acct.clone())).or_default();
        if q.is_empty() {
            self.ready.entry((owner, acct)).or_default().push_back(site);
        }
        if front {
            q.push_front(id);
        } else {
            q.push_back(id);
        }
    }

    fn lease(&mut self, id: TaskId, worker: Option<WorkerId>, now: u64, ttl: Duration) {
        let Some(e) = self.entries.get_mut(&id) else {
            return;
        };
        if matches!(e.st, St::Queued) {
            self.queued -= 1;
        }
        let deadline = now.saturating_add(ms(ttl));
        e.st = St::Leased {
            worker,
            deadline,
            granted: now,
            touched: now,
        };
        self.leased.entry(e.work.site).or_default().insert(id);
        self.heap.push(Reverse((deadline, id)));
    }

    fn unlease(&mut self, site: SiteKey, id: TaskId) {
        if let Some(s) = self.leased.get_mut(&site) {
            s.remove(&id);
            if s.is_empty() {
                self.leased.remove(&site);
            }
        }
    }

    /// 应用一条日志记录。`now` 和 `ttl` 只用于新分配的截止时刻。
    pub(crate) fn apply(&mut self, rec: &Record, now: u64, ttl: Duration) {
        match rec {
            Record::Submit { id, task, account } => {
                self.next_seq = self.next_seq.max(id.0 >> super::SLOT_BITS);
                let work = Arc::new(task.clone());
                let (acct, last, st) = (account.clone(), Fence::default(), St::Queued);
                self.entries.insert(
                    *id,
                    Entry {
                        work,
                        acct,
                        last,
                        st,
                    },
                );
                self.queued += 1;
                self.enqueue(*id, false);
            }
            Record::Grant { id, fence, worker } => {
                if let Some(e) = self.entries.get_mut(id) {
                    e.last = *fence;
                    self.max_ttl = self.max_ttl.max(ms(ttl));
                    self.lease(*id, worker.clone(), now, ttl);
                }
            }
            Record::Done { id, fence, .. } => {
                if let Some(e) = self.entries.remove(id) {
                    match e.st {
                        St::Queued => self.queued -= 1,
                        _ => self.unlease(e.work.site, *id),
                    }
                }
                self.done.insert(*id, (*fence, now));
                self.done_order.push_back((now, *id));
            }
            Record::Cancel { id } => {
                let Some(e) = self.entries.get_mut(id) else {
                    return;
                };
                match e.st {
                    St::Queued => {
                        self.queued -= 1;
                        self.entries.remove(id);
                    }
                    St::Leased { .. } => e.st = St::Cancelled,
                    St::Cancelled => {}
                }
            }
            _ => {}
        }
    }

    /// 给 `owner` 在账户 `acct` 下挑一个资源够、站点未暂停的任务，按站点轮转。
    pub(crate) fn pick(
        &mut self,
        owner: &Owner,
        acct: &Acct,
        free: &Resources,
        now: u64,
    ) -> Option<TaskId> {
        let key = (owner.clone(), acct.clone());
        let mut ready = self.ready.remove(&key)?;
        let mut found = None;
        for _ in 0..ready.len() {
            let Some(site) = ready.pop_front() else { break };
            if self.paused.get(&site).is_some_and(|&t| t > now) {
                ready.push_back(site);
                continue;
            }
            self.paused.remove(&site);
            let skey = (site, acct.clone());
            let Some(q) = self.sites.get_mut(&skey) else {
                continue;
            };
            while let Some(id) = q.front() {
                if self
                    .entries
                    .get(id)
                    .is_some_and(|e| matches!(e.st, St::Queued))
                {
                    break;
                }
                q.pop_front();
            }
            let Some(&id) = q.front() else {
                self.sites.remove(&skey);
                continue;
            };
            if !fits(&self.entries[&id].work.need, free) {
                ready.push_back(site);
                continue;
            }
            q.pop_front();
            if q.is_empty() {
                self.sites.remove(&skey);
            } else {
                ready.push_back(site);
            }
            found = Some(id);
            break;
        }
        if !ready.is_empty() {
            self.ready.insert(key, ready);
        }
        found
    }

    pub(crate) fn last_fence(&self, id: TaskId) -> Option<Fence> {
        self.entries.get(&id).map(|e| e.last)
    }

    pub(crate) fn work(&self, id: TaskId) -> Option<Arc<Task>> {
        self.entries.get(&id).map(|e| Arc::clone(&e.work))
    }

    pub(crate) fn account(&self, id: TaskId) -> Acct {
        self.entries.get(&id).and_then(|e| e.acct.clone())
    }

    /// 任务是否在租约中；`None` 表示已完成，账本里没有了时为 `Some(false)`。
    pub(crate) fn leased(&self, id: TaskId) -> Option<bool> {
        if self.done.contains_key(&id) {
            return None;
        }
        Some(
            self.entries
                .get(&id)
                .is_some_and(|e| matches!(e.st, St::Leased { .. })),
        )
    }

    pub(crate) fn take_handoff(&mut self, site: SiteKey) -> Option<SiteState> {
        self.handoffs.remove(&site)
    }

    /// 令牌检查：`Ok(true)` 表示正是这个令牌已经完成；令牌不是当前分配时报 `Fenced`。
    fn check(&self, id: TaskId, fence: Fence) -> Result<bool, Error> {
        if let Some(&(f, _)) = self.done.get(&id) {
            return if f == fence {
                Ok(true)
            } else {
                Err(Error::Fenced { current: f })
            };
        }
        let e = self.entries.get(&id).ok_or(Error::UnknownTask)?;
        if e.last != fence {
            return Err(Error::Fenced { current: e.last });
        }
        Ok(false)
    }

    /// 续约：令牌必须是当前分配；持有者自己已经提交过的让它停下。
    /// 租约已过期、但还没重新分配的任务，原持有者续约时重新租给它（没有别人在做，省掉重做）。
    pub(crate) fn renew(
        &mut self,
        id: TaskId,
        fence: Fence,
        worker: Option<&WorkerId>,
        now: u64,
        ttl: Duration,
    ) -> Result<Renewal, Error> {
        if self.check(id, fence)? {
            return Ok(Renewal::Cancel);
        }
        self.check_holder(id, worker)?;
        let until = now.saturating_add(ms(ttl));
        let e = self.entries.get_mut(&id).expect("checked above");
        match &mut e.st {
            St::Cancelled => return Ok(Renewal::Cancel),
            St::Leased {
                deadline, touched, ..
            } => {
                *deadline = until;
                *touched = now;
                self.heap.push(Reverse((until, id)));
            }
            St::Queued => self.lease(id, worker.cloned(), now, ttl),
        }
        Ok(Renewal::Extended {
            expires: Deadline(until),
            ttl,
        })
    }

    /// 租约在别的工人手里时拒绝 `by`（`None`：进程内调用，不查）。令牌是可以猜的小整数，
    /// 只凭令牌，任何通过认证的工人都能提交或续约别人的任务（0.90）。
    fn check_holder(&self, id: TaskId, by: Option<&WorkerId>) -> Result<(), Error> {
        if let (
            Some(by),
            Some(St::Leased {
                worker: Some(h), ..
            }),
        ) = (by, self.entries.get(&id).map(|e| &e.st))
            && h != by
        {
            return Err(Error::Rejected(format!(
                "task {} is leased to another worker",
                id.0
            )));
        }
        Ok(())
    }

    /// 提交前检查令牌。接受当前分配；租约过期但还没重新分配的也接受（没有别人在做）。
    /// `by` 是提交的工人：租约在别的工人手里时拒绝。
    pub(crate) fn check_commit(
        &self,
        id: TaskId,
        fence: Fence,
        by: Option<&WorkerId>,
    ) -> Result<Commit, Error> {
        if self.check(id, fence)? {
            return Ok(Commit::Duplicate);
        }
        self.check_holder(id, by)?;
        match self.entries[&id].st {
            St::Cancelled => Err(Error::Cancelled),
            St::Leased { .. } | St::Queued => Ok(Commit::Fresh),
        }
    }

    /// 本次提交的耗时（分配到提交）。
    pub(crate) fn elapsed(&self, id: TaskId, now: u64) -> Option<Duration> {
        match self.entries.get(&id)?.st {
            St::Leased { granted, .. } => Some(Duration::from_millis(now.saturating_sub(granted))),
            _ => None,
        }
    }

    pub(crate) fn next_deadline(&self) -> Option<u64> {
        self.heap.peek().map(|Reverse((d, _))| *d)
    }

    /// 把到期的租约放回队头（下一个负责者优先做）；已取消的直接丢弃。
    pub(crate) fn expire(&mut self, now: u64) -> Vec<Expired> {
        let mut out = Vec::new();
        while let Some(&Reverse((d, id))) = self.heap.peek() {
            if d > now {
                break;
            }
            self.heap.pop();
            let Some(e) = self.entries.get_mut(&id) else {
                continue;
            };
            let site = e.work.site;
            match &e.st {
                St::Leased {
                    deadline,
                    worker,
                    touched,
                    ..
                } if *deadline <= now => {
                    out.push(Expired {
                        task: id,
                        fence: e.last,
                        worker: worker.clone(),
                        since: Deadline(*touched),
                        account: e.acct.clone(),
                    });
                    e.st = St::Queued;
                    self.queued += 1;
                    self.unlease(site, id);
                    self.enqueue(id, true);
                }
                St::Cancelled => {
                    self.entries.remove(&id);
                    self.unlease(site, id);
                }
                _ => {}
            }
        }
        out
    }

    /// 清理 `horizon` 之前完成的去重记录。
    pub(crate) fn prune_done(&mut self, horizon: u64) {
        while let Some(&(at, id)) = self.done_order.front() {
            if at >= horizon {
                break;
            }
            self.done_order.pop_front();
            if self.done.get(&id).is_some_and(|&(_, t)| t == at) {
                self.done.remove(&id);
            }
        }
    }

    /// 接管或重启后：所有未完成的租约按新时钟重新计时，持有者仍可续约或提交。
    pub(crate) fn restart_leases(&mut self, now: u64, global: &Durations, delta: Delta) {
        self.heap.clear();
        for (id, e) in &mut self.entries {
            if let St::Leased {
                deadline,
                granted,
                touched,
                ..
            } = &mut e.st
            {
                let w = &e.work;
                let ttl = self.durations.task_hi(w.class, w.timeout, global, delta);
                *deadline = now.saturating_add(ms(ttl));
                *granted = now;
                *touched = now;
                self.heap.push(Reverse((*deadline, *id)));
            }
        }
    }

    /// 站点从 `from` 手里迁走后最早可以再发请求的时刻：`now + wait`，且不早于 `from`
    /// 在该站点上所有在途租约的截止（它的请求可能还在路上）。
    fn resume_at(&self, site: SiteKey, from: &WorkerId, wait: Duration, now: u64) -> u64 {
        let horizon = self
            .leased
            .get(&site)
            .into_iter()
            .flatten()
            .filter_map(|id| match &self.entries.get(id)?.st {
                St::Leased {
                    worker: Some(w),
                    deadline,
                    ..
                } if w == from => Some(*deadline),
                _ => None,
            });
        horizon.fold(now.saturating_add(ms(wait)), u64::max)
    }

    /// 换成员表：重算各站点的负责者；换了负责者的站点暂停到交接完成。
    /// `wait_for(旧负责人)` 是等它交出礼貌状态的时长（它仍在线时为一个往返上界，已离线时为 0）。
    pub(crate) fn set_map(
        &mut self,
        map: Arc<HostMap>,
        now: u64,
        wait_for: &dyn Fn(&WorkerId) -> Duration,
    ) {
        let old = std::mem::replace(&mut self.map, map);
        let mut sites: Vec<SiteKey> = self
            .sites
            .keys()
            .map(|(s, _)| *s)
            .chain(self.leased.keys().copied())
            .collect();
        sites.sort_unstable();
        sites.dedup();
        for (site, from, _) in old.moved(&self.map, sites) {
            let Some(from) = from else { continue };
            let until = self.resume_at(site, &from, wait_for(&from), now);
            if until > now {
                let p = self.paused.entry(site).or_insert(0);
                *p = (*p).max(until);
            }
        }
        let mut keys: Vec<(SiteKey, Acct)> = self.sites.keys().cloned().collect();
        keys.sort_unstable();
        let mut ready: HashMap<(Owner, Acct), VecDeque<SiteKey>> = HashMap::new();
        for (site, acct) in keys {
            let owner = self.map.owner_of(site, self.local);
            ready.entry((owner, acct)).or_default().push_back(site);
        }
        self.ready = ready;
    }

    /// 旧负责人交来礼貌状态：暂停改为它给出的剩余等待（不早于它在该站点的租约截止）。
    pub(crate) fn handoff(&mut self, from: &WorkerId, s: SiteState, now: u64) {
        let until = self.resume_at(s.site, from, s.wait, now);
        if until > now {
            self.paused.insert(s.site, until);
        } else {
            self.paused.remove(&s.site);
        }
        self.handoffs.insert(s.site, s);
    }

    /// 快照：全部任务（状态、最近一次令牌、持有者）与去重用的完成记录。
    pub(crate) fn export(&self) -> (Vec<TaskSnap>, Vec<DoneSnap>) {
        let tasks = self
            .entries
            .iter()
            .map(|(id, e)| {
                let (state, worker) = match &e.st {
                    St::Queued => (0, None),
                    St::Leased { worker, .. } => (1, worker.clone()),
                    St::Cancelled => (2, None),
                };
                TaskSnap {
                    id: *id,
                    task: (*e.work).clone(),
                    account: e.acct.clone(),
                    last: e.last,
                    state,
                    worker,
                }
            })
            .collect();
        let done = self
            .done
            .iter()
            .map(|(id, (fence, _))| DoneSnap {
                id: *id,
                fence: *fence,
            })
            .collect();
        (tasks, done)
    }

    /// 从快照载入一个任务。租约的截止时刻在接管或重启时统一重算。
    pub(crate) fn import_task(&mut self, t: &TaskSnap, now: u64) {
        let work = Arc::new(t.task.clone());
        let (acct, last, st) = (t.account.clone(), t.last, St::Queued);
        self.entries.insert(
            t.id,
            Entry {
                work,
                acct,
                last,
                st,
            },
        );
        self.queued += 1;
        match t.state {
            0 => self.enqueue(t.id, false),
            state => {
                self.lease(t.id, t.worker.clone(), now, Duration::ZERO);
                if state == 2
                    && let Some(e) = self.entries.get_mut(&t.id)
                {
                    e.st = St::Cancelled;
                }
            }
        }
    }

    pub(crate) fn import_done(&mut self, d: &DoneSnap, now: u64) {
        self.done.insert(d.id, (d.fence, now));
        self.done_order.push_back((now, d.id));
    }

    pub(crate) fn pause_until(&self, site: SiteKey) -> Option<u64> {
        self.paused.get(&site).copied()
    }
}
