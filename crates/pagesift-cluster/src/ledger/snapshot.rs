//! 检查点：把账本的全部状态写成一段快照记录（`SnapBegin` … `SnapEnd`），再截掉之前的日志。
//!
//! 何时做：自上次快照以来追加的字节数超过上次快照本身的字节数时。这样每写一字节普通日志，
//! 最多再摊到一字节快照（写放大 ≤ 2），日志里存活的字节不超过"一份快照 + 一份快照那么多的
//! 增量"；状态越大快照越少，状态小时快照便宜、做得勤。不需要任何固定的间隔或大小阈值。
//!
//! 快照时按"所有任务分片 → 所有配额分片 → 统计"的顺序拿全部锁（与在线路径的加锁顺序一致），
//! 期间其他写入等待，停顿与状态大小成正比、按上面的规则摊还。快照按帧上限分成多条记录，
//! 温备能原样收到；回放从第一条存活记录（最近一次快照的开始）读起，只在状态为空时载入快照，
//! 状态已是最新时（温备同步中、或崩溃在写完快照与截断之间）跳过。

use std::sync::atomic::Ordering;

use super::Ledger;
use crate::codec::{Codec, codec_struct};
use crate::journal::{Entry, Journal, Origin, Record, encode};
use crate::ledger::shard::Shard;
use crate::types::{Error, Fence, StatKind, Task, TaskId, WorkerId, lock};
use std::sync::Arc;

