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

/// 命名空间编号，写进每条记录，不能改。
pub(crate) mod ns {
    pub const CACHE: u8 = 1;
    pub const SNAP_BLOB: u8 = 2;
    pub const SNAP_ITEM: u8 = 3;
    pub const KV: u8 = 4;
    pub const LIST_ITEM: u8 = 5;
    pub const SET_MEMBER: u8 = 6;
    pub const ITEM: u8 = 7;
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

impl State {
    fn read_raw(&self, loc: &Loc, len: usize) -> Result<Vec<u8>> {
        let seg = self
            .segs
            .get(&loc.seg)
            .ok_or_else(|| Error::Corrupt(format!("segment {} missing", loc.seg)))?;
        let mut buf = vec![0; len];
        read_exact_at(&seg.file, &mut buf, u64::from(loc.off))?;
        Ok(buf)
    }

    /// 找到键的位置（已经核对过键）。
    fn find(&self, ns: u8, key: &[u8]) -> Result<Option<(Slot, &Loc)>> {
        match self.index.candidate(ns, key) {
            None => Ok(None),
            Some((slot, loc, true)) => Ok(Some((slot, loc))),
            Some((slot, loc, false)) => {
                let n = record::key_probe_len(key).min(loc.len() as usize);
                let prefix = self.read_raw(loc, n)?;
                Ok(record::key_matches(&prefix, ns, key).then_some((slot, loc)))
            }
        }
    }

    /// 读出完整记录，返回 (位置, 写入时间, 值)。
    fn get_full(&self, ns: u8, key: &[u8]) -> Result<Option<(&Loc, u64, Vec<u8>)>> {
        let Some((_, loc, _)) = self.index.candidate(ns, key) else {
            return Ok(None);
        };
        let mut raw = self.read_raw(loc, loc.len() as usize)?;
        let rec = record::decode(&raw).ok_or_else(|| {
            Error::Corrupt(format!("checksum mismatch at {}:{}", loc.seg, loc.off))
        })?;
        if rec.ns != ns || rec.key != key {
            return Ok(None);
        }
        let ts = rec.ts_ms;
        let start = raw.len() - rec.value.len();
        raw.drain(..start);
        Ok(Some((loc, ts, raw)))
    }

    fn read_key(&self, loc: &Loc) -> Result<Vec<u8>> {
        let head = self.read_raw(loc, REC_HEADER + 12)?;
        let klen = u16::from_le_bytes([head[REC_HEADER + 10], head[REC_HEADER + 11]]) as usize;
        let mut key = vec![0; klen];
        let seg = &self.segs[&loc.seg];
        read_exact_at(
            &seg.file,
            &mut key,
            u64::from(loc.off) + (REC_HEADER + 12) as u64,
        )?;
        Ok(key)
    }

    fn account_append(&mut self, seg: u32, len: u64, tomb: bool) {
        let s = self.segs.get_mut(&seg).expect("active segment registered");
        s.total += len;
        if tomb {
            s.tomb += len;
        }
        self.disk += len;
    }

    /// 让键指向新位置（`Some`）或删除（`None`），返回之前是否存在。
    fn apply(&mut self, ns: u8, key: &[u8], new: Option<Loc>) -> Result<bool> {
        let found = self
            .find(ns, key)?
            .map(|(slot, loc)| (slot, loc.seg, u64::from(loc.len())));
        let existed = found.is_some();
        if let Some((_, seg, len)) = &found {
            if let Some(s) = self.segs.get_mut(seg) {
                s.live -= len;
            }
            self.live -= len;
        }
        let now = new.is_some();
        if let Some(loc) = &new {
            let len = u64::from(loc.len());
            self.segs.get_mut(&loc.seg).expect("segment").live += len;
            self.live += len;
        }
        match (found, new) {
            (Some((slot, ..)), Some(loc)) => {
                *self.index.get_mut(&slot, ns, key).expect("slot") = loc;
            }
            (None, Some(loc)) => self.index.insert_new(ns, key, loc),
            (Some((slot, ..)), None) => {
                self.index.remove(&slot, ns, key);
            }
            (None, None) => {}
        }
        if existed != now {
            self.update_group(ns, key, now);
        }
        Ok(existed)
    }

    fn update_group(&mut self, ns: u8, key: &[u8], added: bool) {
        let is_list = match ns {
            ns::LIST_ITEM | ns::SNAP_ITEM => true,
            ns::SET_MEMBER => false,
            _ => return,
        };
        let Some((name, rest)) = split_group(key) else {
            return;
        };
        let seq = match (is_list, <[u8; 8]>::try_from(rest)) {
            (true, Ok(b)) => u64::from_be_bytes(b),
            _ => 0,
        };
        let gk = (ns, Box::<[u8]>::from(name));
        if added {
            let g = self.groups.entry(gk).or_insert(Group {
                live: 0,
                head: seq,
                next: seq,
            });
            g.live += 1;
            g.head = g.head.min(seq);
            g.next = g.next.max(seq + 1);
        } else if let Some(g) = self.groups.get_mut(&gk) {
            g.live -= 1;
            if g.live == 0 {
                self.groups.remove(&gk);
            } else if seq == g.head {
                g.head = seq + 1;
            }
        }
    }

