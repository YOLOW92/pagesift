//! 账本的变更日志。
//!
//! 每次改变任务状态的操作都先写成一条 [`Record`]，追加到 `pagesift-store` 的只追加列表，
//! 再应用到内存状态；重启恢复、温备回放都是把同一串记录按序重新应用，所以与在线路径逐条等价。
//! 每条记录带写入者的任期 `term`：温备接管后任期加一，旧任期的记录被拒绝（栅栏）。
//!
//! 只用到 store 已有的列表接口（`list_push`、`list_range`、`list_len`）；列表只追加不弹出，
//! 所以序号就是日志下标，温备按下标增量拉取。
//!
//! 记录：`[版本 u8][任期 u64][类型 u8][字段…]`，字段编码见 [`crate::codec`]。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pagesift_stat::{DecodeError, Reader, Writer};
use pagesift_store::Store;
use tokio::sync::Notify;

use crate::codec::{codec_enum, codec_struct};
use crate::ledger::snapshot::{DoneSnap, HoldSnap, ReqSnap, SeenSnap, TaskSnap, UsageSnap};
use crate::types::{Error, Fence, StatKind, StatsDelta, Task, TaskId, WorkerId};

/// [协议] 日志记录格式版本，放在每条记录的第一个字节；改布局时加一（4：加快照记录）
pub const JOURNAL_VERSION: u8 = 4;

/// 存放日志的列表名。
const LIST: &[u8] = b"cluster/journal";
/// 逻辑下标与列表序号之差（温备从快照重新同步时整体平移），存在 KV 里。
const OFFSET: &[u8] = b"cluster/journal-offset";

/// 统计增量的来源：工人 id 与它本次启动的编号（重启后序号从头开始，不能与上次混在一起）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Origin {
    pub worker: WorkerId,
    pub boot: u64,
}

codec_struct! {
    Origin { worker, boot }
}

codec_enum! {
    #[derive(Debug, Clone, PartialEq)]
    pub enum Record {
        /// 新任务；`account` 为空的任务不参与账户间的公平调度。
        1 => Submit { id: TaskId, task: Task, account: Option<Arc<str>> }, // [协议] 记录类型标签
        /// 分配给 `worker`（`None` 表示单主机本进程）。
        2 => Grant { id: TaskId, fence: Fence, worker: Option<WorkerId> }, // [协议] 同上
        /// 完成。完成标记与结果在同一条 CRC 记录里（幂等提交）；
        /// 单主机模式不序列化结果，`result` 为空，由结果出口自己持久化。
        3 => Done { id: TaskId, fence: Fence, result: Option<Vec<u8>> }, // [协议] 同上
        4 => Cancel { id: TaskId }, // [协议] 同上
        /// 合并进网关的统计增量（merge 后存入账本）。
        5 => Stats { origin: Origin, delta: StatsDelta }, // [协议] 同上
        /// 新任期开始（重启恢复或温备接管）。
        6 => Term {}, // [协议] 同上
        /// 冻结配额：`day` 是 UTC 日序号，`task` 是随分配自动冻结时的任务。
        7 => Reserve { hold: u64, account: Arc<str>, day: u64, amount: u64, task: Option<TaskId> }, // [协议] 同上
        /// 结算：按实际用量 `actual` 记账，冻结解除。
        8 => Settle { hold: u64, account: Arc<str>, actual: u64 }, // [协议] 同上
        /// 释放：冻结解除，不记账（租约过期、取消、上层放弃）。
        9 => Release { hold: u64, account: Arc<str> }, // [协议] 同上
        /// 一次被放行的请求（墙钟毫秒），温备接管时据最近一分钟的记录重建令牌桶。
        10 => Request { account: Arc<str>, at: u64 }, // [协议] 同上
        /// 快照开始：此后到 `SnapEnd` 的记录合起来是账本在这一刻的全部状态；
        /// `task_hw`、`hold_hw` 是分配过的最大任务、冻结序号（编号不重用）。
        11 => SnapBegin { task_hw: u64, hold_hw: u64 }, // [协议] 同上
        12 => SnapTasks { tasks: Vec<TaskSnap> }, // [协议] 同上
        13 => SnapDone { done: Vec<DoneSnap> }, // [协议] 同上
        14 => SnapQuota { holds: Vec<HoldSnap>, usage: Vec<UsageSnap>, requests: Vec<ReqSnap> }, // [协议] 同上
        15 => SnapStats { tables: Vec<(StatKind, Vec<u8>)>, seen: Vec<SeenSnap> }, // [协议] 同上
        16 => SnapEnd {}, // [协议] 同上
        /// 上层（API 任务记录）的一次写入，内容由上层编码，账本只负责落日志、传给温备（[`crate::AppState`]）。
        17 => App { op: Vec<u8> }, // [协议] 同上
        /// 快照里上层状态的一段（[`crate::AppState::dump`] 的结果）。
        18 => SnapApp { ops: Vec<Vec<u8>> }, // [协议] 同上
    }
}

