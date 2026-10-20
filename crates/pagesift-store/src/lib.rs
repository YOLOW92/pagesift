#![forbid(unsafe_code)]
//! pagesift 的内嵌存储：只追加的分段日志 + 内存哈希索引（PRD 6.8、F-6.4）。
//!
//! 三个命名空间共用一套日志：
//! - [`Cache`]：抓取结果缓存，语义对齐 Firecrawl 的 `maxAge`、`minAge`、`storeInCache`；
//! - [`Snapshots`]：原始 HTML 快照，zstd 压缩，按内容哈希去重；
//! - [`Tasks`]：爬取任务状态用的 KV、追加列表和集合。
//!
//! # 并发与异步
//!
//! [`Store`] 是 `Clone + Send + Sync` 的句柄，多读单写：读只拿读锁并用 pread 读盘，
//! 写操作串行。所有方法都是同步的，单次调用通常只有一次系统调用；
//! 在 tokio 里可以用 [`Store::blocking`]（`tokio` 特性，默认开启）放进 `spawn_blocking`。
//!
//! # 持久性
//!
//! 每条记录带长度和 CRC32。重新打开时逐条校验，遇到写了一半的尾部就截断到最后一条完整记录。
//! 何时 fsync 由 [`SyncPolicy`] 决定。同一个目录同一时间只能被一个进程打开。

mod cache;
mod engine;
mod index;
mod record;
mod snapshot;
mod tasks;
#[cfg(test)]
mod tests;

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use cache::{Cache, CacheEntry, CacheLookup, CachePolicy};
pub use engine::Store;
pub use snapshot::{ContentHash, SnapshotMeta, SnapshotPut, Snapshots};
pub use tasks::Tasks;

/// 写入后何时调用 fsync。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPolicy {
    /// 每次写入都 fsync，最安全也最慢。
    Always,
    /// 距上次 fsync 超过这个间隔时，在下一次写入后 fsync。进程崩溃不丢数据（已写入内核），
    /// 断电最多丢这个间隔内的写入。
    Interval(Duration),
    /// 交给操作系统。
    Never,
}

/// 打开存储的配置，用 [`Config::new`] 创建后按需修改字段。
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Config {
    pub dir: PathBuf,
    pub sync: SyncPolicy,
    /// 单个段文件的大小上限，超过就换新段。不能超过 2GiB。
    pub max_segment_bytes: u64,
    /// 存储空间上限（F-8.4）。超过后按 LRU 淘汰缓存和快照内容，任务状态不会被淘汰。
    pub max_bytes: Option<u64>,
    /// 淘汰到上限的这个比例为止。
    pub low_watermark: f64,
    /// 无效数据（被覆盖、删除）占比达到这个值时后台压缩。
    pub compact_ratio: f64,
    /// 总大小低于这个值时不自动压缩。
    pub compact_min_bytes: u64,
    /// 是否在后台线程自动压缩和淘汰；关掉后需要自己调用 [`Store::gc`]、[`Store::compact`]。
    pub background: bool,
    /// 单个快照（压缩前）的大小上限。
    pub snapshot_max_bytes: usize,
    pub snapshot_zstd_level: i32,
    /// 测试用：把哈希截短以制造冲突。
    pub(crate) hash_mask: u64,
}

impl Config {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            sync: SyncPolicy::Interval(Duration::from_secs(1)),
            max_segment_bytes: 64 << 20,
            max_bytes: None,
            low_watermark: 0.9,
            compact_ratio: 0.5,
            compact_min_bytes: 16 << 20,
            background: true,
            snapshot_max_bytes: 512 << 10,
            snapshot_zstd_level: 3,
            hash_mask: u64::MAX,
        }
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Io(io::Error),
    /// 段文件不是本存储的格式。
    Corrupt(String),
    /// 键超过 65535 字节。
    KeyTooLarge(usize),
    /// 值或快照超过上限。
    ValueTooLarge {
        size: usize,
        limit: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "store io error: {e}"),
            Error::Corrupt(m) => write!(f, "store corrupt: {m}"),
            Error::KeyTooLarge(n) => write!(f, "key too large: {n} bytes"),
            Error::ValueTooLarge { size, limit } => {
                write!(f, "value too large: {size} bytes (limit {limit})")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// 打开时重建索引的情况。
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct OpenReport {
    pub segments: usize,
    pub records: u64,
    /// 因为写了一半或校验失败被截掉的字节数。
    pub truncated_bytes: u64,
    pub elapsed: Duration,
}

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Stats {
    pub entries: u64,
    /// 索引堆内存估算值（字节）。
    pub index_bytes: u64,
    pub disk_bytes: u64,
    pub live_bytes: u64,
    pub segments: usize,
    pub open: OpenReport,
    /// 后台维护最近一次失败的原因。
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct GcReport {
    pub evicted_entries: u64,
    pub evicted_bytes: u64,
    pub compacted_segments: usize,
    pub disk_bytes_before: u64,
    pub disk_bytes_after: u64,
}

pub(crate) fn to_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

pub(crate) fn from_ms(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms)
}

pub(crate) fn now_secs() -> u32 {
    (to_ms(SystemTime::now()) / 1000) as u32
}