    /// 压缩时把存活记录挪到新位置，保留访问时间。
    fn relocate(&mut self, ns: u8, key: &[u8], seg: u32, off: u32) {
        let (slot, _, _) = self.index.candidate(ns, key).expect("live record");
        let loc = self.index.get_mut(&slot, ns, key).expect("slot");
        let (old, len) = (loc.seg, u64::from(loc.len()));
        loc.seg = seg;
        loc.off = off;
        if let Some(s) = self.segs.get_mut(&old) {
            s.live -= len;
        }
        self.segs.get_mut(&seg).expect("segment").live += len;
    }

    fn is_at(&self, ns: u8, key: &[u8], seg: u32, off: u32) -> bool {
        self.index
            .candidate(ns, key)
            .is_some_and(|(_, loc, _)| loc.seg == seg && loc.off == off)
    }
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
    last_error: Mutex<Option<String>>,
    open: OpenReport,
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

fn seg_path(dir: &Path, id: u32) -> PathBuf {
    dir.join(format!("{id:08}.seg"))
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

fn create_segment(dir: &Path, id: u32) -> Result<Arc<File>> {
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(seg_path(dir, id))?;
    write_all_at(&f, SEG_MAGIC, 0)?;
    f.sync_all()?;
    sync_dir(dir)?;
    Ok(Arc::new(f))
}

impl Store {
    pub fn open(cfg: Config) -> Result<Store> {
        if !(SEG_HEADER + 64..=2 << 30).contains(&cfg.max_segment_bytes) {
            return Err(Error::Corrupt(
                "max_segment_bytes must be within 72B..=2GiB".into(),
            ));
        }
        let start = Instant::now();
        fs::create_dir_all(&cfg.dir)?;
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
            last_error: Mutex::new(None),
            open: report,
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

    fn append(&self, w: &mut Writer, raw: &[u8], tomb: bool) -> Result<(u32, u32)> {
        let len = raw.len() as u64;
        if w.len > SEG_HEADER && w.len + len > self.0.cfg.max_segment_bytes {
            self.roll(w)?;
        }
        if let Err(e) = write_all_at(&w.file, raw, w.len) {
            let _ = w.file.set_len(w.len);
            return Err(e.into());
        }
        let off = w.len as u32;
        w.len += len;
        match self.0.cfg.sync {
            SyncPolicy::Always => w.file.sync_data()?,
            SyncPolicy::Interval(d) if w.last_sync.elapsed() >= d => {
                w.file.sync_data()?;
                w.last_sync = Instant::now();
            }
            _ => {}
        }
        self.state_w().account_append(w.active, len, tomb);
        Ok((w.active, off))
    }

    /// 换一个新的活动段。旧段总是先 fsync，压缩依赖这一点。
    fn roll(&self, w: &mut Writer) -> Result<()> {
        w.file.sync_data()?;
        let id = w.active + 1;
        let file = create_segment(&self.0.cfg.dir, id)?;
        {
            let mut st = self.state_w();
            st.segs.insert(id, Seg::new(file.clone()));
            st.disk += SEG_HEADER;
        }
        w.active = id;
        w.file = file;
        w.len = SEG_HEADER;
        w.last_sync = Instant::now();
        Ok(())
    }

    fn needs_compact(&self, st: &State) -> bool {
        let c = &self.0.cfg;
        st.segs.len() > 1
            && st.disk >= c.compact_min_bytes
            && (st.disk - st.live) as f64 >= c.compact_ratio * st.disk as f64
    }

    fn maybe_maintain(&self) {
        let inner = &self.0;
        if !inner.cfg.background {
            return;
        }
        let (over, compact) = {
            let st = self.state_r();
            let over = inner.cfg.max_bytes.is_some_and(|m| st.disk > m);
            (over, self.needs_compact(&st))
        };
        if !(over || compact) {
            return;
        }
        let now = to_ms(SystemTime::now());
        if now.saturating_sub(inner.last_maint_ms.load(Ordering::Relaxed)) < 1000
            || inner.busy.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let s = self.clone();
        let spawned = std::thread::Builder::new()
            .name("pagesift-store-gc".into())
            .spawn(move || {
                let r = if over {
                    s.gc().map(drop)
                } else {
                    s.compact_with(s.0.cfg.compact_ratio, false).map(drop)
                };
                if let Err(e) = r {
                    *lock(&s.0.last_error) = Some(e.to_string());
                }
                s.0.last_maint_ms
                    .store(to_ms(SystemTime::now()), Ordering::Relaxed);
                s.0.busy.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            inner.busy.store(false, Ordering::Release);
        }
    }

    fn compact_with(&self, min_ratio: f64, roll_active: bool) -> Result<usize> {
        let _m = lock(&self.0.maint);
        let targets: Vec<u32> = {
            let mut w = self.writer();
            if roll_active {
                let garbage = self.state_r().segs[&w.active].garbage();
                if garbage > 0 {
                    self.roll(&mut w)?;
                }
            }
            let st = self.state_r();
            st.segs
                .iter()
                .filter(|(id, s)| {
                    **id != w.active
                        && s.garbage() > 0
                        && s.garbage() as f64 >= min_ratio * s.total as f64
                })
                .map(|(id, _)| *id)
                .collect()
        };
        drop(_m);
        for &id in &targets {
            self.compact_segment(id)?;
        }
        Ok(targets.len())
    }

    /// 测试用：除活动段外的段编号。
    #[cfg(test)]
    pub(crate) fn sealed_segments(&self) -> Vec<u32> {
        let active = self.writer().active;
        self.state_r()
            .segs
            .keys()
            .copied()
            .filter(|id| *id != active)
            .collect()
    }

    /// 把一个段里存活的记录复制到活动段，然后删掉这个段。
    ///
    /// 删除标记只有在所有更旧的段都没有失效 put 时才能丢掉，否则重启重放时旧值会复活；
    /// 这时如果键仍然是删除状态，就把标记一并复制过去。
    pub(crate) fn compact_segment(&self, id: u32) -> Result<()> {
        let _m = lock(&self.0.maint);
        let (file, len, drop_tombs) = {
            let st = self.state_r();
            let Some(s) = st.segs.get(&id) else {
                return Ok(());
            };
            let drop_tombs = st.segs.range(..id).all(|(_, s)| s.dead_puts() == 0);
            (s.file.clone(), s.total, drop_tombs)
        };
        record::scan(&file, len, |off, raw, rec| -> Result<()> {
            let mut w = self.writer();
            match rec.op {
                Op::Put => {
                    if self.state_r().is_at(rec.ns, rec.key, id, off) {
                        let (seg, noff) = self.append(&mut w, raw, false)?;
                        self.state_w().relocate(rec.ns, rec.key, seg, noff);
                    }
                }
                Op::Del => {
                    if !drop_tombs && self.state_r().find(rec.ns, rec.key)?.is_none() {
                        self.append(&mut w, raw, true)?;
                    }
                }
            }
            Ok(())
        })?;
        // 复制出去的记录落盘之后才能删旧段。
        self.writer().file.sync_data()?;
        {
            let mut st = self.state_w();
            let seg = st.segs.remove(&id).expect("segment");
            debug_assert_eq!(seg.live, 0);
            st.disk -= seg.total;
            st.live -= seg.live;
        }
        fs::remove_file(seg_path(&self.0.cfg.dir, id))?;
        sync_dir(&self.0.cfg.dir)?;
        Ok(())
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) struct Tx<'a> {
    store: &'a Store,
    w: MutexGuard<'a, Writer>,
}

impl Tx<'_> {
    pub fn put(&mut self, ns: u8, key: &[u8], value: &[u8], ts_ms: u64) -> Result<bool> {
        if key.len() > MAX_KEY {
            return Err(Error::KeyTooLarge(key.len()));
        }
        let len = record::encoded_len(key, value);
        if len > MAX_RECORD {
            return Err(Error::ValueTooLarge {
                size: value.len(),
                limit: MAX_RECORD - (len - value.len()),
            });
        }
        let raw = record::encode(ns, Op::Put, ts_ms, key, value);
        let (seg, off) = self.store.append(&mut self.w, &raw, false)?;
        let loc = Loc::new(seg, off, len as u32, ns, now_secs());
        self.store.state_w().apply(ns, key, Some(loc))
    }

    pub fn delete(&mut self, ns: u8, key: &[u8]) -> Result<bool> {
        if !self.store.contains(ns, key)? {
            return Ok(false);
        }
        let raw = record::encode(ns, Op::Del, to_ms(SystemTime::now()), key, &[]);
        self.store.append(&mut self.w, &raw, true)?;
        self.store.state_w().apply(ns, key, None)
    }

    pub fn get(&self, ns: u8, key: &[u8]) -> Result<Option<(u64, Vec<u8>)>> {
        self.store.get(ns, key)
    }

    pub fn contains(&self, ns: u8, key: &[u8]) -> Result<bool> {
        self.store.contains(ns, key)
    }

    pub fn group(&self, ns: u8, name: &[u8]) -> Option<Group> {
        self.store.group(ns, name)
    }

    /// 记下探测到的列表头，下次不用从头探测。
    pub fn set_head(&self, ns: u8, name: &[u8], head: u64) {
        if let Some(g) = self.store.state_w().groups.get_mut(&(ns, Box::from(name))) {
            g.head = head;
        }
    }
}

impl Store {
    /// 用 [`Store::tx`] 写完之后调用，按需触发后台维护。
    pub(crate) fn finish(&self) {
        self.maybe_maintain();
    }
}
