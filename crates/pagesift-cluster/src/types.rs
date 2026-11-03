//! 集群的基本类型：任务、租约、栅栏、资源、归属、统计增量。
//!
//! `Lease`、`Resources` 定义在这里，`pagesift-flow` 里没有对应的类型。

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

pub use pagesift_stat::decide::{Cost as Resources, Resource};
pub use pagesift_stat::table::Scope;

/// 工人的稳定标识（配置给出，重启不变）：HRW 用它算站点归属，令牌按它登记。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkerId(Arc<str>);

impl WorkerId {
    pub fn new(id: impl Into<Arc<str>>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 任务编号，由账本分配，跨故障切换唯一。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(pub u64);

/// 站点键：注册域的 64 位哈希，与 `pagesift_stat::table::Scope::Site` 用同一个值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SiteKey(pub u64);

impl SiteKey {
    /// 由注册域（eTLD+1，小写）得到站点键：BLAKE3 摘要的前 8 字节。
    pub fn of_domain(registered_domain: &str) -> Self {
        Self(hash64(registered_domain.as_bytes()))
    }
}

/// 栅栏令牌：网关任期 `term` 与本任务的分配次数 `epoch`，按字典序比较。
///
/// 每次重新分配 `epoch+1`；温备接管时 `term+1`。只接受与账本里当前分配完全相同的令牌，
/// 所以旧任期（旧网关在失联后私自发出的）和旧分配（已被重新分配的）写入都会被拒绝（不变量 5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Fence {
    pub term: u64,
    pub epoch: u64,
}

/// 一个待执行的任务。`body` 是上层编码好的任务描述（`Source`、选项等），本 crate 不解析。
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub site: SiteKey,
    /// 任务类别（单页、PDF、渲染……）：租约时长按类别统计。
    pub class: u32,
    /// 本任务的总预算（请求的 `timeout`）。还没有该类别的耗时统计时，租约时长取它（冷启动）。
    pub timeout: Duration,
    /// 预计占用的四种资源，拉取时与工人报告的可用量比较。
    pub need: Resources,
    pub body: Vec<u8>,
}

/// 协调端本地单调时钟上的时刻（毫秒，见 [`crate::clock`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Deadline(pub u64);

/// 执行权：持有者在 `expires` 之前提交或续约，否则任务被重新分配。
#[derive(Debug, Clone)]
pub struct Lease {
    pub task: TaskId,
    pub epoch: u64,
    pub term: u64,
    /// 本地时钟上的截止时刻（工人一侧已扣掉往返时间的上界，见 [`crate::remote`]）。
    pub expires: Deadline,
    /// 租约时长：`q(1−δ)`（该类任务耗时）+ `q(1−δ)`（往返时间）。
    pub ttl: Duration,
    /// 持有者拿到租约的本地时刻（算执行耗时用）。
    pub received: Deadline,
    /// 拿到租约后多久该续约（[`crate::ledger::renew_after`] 的推导）。
    pub renew_after: Duration,
    pub work: Arc<Task>,
    /// 站点刚换负责人时，旧负责人交来的礼貌状态，执行前先导入。
    pub handoff: Option<SiteState>,
}

impl Lease {
    pub fn fence(&self) -> Fence {
        Fence {
            term: self.term,
            epoch: self.epoch,
        }
    }
}

/// 续约的结果。
#[derive(Debug, Clone, PartialEq)]
pub enum Renewal {
    Extended {
        expires: Deadline,
        ttl: Duration,
    },
    /// 任务已被取消（网关改状态，下一次续约时告知），持有者应停止执行。
    Cancel,
}

/// 站点的负责者。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Owner {
    /// 单主机：全部站点归本进程。
    Local,
    Worker(WorkerId),
    /// 多服务器模式下当前没有工人在线。
    Unassigned,
}

/// 站点礼貌状态的交接包。`state` 由礼貌模块自己编码（下次可发时间、惩罚、退避），
/// `wait` 是导出时刻起新负责人至少还要等多久（旧负责人最后一次请求时间 + 当前礼貌间隔 − 现在）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteState {
    pub site: SiteKey,
    pub wait: Duration,
    pub state: Vec<u8>,
}

/// 统计表的种类编号：由使用方定义常量，网关按编号找到对应的 `Stat` 类型合并。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StatKind(pub u32);

/// 一次统计增量：`table` 是 `pagesift_stat::table::StatTable::encode` 的结果。
/// `seq` 由发送方按（种类）递增编号，网关据此丢弃重复。
#[derive(Debug, Clone, PartialEq)]
pub struct StatsDelta {
    pub kind: StatKind,
    pub seq: u64,
    pub table: Vec<u8>,
}

/// 下发的先验：全局层与所请求站点（或路径族）层的条目，每种统计一张编码后的表。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Priors {
    pub tables: Vec<(StatKind, Vec<u8>)>,
}

/// 协调操作的错误。
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// 栅栏拒绝：令牌不是当前分配（已被重新分配，或来自旧任期）。`current` 是账本里的当前令牌。
    Fenced { current: Fence },
    /// 任务已取消。
    Cancelled,
    /// 账本里没有这个任务（从未存在，或完成已久、去重记录已清理）。
    UnknownTask,
    /// 握手被拒：令牌不对、版本不兼容、角色不对。
    Rejected(String),
    /// 网络或帧格式错误。
    Transport(String),
    /// 存储读写失败。
    Store(String),
    /// 连接已关闭。
    Closed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fenced { current } => write!(
                f,
                "fenced: current lease is term {} epoch {}",
                current.term, current.epoch
            ),
            Self::Cancelled => f.write_str("task cancelled"),
            Self::UnknownTask => f.write_str("unknown task"),
            Self::Rejected(m) => write!(f, "rejected: {m}"),
            Self::Transport(m) => write!(f, "transport: {m}"),
            Self::Store(m) => write!(f, "store: {m}"),
            Self::Closed => f.write_str("connection closed"),
        }
    }
}

impl std::error::Error for Error {}

impl From<pagesift_store::Error> for Error {
    fn from(e: pagesift_store::Error) -> Self {
        Self::Store(e.to_string())
    }
}

impl From<pagesift_stat::DecodeError> for Error {
    fn from(e: pagesift_stat::DecodeError) -> Self {
        Self::Transport(e.to_string())
    }
}

/// 资源是否够：每一种需要量都不超过可用量。
pub fn fits(need: &Resources, free: &Resources) -> bool {
    Resource::ALL.iter().all(|&r| need.get(r) <= free.get(r))
}

/// BLAKE3 摘要的前 8 字节（小端）。
pub(crate) fn hash64(bytes: &[u8]) -> u64 {
    let h = blake3::hash(bytes);
    let (head, _) = h
        .as_bytes()
        .split_first_chunk::<{ size_of::<u64>() }>()
        .expect("32-byte digest");
    u64::from_le_bytes(*head)
}

/// 取锁；持锁的线程 panic 过也照常用（各处的状态在每次修改后都自洽）。
pub(crate) fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
