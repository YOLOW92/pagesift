//! 账户的每日配额与每分钟请求数。
//!
//! 配额只存在账本里：额度先冻结（`Reserve`）、后结算（`Settle`）或释放（`Release`），
//! 都是追加的日志记录；当天余额 = 每日上限 − 已结算 − 冻结中，由记录求和得到，
//! 崩溃重放、温备回放后一致。随任务分配自动冻结的额度（一页一份）在提交时结算，
//! 租约过期或取消时释放；任务放回队列后下次分配再冻结。
//!
//! 每分钟请求数用内存令牌桶（容量 = 每分钟请求数，匀速补充）。每个放行的请求写一条
//! `Request` 记录（墙钟时刻）；没有桶的账户（重启、温备接管后）由最近一分钟的记录重建：
//! 一分钟前的桶按满桶算再依次扣掉窗口内的请求。桶从空补满正好一分钟，所以重建的桶
//! 不会比真实的更满超过窗口开头那一刻的缺口，只可能多放行、不会多拒绝。
//!
//! 按账户分片（与任务分片同一个槽位规则），热路径不碰全局锁。

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use std::sync::atomic::Ordering;

use pagesift_stat::rng::splitmix64;

use super::drr::Acct;
use super::snapshot::{HoldSnap, ReqSnap, UsageSnap};
use super::{Ledger, SLOT_BITS, SLOT_MASK, Slot};
use crate::journal::Record;
use crate::types::{Error, TaskId, lock};

/// 一天、一分钟的毫秒数（UTC 日按 Unix 时间整除）。
pub(crate) const DAY_MS: u64 = 86_400 * 1000;
const MINUTE_MS: u64 = 60 * 1000;

/// 账户的限额（来自部署配置；各项不设就是不限）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountLimits {
    pub pages_per_day: Option<u64>,
    pub requests_per_minute: Option<u32>,
}

/// 一笔冻结。`id == 0` 表示账户不限额，没有记账。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    pub id: u64,
    pub account: Arc<str>,
    /// UTC 日序号（Unix 毫秒 / 一天）。
    pub day: u64,
    pub amount: u64,
}

/// 被限额拒绝，或账本出错。
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    /// 当天配额不够；`retry_after` 到下一个 UTC 零点。
    Quota {
        limit: u64,
        retry_after: Duration,
    },
    /// 每分钟请求数超限；`retry_after` 是等到一个令牌的时间。
    Rate {
        limit: u32,
        retry_after: Duration,
    },
    Error(Error),
}

impl From<Error> for Refusal {
    fn from(e: Error) -> Self {
        Self::Error(e)
    }
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last: u64,
}

impl Bucket {
    /// 按速率 `rpm / 分钟` 补到 `now`，容量 `rpm`。
    fn refill(&mut self, rpm: f64, now: u64) {
        let dt = now.saturating_sub(self.last) as f64;
        self.tokens = (self.tokens + dt * rpm / MINUTE_MS as f64).min(rpm);
        self.last = self.last.max(now);
    }
}

#[derive(Debug, Default)]
pub(crate) struct QuotaShard {
    limits: HashMap<Arc<str>, AccountLimits>,
    /// 冻结中的额度，及随分配冻结时所属的任务。
    holds: HashMap<u64, (Hold, Option<TaskId>)>,
    /// 随分配自动冻结的额度属于哪个任务。
    task_holds: HashMap<TaskId, u64>,
    /// （账户，日）→ 已结算 / 冻结中。
    settled: HashMap<(Arc<str>, u64), u64>,
    reserved: HashMap<(Arc<str>, u64), u64>,
    /// 最近一分钟放行的请求（墙钟毫秒，个数），重建令牌桶用。
    recent: HashMap<Arc<str>, VecDeque<(u64, u64)>>,
    buckets: HashMap<Arc<str>, Bucket>,
    pub(crate) next_seq: u64,
}

impl QuotaShard {
    pub(crate) fn set_limits(&mut self, account: &Arc<str>, l: AccountLimits) {
        if self.limits.get(account).map(|x| x.requests_per_minute) != Some(l.requests_per_minute) {
            self.buckets.remove(account);
        }
        self.limits.insert(account.clone(), l);
    }

    pub(crate) fn limits(&self, account: &str) -> AccountLimits {
        self.limits.get(account).copied().unwrap_or_default()
    }

    /// 当天还剩多少；不限额时为 `None`。
    pub(crate) fn balance(&self, account: &Arc<str>, day: u64) -> Option<u64> {
        let max = self.limits(account).pages_per_day?;
        let key = (account.clone(), day);
        let used = self.settled.get(&key).copied().unwrap_or(0)
            + self.reserved.get(&key).copied().unwrap_or(0);
        Some(max.saturating_sub(used))
    }