/// 一个任务：`state` 0 排队、1 租约中、2 租约中但已取消。
#[derive(Debug, Clone, PartialEq)]
pub struct TaskSnap {
    pub id: TaskId,
    pub task: Task,
    pub account: Option<Arc<str>>,
    pub last: Fence,
    pub state: u8,
    pub worker: Option<WorkerId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DoneSnap {
    pub id: TaskId,
    pub fence: Fence,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HoldSnap {
    pub hold: u64,
    pub account: Arc<str>,
    pub day: u64,
    pub amount: u64,
    pub task: Option<TaskId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageSnap {
    pub account: Arc<str>,
    pub day: u64,
    pub settled: u64,
}

/// `count` 个请求，时刻按所在秒的末尾记。
#[derive(Debug, Clone, PartialEq)]
pub struct ReqSnap {
    pub account: Arc<str>,
    pub at: u64,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeenSnap {
    pub origin: Origin,
    pub kind: StatKind,
    pub below: u64,
    pub above: Vec<u64>,
}

codec_struct! {
    TaskSnap { id, task, account, last, state, worker }
    DoneSnap { id, fence }
    HoldSnap { hold, account, day, amount, task }
    UsageSnap { account, day, settled }
    ReqSnap { account, at, count }
    SeenSnap { origin, kind, below, above }
}

// [协议] 回放到快照中的位置：不在快照里、正在载入、跳过
const OUTSIDE: u8 = 0;
const LOADING: u8 = 1; // [协议] 同上
const SKIPPING: u8 = 2; // [协议] 同上

/// 按编码字节切块，每块不超过 `budget`（单个元素超过时独占一块）。
fn chunks<T: Codec + Clone>(items: Vec<T>, budget: usize) -> Vec<Vec<T>> {
    let mut out = vec![Vec::new()];
    let mut used = 0;
    for x in items {
        let mut w = pagesift_stat::Writer::new();
        x.put(&mut w);
        let n = w.as_slice().len();
        if used + n > budget && !out.last().is_some_and(Vec::is_empty) {
            out.push(Vec::new());
            used = 0;
        }
        used += n;
        out.last_mut().expect("non-empty").push(x);
    }
    out
}

impl Ledger {
    /// 回放本地日志（从最近一次快照起），并据此恢复何时做下一次快照的字节计数。
    pub(super) fn replay(&self) -> Result<(), Error> {
        let Some(j) = &self.journal else {
            return Ok(());
        };
        let (mut since, mut snap, mut section) = (0, 0, 0);
        // 截断过的日志一定从快照开始；截断做到一半时崩溃会在前面留下快照之前的记录，跳过它们
        // （0.90 照常回放这些残留，状态因此不再"新"，后面的快照被当成已是最新而跳过）
        let mut leftover = j.head() > 0;
        for (_, raw) in j.read(0, usize::MAX)? {
            let e = Entry::decode(&raw)?;
            if leftover && !matches!(e.rec, Record::SnapBegin { .. }) {
                continue;
            }
            leftover = false;
            let n = raw.len() as u64;
            match e.rec {
                Record::SnapBegin { .. } => section = n,
                Record::SnapEnd {} => {
                    (snap, since) = (section + n, 0);
                    section = 0;
                }
                _ if section > 0 => section += n,
                _ => since += n,
            }
            self.term.fetch_max(e.term, Ordering::SeqCst);
            self.apply_entry(&e);
        }
        j.since.store(since, Ordering::Relaxed);
        j.snap.store(snap, Ordering::Relaxed);
        Ok(())
    }

    /// 处理快照记录；不是快照记录时返回 `false`。
    pub(super) fn apply_snapshot_record(&self, rec: &Record) -> bool {
        let mode = self.snap_mode.load(Ordering::Acquire);
        let now = self.clock.now_ms();
        match rec {
            Record::SnapBegin { task_hw, hold_hw } => {
                if !self.fresh.load(Ordering::Acquire) {
                    self.snap_mode.store(SKIPPING, Ordering::Release);
                    return true;
                }
                self.snap_mode.store(LOADING, Ordering::Release);
                for slot in self.slots.iter() {
                    let mut s = lock(&slot.shard);
                    s.next_seq = s.next_seq.max(*task_hw);
                    let mut q = lock(&slot.quota);
                    q.next_seq = q.next_seq.max(*hold_hw);
                }
            }
            Record::SnapEnd {} => {
                if mode == LOADING {
                    self.fresh.store(false, Ordering::Release);
                }
                self.snap_mode.store(OUTSIDE, Ordering::Release);
            }
            _ if mode != LOADING => {
                return matches!(
                    rec,
                    Record::SnapTasks { .. }
                        | Record::SnapDone { .. }
                        | Record::SnapQuota { .. }
                        | Record::SnapStats { .. }
                        | Record::SnapApp { .. }
                );
            }
            Record::SnapTasks { tasks } => {
                for t in tasks {
                    self.add_account(&t.account);
                    let slot = self.slot_of_id(t.id);
                    let mut s = lock(&slot.shard);
                    s.import_task(t, now);
                    self.publish(slot, &s);
                }
            }
            Record::SnapDone { done } => {
                for d in done {
                    lock(&self.slot_of_id(d.id).shard).import_done(d, now);
                }
            }
            Record::SnapQuota {
                holds,
                usage,
                requests,
            } => {
                for slot in self.slots.iter() {
                    // 每个账户的条目只进它自己的分片
                    let mine = |a: &str| std::ptr::eq(self.quota_slot(a), slot);
                    let h: Vec<_> = holds.iter().filter(|x| mine(&x.account)).cloned().collect();
                    let u: Vec<_> = usage.iter().filter(|x| mine(&x.account)).cloned().collect();
                    let r: Vec<_> = requests
                        .iter()
                        .filter(|x| mine(&x.account))
                        .cloned()
                        .collect();
                    lock(&slot.quota).import(&h, &u, &r);
                }
            }
            Record::SnapStats { tables, seen } => lock(&self.stats).import(tables, seen),
            Record::SnapApp { ops } => ops.iter().for_each(|op| self.apply_app(op)),
            _ => return false,
        }
        true
    }

    /// 字节计数到了就做一次快照（只在主网关或单主机上；同一时刻只有一个）。
    pub(super) fn maybe_snapshot(&self) {
        let Some(j) = &self.journal else { return };
        if self.is_follower()
            || j.since.load(Ordering::Relaxed) <= j.snap.load(Ordering::Relaxed)
            || self.snapshotting.swap(true, Ordering::AcqRel)
        {
            return;
        }
        // 写快照失败时日志保持原样，下一次写入后再试
        let _ = self.snapshot(j);
        self.snapshotting.store(false, Ordering::Release);
    }

    fn snapshot(&self, j: &Journal) -> Result<(), Error> {
        let term = self.term();
        let shards: Vec<_> = self.slots.iter().map(|s| lock(&s.shard)).collect();
        let quotas: Vec<_> = self.slots.iter().map(|s| lock(&s.quota)).collect();
        let stats = lock(&self.stats);
        // [推导] 每条快照记录占帧上限的一半，温备能原样收到（与网关日志传送的批大小同一规则）
        let budget = crate::transport::default_max_frame() / 2;
        let begin = j.len();
        let mut bytes = 0u64;
        let mut put = |rec: Record| -> Result<(), Error> {
            let raw = encode(term, &rec);
            bytes += raw.len() as u64;
            j.append_raw(&raw).map(drop)
        };
        let task_hw = shards.iter().map(|s| s.next_seq).max().unwrap_or(0);
        let hold_hw = quotas.iter().map(|q| q.next_seq).max().unwrap_or(0);
        put(Record::SnapBegin { task_hw, hold_hw })?;
        let (mut tasks, mut done) = (Vec::new(), Vec::new());
        for s in &shards {
            let (t, d) = Shard::export(s);
            tasks.extend(t);
            done.extend(d);
        }
        for tasks in chunks(tasks, budget) {
            put(Record::SnapTasks { tasks })?;
        }
        for done in chunks(done, budget) {
            put(Record::SnapDone { done })?;
        }
        let (mut holds, mut usage, mut requests) = (Vec::new(), Vec::new(), Vec::new());
        for q in &quotas {
            let (h, u, r) = q.export();
            holds.extend(h);
            usage.extend(u);
            requests.extend(r);
        }
        for holds in chunks(holds, budget) {
            put(Record::SnapQuota {
                holds,
                usage: Vec::new(),
                requests: Vec::new(),
            })?;
        }
        put(Record::SnapQuota {
            holds: Vec::new(),
            usage,
            requests,
        })?;
        let (tables, seen) = stats.export();
        put(Record::SnapStats { tables, seen })?;
        if let Some(app) = self.app.get() {
            for ops in chunks(app.dump(), budget) {
                put(Record::SnapApp { ops })?;
            }
        }
        put(Record::SnapEnd {})?;
        j.trim(begin)?;
        j.since.store(0, Ordering::Relaxed);
        j.snap.store(bytes, Ordering::Relaxed);
        Ok(())
    }

    /// 清空状态（温备从主网关的快照重新同步之前）。限额、已登记的统计种类、学到的耗时保留。
    pub(super) fn reset_state(&self) {
        for slot in self.slots.iter() {
            let mut s = lock(&slot.shard);
            let map = Arc::clone(&s.map);
            *s = Shard::new(self.local, map);
            self.publish(slot, &s);
            lock(&slot.quota).clear();
        }
        lock(&self.stats).clear();
        self.fresh.store(true, Ordering::Release);
        self.snap_mode.store(OUTSIDE, Ordering::Release);
    }

    /// 温备：主网关的快照结束后同样截断自己的日志。
    pub(super) fn after_shipped(&self, index: u64, rec: &Record, len: u64) -> Result<(), Error> {
        let Some(j) = &self.journal else {
            return Ok(());
        };
        match rec {
            Record::SnapBegin { .. } => {
                self.snap_begin.store(index, Ordering::Release);
                j.since.store(len, Ordering::Relaxed);
            }
            Record::SnapEnd {} => {
                let begin = self.snap_begin.load(Ordering::Acquire);
                j.snap
                    .store(j.since.load(Ordering::Relaxed), Ordering::Relaxed);
                j.since.store(0, Ordering::Relaxed);
                j.trim(begin)?;
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::clock::ManualClock;
    use crate::journal::{Journal, Record, encode};
    use crate::ledger::{Ledger, LedgerConfig};
    use crate::types::{Resource, Resources, SiteKey, Task, TaskId};
    use std::time::Duration;

    fn task(site: u64) -> Task {
        Task {
            site: SiteKey(site),
            class: 1,
            timeout: Duration::from_secs(60),
            need: Resources::default().with(Resource::Cpu, 1.0),
            body: b"t".to_vec(),
        }
    }

    fn count(l: &Ledger) -> usize {
        let free = Resources::default().with(Resource::Cpu, 4.0);
        let mut n = 0;
        while let Some(x) = l.pull(None, &free, Duration::ZERO).unwrap() {
            l.commit(x.task, x.fence(), None, None, |_| {}).unwrap();
            n += 1;
        }
        n
    }

    /// 截断做到一半时崩溃：日志开头是快照之前的残留记录。重启后跳过残留、载入快照，任务一个不少
    /// （0.90 先回放残留，状态不再"新"，快照被跳过，只剩残留里的任务）。
    #[test]
    fn replay_skips_records_left_before_the_snapshot_by_an_interrupted_trim() {
        let (d1, d2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let s1 = pagesift_store::Store::open_dir(d1.path()).unwrap();
        let l1 = Ledger::open(
            LedgerConfig::new(ManualClock::new(), true).with_journal(Journal::new(s1.clone())),
        )
        .unwrap();
        for i in 0..5 {
            l1.submit(task(i)).unwrap();
        }
        let j1 = l1.journal().unwrap();
        let live = j1.read(0, usize::MAX).unwrap();
        assert!(matches!(
            crate::journal::Entry::decode(&live[0].1).unwrap().rec,
            Record::SnapBegin { .. }
        ));
        drop(l1);
        // 第二份日志：两条残留（一个不在快照里的任务），后面是第一份的快照和之后的记录，再删掉第一条
        let s2 = pagesift_store::Store::open_dir(d2.path()).unwrap();
        let stray = encode(
            1,
            &Record::Submit {
                id: TaskId((999 << crate::ledger::SLOT_BITS) | 1),
                task: task(99),
                account: None,
            },
        );
        let j2 = Journal::new(s2.clone());
        j2.append_raw(&stray).unwrap();
        j2.append_raw(&stray).unwrap();
        for (_, raw) in &live {
            j2.append_raw(raw).unwrap();
        }
        s2.tasks().list_trim(b"cluster/journal", 1).unwrap();
        drop(j2);
        let l2 = Ledger::open(
            LedgerConfig::new(ManualClock::new(), true).with_journal(Journal::new(s2)),
        )
        .unwrap();
        assert_eq!(count(&l2), 5);
    }
}
