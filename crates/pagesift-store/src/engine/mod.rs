//! 段文件管理、读写路径、压缩和淘汰。

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Instant, SystemTime};

use crate::index::{Index, Loc, Slot};
use crate::record::{
    self, MAX_KEY, MAX_RECORD, Op, REC_HEADER, SEG_HEADER, SEG_MAGIC, read_exact_at, write_all_at,
};
use crate::{
    Cache, Config, Error, GcReport, Items, OpenReport, Result, Snapshots, Stats, SyncPolicy, Tasks,
    now_secs, to_ms,
};

mod segments;
mod state;
mod tx;

use segments::*;

/// 命名空间编号，写进每条记录，不能改。
pub(crate) mod ns {
    pub const CACHE: u8 = 1;
    pub const SNAP_BLOB: u8 = 2;
    pub const SNAP_ITEM: u8 = 3;
    pub const KV: u8 = 4;
    pub const LIST_ITEM: u8 = 5;
    pub const SET_MEMBER: u8 = 6;
    pub const ITEM: u8 = 7;
    pub const SELECTOR: u8 = 8; // [协议] 本存储格式的命名空间编号（接在 ITEM = 7 之后）
    pub const GRAPH_NODE: u8 = 9; // [协议] DataGraph 版本节点命名空间
    pub const GRAPH_EDGE: u8 = 10; // [协议] DataGraph 版本迁移边命名空间
    pub const GRAPH_FACT: u8 = 11; // [协议] DataGraph 别名与冲突事实命名空间
    pub const GRAPH_JOB: u8 = 12; // [协议] DataGraph 作业摘要与增量命名空间
}

fn evictable(n: u8) -> bool {
    matches!(n, ns::CACHE | ns::SNAP_BLOB)
}

/// 分组键：`[name_len u16][name][rest]`，列表的 rest 是大端序号，集合的 rest 是成员。
pub(crate) fn group_key(name: &[u8], rest: &[u8]) -> Result<Vec<u8>> {
    if name.len() + rest.len() + 2 > MAX_KEY {
        return Err(Error::KeyTooLarge(name.len() + rest.len() + 2));
    }
    let mut k = Vec::with_capacity(2 + name.len() + rest.len());
    k.extend_from_slice(&(name.len() as u16).to_le_bytes());
    k.extend_from_slice(name);
    k.extend_from_slice(rest);
    Ok(k)
}

pub(crate) fn split_group(key: &[u8]) -> Option<(&[u8], &[u8])> {
    let n = u16::from_le_bytes(key.get(..2)?.try_into().ok()?) as usize;
    Some((key.get(2..2 + n)?, &key[2 + n..]))
}

/// 列表和集合的内存统计，由索引变化推出来，重启后重建。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Group {
    pub live: u64,
    /// 列表最小的可能存活序号，弹出时从这里往后探测。
    pub head: u64,
    pub next: u64,
}

struct Seg {
    file: Arc<File>,
    total: u64,
    live: u64,
    tomb: u64,
}

impl Seg {
    fn new(file: Arc<File>) -> Self {
        Self {
            file,
            total: SEG_HEADER,
            live: 0,
            tomb: 0,
        }
    }
    fn garbage(&self) -> u64 {
        self.total - SEG_HEADER - self.live
    }
    fn dead_puts(&self) -> u64 {
        self.garbage() - self.tomb
    }
}

struct State {
    index: Index,
    segs: BTreeMap<u32, Seg>,
    groups: HashMap<(u8, Box<[u8]>), Group>,
    disk: u64,
    live: u64,
}

struct Writer {
    active: u32,
    file: Arc<File>,
    len: u64,
    last_sync: Instant,
}