    pub(crate) fn task_hold(&self, task: TaskId) -> Option<Hold> {
        Some(self.holds.get(self.task_holds.get(&task)?)?.0.clone())
    }

    pub(crate) fn task_holds(&self) -> Vec<(TaskId, Hold)> {
        self.task_holds
            .iter()
            .filter_map(|(t, h)| Some((*t, self.holds.get(h)?.0.clone())))
            .collect()
    }

    /// 丢掉今天之前的日账（配额按 UTC 日重置）。
    pub(crate) fn prune(&mut self, today: u64) {
        self.settled.retain(|(_, d), _| *d >= today);
        self.reserved.retain(|(_, d), _| *d >= today);
    }

    fn close(&mut self, hold: u64) -> Option<Hold> {
        let (h, task) = self.holds.remove(&hold)?;
        if let Some(t) = task {
            self.task_holds.remove(&t);
        }
        if let Some(r) = self.reserved.get_mut(&(h.account.clone(), h.day)) {
            *r = r.saturating_sub(h.amount);
        }
        Some(h)
    }

    pub(crate) fn apply(&mut self, rec: &Record) {
        match rec {
            Record::Reserve {
                hold,
                account,
                day,
                amount,
                task,
            } => {
                self.next_seq = self.next_seq.max(hold >> super::SLOT_BITS);
                let h = Hold {
                    id: *hold,
                    account: account.clone(),
                    day: *day,
                    amount: *amount,
                };
                *self.reserved.entry((account.clone(), *day)).or_default() += amount;
                self.holds.insert(*hold, (h, *task));
                if let Some(t) = task {
                    self.task_holds.insert(*t, *hold);
                }
            }
            Record::Settle { hold, actual, .. } => {
                if let Some(h) = self.close(*hold) {
                    *self.settled.entry((h.account, h.day)).or_default() += actual;
                }
            }
            Record::Release { hold, .. } => {
                self.close(*hold);
            }
            Record::Request { account, at } => {
                let q = self.recent.entry(account.clone()).or_default();
                q.push_back((*at, 1));
                while q.front().is_some_and(|&(t, _)| t + MINUTE_MS < *at) {
                    q.pop_front();
                }
                if let Some(b) = self.buckets.get_mut(account) {
                    b.tokens -= 1.0;
                }
            }
            _ => {}
        }
    }

    /// 快照：冻结、当天以来的已结算、最近一分钟的请求。请求按秒合并，时刻取所在秒的末尾：
    /// 消耗记得越晚，此前补充的令牌被容量截掉得越多，重建出的桶只会更空（不比逐条记录宽松）。
    pub(crate) fn export(&self) -> (Vec<HoldSnap>, Vec<UsageSnap>, Vec<ReqSnap>) {
        let holds = self
            .holds
            .values()
            .map(|(h, task)| HoldSnap {
                hold: h.id,
                account: h.account.clone(),
                day: h.day,
                amount: h.amount,
                task: *task,
            })
            .collect();
        let usage = self
            .settled
            .iter()
            .map(|((account, day), n)| UsageSnap {
                account: account.clone(),
                day: *day,
                settled: *n,
            })
            .collect();
        let mut per_second: HashMap<(Arc<str>, u64), u64> = HashMap::new();
        for (a, q) in &self.recent {
            for &(t, n) in q {
                *per_second
                    .entry((a.clone(), t.div_ceil(1000) * 1000))
                    .or_default() += n;
            }
        }
        let requests = per_second
            .into_iter()
            .map(|((account, at), count)| ReqSnap { account, at, count })
            .collect();
        (holds, usage, requests)
    }

    pub(crate) fn import(&mut self, holds: &[HoldSnap], usage: &[UsageSnap], requests: &[ReqSnap]) {
        for h in holds {
            self.apply(&Record::Reserve {
                hold: h.hold,
                account: h.account.clone(),
                day: h.day,
                amount: h.amount,
                task: h.task,
            });
        }
        for u in usage {
            *self.settled.entry((u.account.clone(), u.day)).or_default() += u.settled;
        }
        for r in requests {
            let q = self.recent.entry(r.account.clone()).or_default();
            q.push_back((r.at, r.count));
            q.make_contiguous().sort_unstable();
        }
    }

    /// 清空账目（温备从快照重新同步时），限额是配置，保留。
    pub(crate) fn clear(&mut self) {
        let limits = std::mem::take(&mut self.limits);
        *self = Self {
            limits,
            ..Self::default()
        };
    }