/// 解码后的一条日志。
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub term: u64,
    pub rec: Record,
}

pub fn encode(term: u64, rec: &Record) -> Vec<u8> {
    let mut w = Writer::new();
    w.u8(JOURNAL_VERSION);
    w.u64(term);
    w.u8(rec.tag());
    rec.put_body(&mut w);
    w.into_inner()
}

impl Entry {
    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(buf);
        if r.u8()? != JOURNAL_VERSION {
            return Err(DecodeError::Invalid("journal version"));
        }
        let term = r.u64()?;
        let tag = r.u8()?;
        let rec = Record::get_body(tag, &mut r)?;
        if !r.is_empty() {
            return Err(DecodeError::Invalid("journal trailing bytes"));
        }
        Ok(Self { term, rec })
    }
}

/// 存在 store 里的日志。下标是逻辑下标：主网关上等于列表序号，温备从快照重新同步后整体平移。
/// 头部在快照之后截断，所以第一条存活记录是最近一次快照的开始（或从未做过快照时的第一条）。
pub struct Journal {
    store: Store,
    appended: Arc<Notify>,
    offset: AtomicU64,
    /// 最近一次快照之后追加的字节数，与最近一次快照本身的字节数（决定何时做下一次快照）。
    pub(crate) since: AtomicU64,
    pub(crate) snap: AtomicU64,
}

impl Journal {
    pub fn new(store: Store) -> Self {
        let offset = store
            .tasks()
            .kv_get(OFFSET)
            .ok()
            .flatten()
            .and_then(|v| Some(u64::from_le_bytes(v.try_into().ok()?)))
            .unwrap_or(0);
        Self {
            store,
            appended: Arc::new(Notify::new()),
            offset: AtomicU64::new(offset),
            since: AtomicU64::new(0),
            snap: AtomicU64::new(0),
        }
    }

    fn offset(&self) -> u64 {
        self.offset.load(Ordering::Acquire)
    }

    /// 追加一条，返回它的下标。
    pub fn append(&self, term: u64, rec: &Record) -> Result<u64, Error> {
        self.append_raw(&encode(term, rec))
    }

    /// 追加一条已编码的记录（温备回放主网关发来的原样字节）。
    pub fn append_raw(&self, raw: &[u8]) -> Result<u64, Error> {
        let i = self.store.tasks().list_push(LIST, raw)? + self.offset();
        self.since.fetch_add(raw.len() as u64, Ordering::Relaxed);
        self.appended.notify_waiters();
        Ok(i)
    }