pub(crate) struct Inner {
    cfg: Config,
    writer: Mutex<Writer>,
    state: RwLock<State>,
    /// 同一时间只跑一个压缩或 gc。
    maint: Mutex<()>,
    busy: AtomicBool,
    last_maint_ms: AtomicU64,
    /// 上次维护结束时的磁盘总量和无效数据量（字节），判断之后垃圾长了多少（[`Store::maybe_maintain`]）。
    last_maint_disk: AtomicU64,
    last_maint_garbage: AtomicU64,
    last_error: Mutex<Option<String>>,
    /// 故障注入：为真时每次写入都失败（[`Store::fail_writes`]）。
    fail_writes: AtomicBool,
    open: OpenReport,
    /// 持有期间其他进程打不开同一个目录；随存储一起释放。
    _lock: File,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if self.cfg.sync != SyncPolicy::Never {
            let w = self
                .writer
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner);
            let _ = w.file.sync_data();
        }
    }
}

/// 存储句柄，克隆很便宜（内部是 `Arc`）。
#[derive(Clone)]
pub struct Store(Arc<Inner>);

impl Store {
    pub fn open(cfg: Config) -> Result<Store> {
        if !(SEG_HEADER + 64..=2 << 30).contains(&cfg.max_segment_bytes) {
            return Err(Error::Corrupt(
                "max_segment_bytes must be within 72B..=2GiB".into(),
            ));
        }
        let start = Instant::now();
        fs::create_dir_all(&cfg.dir)?;
        // 段文件没有跨进程协调：两个进程同时追加会互相覆盖。用目录里的锁文件保证同一时间只有一个
        // 进程打开这个存储；拿不到锁返回 `WouldBlock`，调用方可以决定不用缓存继续运行。
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(cfg.dir.join("LOCK"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "the directory is in use by another process",
                )));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(Error::Io(e)),
        }
        let mut ids: Vec<u32> = fs::read_dir(&cfg.dir)?
            .filter_map(|e| {
                let name = e.ok()?.file_name();
                name.to_str()?.strip_suffix(".seg")?.parse().ok()
            })
            .collect();
        ids.sort_unstable();

        let mut st = State {
            index: Index::new(cfg.hash_mask),
            segs: BTreeMap::new(),
            groups: HashMap::new(),
            disk: 0,
            live: 0,
        };
        let mut report = OpenReport {
            segments: ids.len(),
            ..OpenReport::default()
        };
        let mut last_len = 0;
        for &id in &ids {
            let path = seg_path(&cfg.dir, id);
            let file = OpenOptions::new().read(true).write(true).open(&path)?;
            let len = file.metadata()?.len();
            if len < SEG_HEADER {
                // 新段的魔数没写完就崩溃了。
                file.set_len(0)?;
                write_all_at(&file, SEG_MAGIC, 0)?;
                file.sync_all()?;
                report.truncated_bytes += len;
            } else {
                let mut magic = [0; 8];
                read_exact_at(&file, &mut magic, 0)?;
                if &magic != SEG_MAGIC {
                    return Err(Error::Corrupt(format!("{}: bad magic", path.display())));
                }
            }
            let len = len.max(SEG_HEADER);
            let file = Arc::new(file);
            st.segs.insert(id, Seg::new(file.clone()));
            st.disk += SEG_HEADER;
            let end = record::scan(&file, len, |off, raw, rec| -> Result<()> {
                if rec.ns >= 16 {
                    return Err(Error::Corrupt(format!("unknown namespace {}", rec.ns)));
                }
                let rlen = raw.len() as u64;
                st.account_append(id, rlen, rec.op == Op::Del);
                let new = (rec.op == Op::Put)
                    .then(|| Loc::new(id, off, rlen as u32, rec.ns, (rec.ts_ms / 1000) as u32));
                st.apply(rec.ns, rec.key, new)?;
                report.records += 1;
                Ok(())
            })?;
            if end < len {
                file.set_len(end)?;
                file.sync_all()?;
                report.truncated_bytes += len - end;
            }
            last_len = end;
        }

        let (active, file, len) = match ids.last() {
            Some(&id) if last_len < cfg.max_segment_bytes => {
                (id, st.segs[&id].file.clone(), last_len)
            }
            last => {
                let id = last.map_or(1, |id| id + 1);
                let file = create_segment(&cfg.dir, id)?;
                st.segs.insert(id, Seg::new(file.clone()));
                st.disk += SEG_HEADER;
                (id, file, SEG_HEADER)
            }
        };
        report.elapsed = start.elapsed();
        Ok(Store(Arc::new(Inner {
            cfg,
            writer: Mutex::new(Writer {
                active,
                file,
                len,
                last_sync: Instant::now(),
            }),
            state: RwLock::new(st),
            maint: Mutex::new(()),
            busy: AtomicBool::new(false),
            last_maint_ms: AtomicU64::new(0),
            last_maint_disk: AtomicU64::new(0),
            last_maint_garbage: AtomicU64::new(0),
            last_error: Mutex::new(None),
            fail_writes: AtomicBool::new(false),
            open: report,
            _lock: lock,
        })))
    }

    /// 用默认配置打开目录。
    pub fn open_dir(dir: impl Into<PathBuf>) -> Result<Store> {
        Self::open(Config::new(dir))
    }

    pub fn config(&self) -> &Config {
        &self.0.cfg
    }

    pub fn cache(&self) -> Cache<'_> {
        Cache::new(self)
    }

    pub fn snapshots(&self) -> Snapshots<'_> {
        Snapshots::new(self)
    }

    pub fn items(&self) -> Items<'_> {
        Items::new(self)
    }

    pub fn selectors(&self) -> crate::Selectors<'_> {
        crate::Selectors::new(self)
    }

    pub fn graph(&self) -> crate::GraphStore<'_> {
        crate::GraphStore::new(self)
    }

    pub fn tasks(&self) -> Tasks<'_> {
        Tasks::new(self)
    }

    pub fn stats(&self) -> Stats {
        let st = self.state_r();
        let groups = st.groups.capacity() as u64
            * (std::mem::size_of::<((u8, Box<[u8]>), Group)>() as u64 + 1)
            + st.groups.keys().map(|k| k.1.len() as u64).sum::<u64>();
        Stats {
            entries: st.index.len() as u64,
            index_bytes: st.index.heap_bytes() + groups,
            disk_bytes: st.disk,
            live_bytes: st.live,
            segments: st.segs.len(),
            open: self.0.open.clone(),
            last_error: lock(&self.0.last_error).clone(),
        }
    }

    /// 把活动段 fsync 到盘上。
    pub fn flush(&self) -> Result<()> {
        let mut w = self.writer();
        w.file.sync_data()?;
        w.last_sync = Instant::now();
        Ok(())
    }

    /// 重写所有含无效数据的段（包括当前活动段），返回重写的段数。
    pub fn compact(&self) -> Result<usize> {
        self.compact_with(0.0, true)
    }

    /// 供 `pagesift gc`：超过 [`Config::max_bytes`] 时按 LRU 淘汰缓存和快照内容，然后压缩。
    pub fn gc(&self) -> Result<GcReport> {
        let mut rep = GcReport {
            disk_bytes_before: self.state_r().disk,
            ..GcReport::default()
        };
        if let Some(max) = self.0.cfg.max_bytes {
            let target = (max as f64 * self.0.cfg.low_watermark) as u64;
            let mut cands: Vec<(u32, u32, u32, u32)> = {
                let st = self.state_r();
                if st.live <= target {
                    Vec::new()
                } else {
                    st.index
                        .iter()
                        .filter(|l| evictable(l.ns()))
                        .map(|l| (l.atime(), l.seg, l.off, l.len()))
                        .collect()
                }
            };
            cands.sort_unstable();
            let _m = lock(&self.0.maint);
            for (_, seg, off, len) in cands {
                if self.state_r().live <= target {
                    break;
                }
                let mut tx = self.tx();
                let victim = {
                    let st = self.state_r();
                    if !st.segs.contains_key(&seg) {
                        continue;
                    }
                    let probe = Loc::new(seg, off, len, 0, 0);
                    let key = st.read_key(&probe)?;
                    let n = st.read_raw(&probe, REC_HEADER + 1)?[REC_HEADER];
                    st.is_at(n, &key, seg, off).then_some((n, key))
                };
                if let Some((n, key)) = victim
                    && tx.delete(n, &key)?
                {
                    rep.evicted_entries += 1;
                    rep.evicted_bytes += u64::from(len);
                }
            }
        }
        rep.compacted_segments = self.compact()?;
        rep.disk_bytes_after = self.state_r().disk;
        Ok(rep)
    }

    /// 故障注入（测试用）：打开后每次写入都返回 `StorageFull` 错误，就像磁盘满了；关掉后恢复。
    /// 读不受影响。
    #[doc(hidden)]
    pub fn fail_writes(&self, on: bool) {
        self.0
            .fail_writes
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// 在 tokio 的阻塞线程池里调用存储，例如
    /// `store.blocking(move |s| s.cache().get(&key, max_age)).await`。
    #[cfg(feature = "tokio")]
    pub async fn blocking<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Store) -> R + Send + 'static,
        R: Send + 'static,
    {
        let s = self.clone();
        match tokio::task::spawn_blocking(move || f(&s)).await {
            Ok(r) => r,
            Err(e) => match e.try_into_panic() {
                Ok(p) => std::panic::resume_unwind(p),
                Err(e) => panic!("store task cancelled: {e}"),
            },
        }
    }

    // ---- 内部接口 ----

    fn state_r(&self) -> RwLockReadGuard<'_, State> {
        self.0.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn state_w(&self) -> RwLockWriteGuard<'_, State> {
        self.0.state.write().unwrap_or_else(PoisonError::into_inner)
    }

    fn writer(&self) -> MutexGuard<'_, Writer> {
        lock(&self.0.writer)
    }

    /// 拿到写锁，期间的读写对其他写者是原子的。
    pub(crate) fn tx(&self) -> Tx<'_> {
        Tx {
            store: self,
            w: self.writer(),
        }
    }

    pub(crate) fn get(&self, ns: u8, key: &[u8]) -> Result<Option<(u64, Vec<u8>)>> {
        let st = self.state_r();
        let Some((loc, ts, value)) = st.get_full(ns, key)? else {
            return Ok(None);
        };
        if evictable(ns) {
            loc.touch(now_secs());
        }
        Ok(Some((ts, value)))
    }

    pub(crate) fn contains(&self, ns: u8, key: &[u8]) -> Result<bool> {
        Ok(self.state_r().find(ns, key)?.is_some())
    }

    pub(crate) fn put(&self, ns: u8, key: &[u8], value: &[u8], ts_ms: u64) -> Result<bool> {
        let r = self.tx().put(ns, key, value, ts_ms);
        self.maybe_maintain();
        r
    }

    pub(crate) fn delete(&self, ns: u8, key: &[u8]) -> Result<bool> {
        let r = self.tx().delete(ns, key);
        self.maybe_maintain();
        r
    }

    /// 某个命名空间里满足条件的所有键（要逐条读盘，只用于低频操作）。
    pub(crate) fn keys(&self, ns: u8, mut keep: impl FnMut(&[u8]) -> bool) -> Result<Vec<Vec<u8>>> {
        let st = self.state_r();
        let mut out = Vec::new();
        for loc in st.index.iter().filter(|l| l.ns() == ns) {
            let key = st.read_key(loc)?;
            if keep(&key) {
                out.push(key);
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    pub(crate) fn group(&self, ns: u8, name: &[u8]) -> Option<Group> {
        self.state_r().groups.get(&(ns, Box::from(name))).copied()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) struct Tx<'a> {
    store: &'a Store,
    w: MutexGuard<'a, Writer>,
}