    /// 令牌桶检查：放行时返回 `Ok`（调用方随后写 `Request` 记录并应用，桶在应用时扣减）。
    pub(crate) fn check_rate(&mut self, account: &Arc<str>, now: u64) -> Result<(), Refusal> {
        let Some(rpm) = self.limits(account).requests_per_minute else {
            return Ok(());
        };
        let cap = f64::from(rpm.max(1));
        let recent = self.recent.get(account);
        let b = self.buckets.entry(account.clone()).or_insert_with(|| {
            // 没有桶：一分钟前按满桶算，依次扣掉窗口内放行的请求
            let start = now.saturating_sub(MINUTE_MS);
            let mut b = Bucket {
                tokens: cap,
                last: start,
            };
            for &(t, n) in recent.into_iter().flatten().filter(|&&(t, _)| t >= start) {
                b.refill(cap, t);
                b.tokens -= n as f64;
            }
            b
        });
        b.refill(cap, now);
        if b.tokens >= 1.0 {
            return Ok(());
        }
        let wait_ms = ((1.0 - b.tokens) * MINUTE_MS as f64 / cap).ceil() as u64;
        Err(Refusal::Rate {
            limit: rpm,
            retry_after: Duration::from_millis(wait_ms),
        })
    }
}

impl Ledger {
    /// 账户的配额槽位：（槽位号，所在分片）。冻结编号的低位就是槽位号，与任务编号同一规则。
    fn quota_at(&self, account: &str) -> (u64, &Slot) {
        let bits = splitmix64(crate::types::hash64(account.as_bytes())) & SLOT_MASK;
        (bits, &self.slots[(bits & self.mask) as usize])
    }

    pub(super) fn quota_slot(&self, account: &str) -> &Slot {
        self.quota_at(account).1
    }