    /// 下一条的下标。
    pub fn len(&self) -> u64 {
        self.store.tasks().list_next(LIST) + self.offset()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 第一条存活记录的下标。
    pub fn head(&self) -> u64 {
        self.len() - self.store.tasks().list_len(LIST)
    }

    /// 截掉下标小于 `before` 的记录。
    pub(crate) fn trim(&self, before: u64) -> Result<(), Error> {
        let phys = before.saturating_sub(self.offset());
        self.store.tasks().list_trim(LIST, phys)?;
        Ok(())
    }

    /// 清空并让下一条的下标从 `index` 开始（温备从主网关的快照重新同步）。
    pub(crate) fn reset_to(&self, index: u64) -> Result<(), Error> {
        let t = self.store.tasks();
        t.list_trim(LIST, u64::MAX)?;
        let offset = index - t.list_next(LIST);
        t.kv_put(OFFSET, &offset.to_le_bytes())?;
        self.offset.store(offset, Ordering::Release);
        Ok(())
    }

    /// 从下标 `from`（早于头部时从头部）起读，累计编码字节不超过 `budget`
    /// （至少读一条，单条超过预算时也读出）。
    pub fn read(&self, from: u64, budget: usize) -> Result<Vec<(u64, Vec<u8>)>, Error> {
        let mut out = Vec::new();
        let mut used = 0usize;
        let mut i = from.max(self.head());
        let off = self.offset();
        while let Some(raw) = self.store.tasks().list_range(LIST, i - off, 1)?.pop() {
            used += raw.len();
            if used > budget && !out.is_empty() {
                break;
            }
            out.push((i, raw));
            i += 1;
        }
        Ok(out)
    }

    /// 等到有新记录写入。调用方先登记再检查长度，避免漏掉通知：
    /// `let n = j.notified(); if j.len() == seen { n.await }`。
    pub fn notified(&self) -> tokio::sync::futures::OwnedNotified {
        Arc::clone(&self.appended).notified_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Resource, Resources, SiteKey, StatKind};
    use std::time::Duration;

    #[test]
    fn every_record_round_trips() {
        let task = Task {
            site: SiteKey(9),
            class: 3,
            timeout: Duration::from_millis(1500),
            need: Resources::default().with(Resource::Cpu, 0.25),
            body: b"url".to_vec(),
        };
        let fence = Fence { term: 2, epoch: 5 };
        let recs = [
            Record::Submit {
                id: TaskId(1),
                task,
                account: Some("acct".into()),
            },
            Record::Grant {
                id: TaskId(1),
                fence,
                worker: Some(WorkerId::new("w1")),
            },
            Record::Grant {
                id: TaskId(1),
                fence,
                worker: None,
            },
            Record::Done {
                id: TaskId(1),
                fence,
                result: Some(b"artifact".to_vec()),
            },
            Record::Cancel { id: TaskId(4) },
            Record::Stats {
                origin: Origin {
                    worker: WorkerId::new("w2"),
                    boot: 77,
                },
                delta: StatsDelta {
                    kind: StatKind(1),
                    seq: 3,
                    table: vec![1, 2, 3],
                },
            },
            Record::Term {},
            Record::Reserve {
                hold: 9,
                account: "a".into(),
                day: 20_000,
                amount: 3,
                task: Some(TaskId(2)),
            },
            Record::Settle {
                hold: 9,
                account: "a".into(),
                actual: 2,
            },
            Record::Release {
                hold: 9,
                account: "a".into(),
            },
            Record::Request {
                account: "a".into(),
                at: 5,
            },
        ];
        for rec in recs {
            let buf = encode(2, &rec);
            assert_eq!(Entry::decode(&buf).unwrap(), Entry { term: 2, rec });
            assert!(Entry::decode(&buf[..buf.len() - 1]).is_err());
        }
    }

    #[test]
    fn journal_appends_and_reads_by_index() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::new(Store::open_dir(dir.path()).unwrap());
        for t in 0..5 {
            assert_eq!(j.append(t, &Record::Term {}).unwrap(), t);
        }
        assert_eq!(j.len(), 5);
        let got = j.read(2, usize::MAX).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].0, 2);
        assert_eq!(Entry::decode(&got[0].1).unwrap().term, 2);
        assert_eq!(
            j.read(0, 1).unwrap().len(),
            1,
            "budget still reads one record"
        );
    }
}