    /// 设置账户的限额（部署配置；温备也要设同样的值）。每日配额同时是公平调度的权重。
    pub fn set_limits(&self, account: &str, limits: AccountLimits) {
        let a: Arc<str> = account.into();
        lock(&self.quota_slot(account).quota).set_limits(&a, limits);
        lock(&self.registry).set_weight(&a, limits.pages_per_day);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    fn today(&self) -> u64 {
        self.clock.unix_ms() / DAY_MS
    }

    /// 当天还剩多少页；不限额时为 `None`。
    pub fn balance(&self, account: &str) -> Option<u64> {
        let q = lock(&self.quota_slot(account).quota);
        q.balance(&account.into(), self.today())
    }

    fn quota_refusal(&self, limit: u64) -> Refusal {
        let now = self.clock.unix_ms();
        let retry_after = Duration::from_millis(DAY_MS - now % DAY_MS);
        Refusal::Quota { limit, retry_after }
    }

    /// 冻结 `amount` 页；余额不够时拒绝。不限额的账户得到 `id == 0` 的冻结，不记账。
    pub fn reserve(&self, account: &str, amount: u64) -> Result<Hold, Refusal> {
        let r = self.reserve_inner(account, amount);
        self.maybe_snapshot();
        r
    }

    fn reserve_inner(&self, account: &str, amount: u64) -> Result<Hold, Refusal> {
        let term = self.writable()?;
        let (bits, slot) = self.quota_at(account);
        let mut q = lock(&slot.quota);
        let (a, day): (Arc<str>, u64) = (account.into(), self.today());
        q.prune(day);
        let Some(left) = q.balance(&a, day) else {
            return Ok(Hold {
                id: 0,
                account: a,
                day,
                amount,
            });
        };
        if left < amount {
            return Err(self.quota_refusal(q.limits(account).pages_per_day.unwrap_or(0)));
        }
        let id = self.reserve_locked(term, &mut q, bits, a.clone(), day, amount, None)?;
        Ok(Hold {
            id,
            account: a,
            day,
            amount,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn reserve_locked(
        &self,
        term: u64,
        q: &mut QuotaShard,
        bits: u64,
        account: Arc<str>,
        day: u64,
        amount: u64,
        task: Option<TaskId>,
    ) -> Result<u64, Error> {
        let hold = ((q.next_seq + 1) << SLOT_BITS) | bits;
        let rec = Record::Reserve {
            hold,
            account,
            day,
            amount,
            task,
        };
        self.log(term, &rec)?;
        q.apply(&rec);
        Ok(hold)
    }

    /// 按实际用量结算（`actual` 可以小于冻结量，余下的退回）。
    pub fn settle(&self, hold: &Hold, actual: u64) -> Result<(), Error> {
        self.close_hold(hold, Some(actual))
    }

    /// 放弃冻结，不记账。
    pub fn release(&self, hold: &Hold) -> Result<(), Error> {
        self.close_hold(hold, None)
    }

    fn close_hold(&self, hold: &Hold, settle: Option<u64>) -> Result<(), Error> {
        if hold.id == 0 {
            return Ok(());
        }
        let term = self.writable()?;
        let r = {
            let mut q = lock(&self.quota_slot(&hold.account).quota);
            self.close_locked(term, &mut q, hold, settle)
        };
        self.maybe_snapshot();
        r
    }

    fn close_locked(
        &self,
        term: u64,
        q: &mut QuotaShard,
        h: &Hold,
        settle: Option<u64>,
    ) -> Result<(), Error> {
        let (hold, account) = (h.id, h.account.clone());
        let rec = match settle {
            Some(actual) => Record::Settle {
                hold,
                account,
                actual,
            },
            None => Record::Release { hold, account },
        };
        self.log(term, &rec)?;
        q.apply(&rec);
        Ok(())
    }

    /// 每分钟请求数：放行时记一条 `Request`（温备据此重建令牌桶）。
    pub fn check_rate(&self, account: &str) -> Result<(), Refusal> {
        let r = self.check_rate_inner(account);
        self.maybe_snapshot();
        r
    }

    fn check_rate_inner(&self, account: &str) -> Result<(), Refusal> {
        let term = self.writable()?;
        let mut q = lock(&self.quota_slot(account).quota);
        let a: Arc<str> = account.into();
        q.check_rate(&a, self.clock.unix_ms())?;
        if q.limits(account).requests_per_minute.is_none() {
            return Ok(());
        }
        let rec = Record::Request {
            account: a,
            at: self.clock.unix_ms(),
        };
        self.log(term, &rec)?;
        q.apply(&rec);
        Ok(())
    }

    /// 分配时为任务冻结一页；余额不够时返回 `false`（不分配）。不限额的账户不冻结。
    pub(super) fn hold_task(
        &self,
        term: u64,
        account: &Arc<str>,
        task: TaskId,
    ) -> Result<bool, Error> {
        let (bits, slot) = self.quota_at(account);
        let mut q = lock(&slot.quota);
        let day = self.today();
        q.prune(day);
        match q.balance(account, day) {
            None => Ok(true),
            Some(0) => Ok(false),
            Some(_) => {
                self.reserve_locked(term, &mut q, bits, account.clone(), day, 1, Some(task))?;
                Ok(true)
            }
        }
    }

    /// 任务结束：随分配冻结的一页在提交时结算，在租约过期或取消时释放。
    pub(super) fn finish_task(
        &self,
        term: u64,
        account: &Acct,
        task: TaskId,
        settle: bool,
    ) -> Result<(), Error> {
        let Some(a) = account else { return Ok(()) };
        let mut q = lock(&self.quota_slot(a).quota);
        let Some(h) = q.task_hold(task) else {
            return Ok(());
        };
        let amount = settle.then_some(h.amount);
        self.close_locked(term, &mut q, &h, amount)
    }

    /// 恢复或接管后：任务已完成而冻结还在的结算，任务不在租约中的释放（崩溃在两条记录之间）。
    pub(super) fn close_orphan_holds(&self, term: u64) -> Result<(), Error> {
        for qs in self.slots.iter() {
            let holds = lock(&qs.quota).task_holds();
            for (task, h) in holds {
                let state = lock(&self.slot_of_id(task).shard).leased(task);
                if state == Some(true) {
                    continue;
                }
                let mut q = lock(&qs.quota);
                let amount = state.is_none().then_some(h.amount);
                self.close_locked(term, &mut q, &h, amount)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_sums_records_and_bucket_rebuilds_from_recent_requests() {
        let a: Arc<str> = "a".into();
        let mut q = QuotaShard::default();
        q.set_limits(
            &a,
            AccountLimits {
                pages_per_day: Some(10),
                requests_per_minute: Some(3),
            },
        );
        let reserve = |hold, amount| Record::Reserve {
            hold,
            account: a.clone(),
            day: 1,
            amount,
            task: None,
        };
        q.apply(&reserve(1, 4));
        q.apply(&reserve(2, 3));
        assert_eq!(q.balance(&a, 1), Some(3));
        q.apply(&Record::Settle {
            hold: 1,
            account: a.clone(),
            actual: 2,
        });
        q.apply(&Record::Release {
            hold: 2,
            account: a.clone(),
        });
        assert_eq!(q.balance(&a, 1), Some(8), "settled 2, released 3");
        assert_eq!(q.balance(&a, 2), Some(10), "a new UTC day starts full");

        // 最近一分钟放行了 3 个请求：重建的桶是空的；再过 20 s 补回一个
        let mut r = QuotaShard::default();
        r.set_limits(&a, q.limits(&a));
        for at in [1_000, 2_000, 3_000] {
            r.apply(&Record::Request {
                account: a.clone(),
                at,
            });
        }
        assert!(matches!(r.check_rate(&a, 3_000), Err(Refusal::Rate { .. })));
        assert!(r.check_rate(&a, 23_000).is_ok());
    }
}
