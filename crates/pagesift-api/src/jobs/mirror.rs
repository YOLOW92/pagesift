//! 任务记录的复制：多服务器网关把任务存储的每次写入同时交给 [`JobLog`]（账本日志，随日志传送到温备），
//! 温备用 [`apply_op`] 写进自己的存储，接管后照常从存储恢复任务（结束的能查、进行中的从断点继续）。
//!
//! 写入编码成操作：KV 写入/删除、带序号的列表追加、弹出队首、集合加入、清空。按日志顺序重放的结果与原写入
//! 一致；列表追加按序号去重。账本保证每条记录在温备上只应用一次（已同步的温备跳过快照，
//! 重新同步的从快照起应用），弹出队首因此不会重复。webhook 待发队列也走这里：重放后即使多留了
//! 已投过的事件，投递时也按已确认游标跳过，不重发。
//! 写存储与交给日志在同一把锁里完成，日志顺序就是写入顺序；快照拿同一把锁，得到一致的切面。
//!
//! 交给日志失败（账本日志写不进去）时 [`write`] 返回错误：本地已经写进去的部分不撤回，但调用方知道
//! 这组写入没有复制到温备（任务因此失败，见 `jobs::sink`），不会像 0.90 那样只在日志里记一笔警告。
//! 镜像同时记下失败次数（[`JobMirror::failures`]）。
//!
//! 单主机没有 `JobMirror`，任务存储的写法不变。

use std::sync::{Arc, Mutex};

use pagesift_store::{Store, Tasks};

use super::store::{list_key, meta_key};

/// 复制写入的去处（网关：追加进账本日志）。失败时返回原因。
pub trait JobLog: Send + Sync {
    fn append(&self, op: Vec<u8>) -> Result<(), String>;
    /// 一条操作的编码字节上限（超过的写入不复制，例如很大的附件）。默认不限。
    fn max_op(&self) -> usize {
        usize::MAX
    }
}

pub struct JobMirror {
    log: Arc<dyn JobLog>,
    lock: Mutex<()>,
    failures: std::sync::atomic::AtomicU64,
}

// [协议] 本模块自定的操作标签
const KV_PUT: u8 = 1;
const KV_DEL: u8 = 2; // [协议] 同上
const LIST_PUSH: u8 = 3; // [协议] 同上
const LIST_CLEAR: u8 = 4; // [协议] 同上
const SET_INSERT: u8 = 5; // [协议] 同上
const SET_CLEAR: u8 = 6; // [协议] 同上
const LIST_POP: u8 = 7; // [协议] 同上

/// 任务的列表（与 `store.rs` 的布局一致）。
const LISTS: &[&str] = &["pages", "errors", "frontier", "notes"];

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u64).to_le_bytes());
    out.extend_from_slice(b);
}

fn op(tag: u8, a: &[u8], n: Option<u64>, b: Option<&[u8]>) -> Vec<u8> {
    let mut out = vec![tag];
    put_bytes(&mut out, a);
    if let Some(n) = n {
        out.extend_from_slice(&n.to_le_bytes());
    }
    if let Some(b) = b {
        put_bytes(&mut out, b);
    }
    out
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn u64(&mut self) -> Option<u64> {
        let (h, rest) = self.0.split_first_chunk::<{ size_of::<u64>() }>()?;
        self.0 = rest;
        Some(u64::from_le_bytes(*h))
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = usize::try_from(self.u64()?).ok()?;
        let (b, rest) = (self.0.get(..n)?, self.0.get(n..)?);
        self.0 = rest;
        Some(b)
    }
}

/// 记录写入的任务存储视图：每个写操作先写本地存储，再记下对应的复制操作。
pub(crate) struct Writer<'a> {
    t: Tasks<'a>,
    ops: Option<Vec<Vec<u8>>>,
}

impl Writer<'_> {
    pub fn kv_get(&self, key: &str) -> pagesift_store::Result<Option<Vec<u8>>> {
        self.t.kv_get(key)
    }

    pub fn kv_put(&mut self, key: &str, value: &[u8]) -> pagesift_store::Result<()> {
        self.t.kv_put(key, value)?;
        self.note(|| op(KV_PUT, key.as_bytes(), None, Some(value)));
        Ok(())
    }

    pub fn kv_delete(&mut self, key: &str) -> pagesift_store::Result<()> {
        self.t.kv_delete(key)?;
        self.note(|| op(KV_DEL, key.as_bytes(), None, None));
        Ok(())
    }

    pub fn list_push(&mut self, list: &str, item: &[u8]) -> pagesift_store::Result<()> {
        let i = self.t.list_push(list, item)?;
        self.note(|| op(LIST_PUSH, list.as_bytes(), Some(i), Some(item)));
        Ok(())
    }

    pub fn list_clear(&mut self, list: &str) -> pagesift_store::Result<()> {
        self.t.list_clear(list)?;
        self.note(|| op(LIST_CLEAR, list.as_bytes(), None, None));
        Ok(())
    }

    pub fn set_insert(&mut self, set: &str, member: &[u8]) -> pagesift_store::Result<()> {
        self.t.set_insert(set, member)?;
        self.note(|| op(SET_INSERT, set.as_bytes(), None, Some(member)));
        Ok(())
    }

    pub fn list_pop_front(&mut self, list: &str) -> pagesift_store::Result<()> {
        self.t.list_pop_front(list)?;
        self.note(|| op(LIST_POP, list.as_bytes(), None, None));
        Ok(())
    }

    pub fn set_clear(&mut self, set: &str) -> pagesift_store::Result<()> {
        self.t.set_clear(set)?;
        self.note(|| op(SET_CLEAR, set.as_bytes(), None, None));
        Ok(())
    }

    fn note(&mut self, f: impl FnOnce() -> Vec<u8>) {
        if let Some(ops) = &mut self.ops {
            ops.push(f());
        }
    }
}

/// 在任务存储上做一组写入；有 `mirror` 时在它的锁里写，并把这组写入按顺序交给日志。
pub(crate) fn write<R>(
    store: &Store,
    mirror: Option<&JobMirror>,
    f: impl FnOnce(&mut Writer<'_>) -> pagesift_store::Result<R>,
) -> pagesift_store::Result<R> {
    let Some(m) = mirror else {
        return f(&mut Writer {
            t: store.tasks(),
            ops: None,
        });
    };
    let _g = m.lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut w = Writer {
        t: store.tasks(),
        ops: Some(Vec::new()),
    };
    let r = f(&mut w);
    // 失败的一组里已经写进本地的部分也照样复制，两边保持一致；一条交不出去就不再交后面的
    // （温备按序号追加列表，跳过一条后后面的会错位）
    let mut failed = None;
    for o in w.ops.take().unwrap_or_default() {
        if let Err(e) = m.log.append(o) {
            m.failures
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            failed = Some(e);
            break;
        }
    }
    match (r, failed) {
        (Ok(_), Some(e)) => Err(pagesift_store::Error::Io(std::io::Error::other(
            Unreplicated(e),
        ))),
        (r, _) => r,
    }
}

/// 本地已经写进去、只是没能交给复制日志：[`write`] 的这种错误里装的是它（[`unreplicated`] 判断）。
#[derive(Debug)]
struct Unreplicated(String);

impl std::fmt::Display for Unreplicated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "replicating to the standby failed: {}", self.0)
    }
}

impl std::error::Error for Unreplicated {}

/// `e` 是不是"本地写成了、复制失败"（见 [`write`]）。
pub(crate) fn unreplicated(e: &pagesift_store::Error) -> bool {
    matches!(e, pagesift_store::Error::Io(io) if io.get_ref().is_some_and(|x| x.is::<Unreplicated>()))
}

impl JobMirror {
    pub fn new(log: Arc<dyn JobLog>) -> Arc<Self> {
        Arc::new(Self {
            log,
            lock: Mutex::new(()),
            failures: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// 一条复制操作的字节上限（见 [`JobLog::max_op`]）。
    pub fn max_op(&self) -> usize {
        self.log.max_op()
    }

    /// 交给日志失败的写入组数（自进程启动）。
    pub fn failures(&self) -> u64 {
        self.failures.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 存储里全部任务记录，编码成复制操作（快照用）：每个列表、集合先清空再按序号写入，
    /// 温备据此重建，不受它原有内容的影响。
    pub fn dump(&self, store: &Store) -> Vec<Vec<u8>> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let t = store.tasks();
        let mut out = Vec::new();
        for key in t.kv_keys(meta_key("")).unwrap_or_default() {
            let Ok(key) = String::from_utf8(key) else {
                continue;
            };
            let id = &key[meta_key("").len()..];
            if id.contains('/') {
                continue;
            }
            let Ok(Some(meta)) = t.kv_get(&key) else {
                continue;
            };
            out.push(op(KV_PUT, key.as_bytes(), None, Some(&meta)));
            for &what in LISTS {
                let list = list_key(id, what);
                out.push(op(LIST_CLEAR, list.as_bytes(), None, None));
                for (i, item) in t
                    .list_range(&list, 0, usize::MAX)
                    .unwrap_or_default()
                    .into_iter()
                    .enumerate()
                {
                    out.push(op(LIST_PUSH, list.as_bytes(), Some(i as u64), Some(&item)));
                }
            }
            let hook = crate::webhook::outbox_key(id);
            out.push(op(LIST_CLEAR, hook.as_bytes(), None, None));
            for (i, item) in t
                .list_range(&hook, 0, usize::MAX)
                .unwrap_or_default()
                .into_iter()
                .enumerate()
            {
                out.push(op(LIST_PUSH, hook.as_bytes(), Some(i as u64), Some(&item)));
            }
            for k in [crate::webhook::seq_key(id), crate::webhook::ack_key(id)] {
                match t.kv_get(&k) {
                    Ok(Some(v)) => out.push(op(KV_PUT, k.as_bytes(), None, Some(&v))),
                    _ => out.push(op(KV_DEL, k.as_bytes(), None, None)),
                }
            }
            let done = list_key(id, "done");
            out.push(op(SET_CLEAR, done.as_bytes(), None, None));
            for m in t.set_members(&done).unwrap_or_default() {
                out.push(op(SET_INSERT, done.as_bytes(), None, Some(&m)));
            }
        }
        // 附件（内容、到期时刻与授权）：先内容后到期时刻，与在线写入的顺序相同；超过单条上限的不复制
        for (data, exp, access) in crate::attachments::stored(&t) {
            if data.1.len() <= self.max_op() {
                out.push(op(KV_PUT, data.0.as_bytes(), None, Some(&data.1)));
                out.push(op(KV_PUT, exp.0.as_bytes(), None, Some(&exp.1)));
                for (k, v) in access {
                    out.push(op(KV_PUT, k.as_bytes(), None, Some(&v)));
                }
            }
        }
        out
    }
}

/// 温备：把一条复制操作写进本地存储。列表追加按序号去重（已有这个序号就跳过）。
pub fn apply_op(store: &Store, raw: &[u8]) -> pagesift_store::Result<()> {
    let Some((&tag, rest)) = raw.split_first() else {
        return Ok(());
    };
    let mut r = Reader(rest);
    let Some(a) = r.bytes() else {
        return Ok(());
    };
    let t = store.tasks();
    match tag {
        KV_PUT => {
            if let Some(v) = r.bytes() {
                t.kv_put(a, v)?;
            }
        }
        KV_DEL => {
            t.kv_delete(a)?;
        }
        LIST_PUSH => {
            if let (Some(i), Some(v)) = (r.u64(), r.bytes())
                && t.list_next(a) <= i
            {
                t.list_push(a, v)?;
            }
        }
        LIST_CLEAR => {
            t.list_clear(a)?;
        }
        SET_INSERT => {
            if let Some(v) = r.bytes() {
                t.set_insert(a, v)?;
            }
        }
        SET_CLEAR => {
            t.set_clear(a)?;
        }
        LIST_POP => {
            t.list_pop_front(a)?;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Vec2(Mutex<Vec<Vec<u8>>>);

    impl JobLog for Vec2 {
        fn append(&self, op: Vec<u8>) -> Result<(), String> {
            self.0.lock().unwrap().push(op);
            Ok(())
        }
    }

    /// 日志写不进去（第二条起）：写入报错、计数加一，不再交后面的操作。
    struct Broken(Mutex<usize>);

    impl JobLog for Broken {
        fn append(&self, _op: Vec<u8>) -> Result<(), String> {
            let mut n = self.0.lock().unwrap();
            *n += 1;
            if *n > 1 {
                Err("journal full".into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn a_failed_append_is_reported_to_the_writer() {
        let a = tempfile::tempdir().unwrap();
        let src = Store::open_dir(a.path()).unwrap();
        let log = Arc::new(Broken(Mutex::new(0)));
        let m = JobMirror::new(log.clone());
        let r = write(&src, Some(&m), |w| {
            w.list_push(&list_key("j1", "pages"), b"a")?;
            w.list_push(&list_key("j1", "pages"), b"b")?;
            w.set_insert(&list_key("j1", "done"), b"a")
        });
        let e = r.unwrap_err();
        assert!(unreplicated(&e));
        let e = e.to_string();
        assert!(e.contains("journal full"), "{e}");
        assert_eq!(m.failures(), 1);
        assert_eq!(*log.0.lock().unwrap(), 2);
    }

    /// 网关的写入复制到另一个存储后逐项相同；重放两遍、再叠加一次快照，结果不变。
    #[test]
    fn replicated_writes_rebuild_the_same_job_records() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (src, dst) = (
            Store::open_dir(a.path()).unwrap(),
            Store::open_dir(b.path()).unwrap(),
        );
        let log = Arc::new(Vec2(Mutex::default()));
        let m = JobMirror::new(log.clone());
        write(&src, Some(&m), |w| {
            w.kv_put(&meta_key("j1"), b"{\"meta\":1}")?;
            for i in 0..3u8 {
                w.list_push(&list_key("j1", "pages"), &[i])?;
                w.set_insert(&list_key("j1", "done"), &[i])?;
            }
            w.list_push(&list_key("j1", "frontier"), b"u")
        })
        .unwrap();
        let ops = log.0.lock().unwrap().clone();
        for _ in 0..2 {
            for o in &ops {
                apply_op(&dst, o).unwrap();
            }
        }
        for o in m.dump(&src) {
            apply_op(&dst, &o).unwrap();
        }
        let (s, d) = (src.tasks(), dst.tasks());
        assert_eq!(
            d.kv_get(meta_key("j1")).unwrap(),
            s.kv_get(meta_key("j1")).unwrap()
        );
        for &what in LISTS {
            let l = list_key("j1", what);
            assert_eq!(
                d.list_range(&l, 0, usize::MAX).unwrap(),
                s.list_range(&l, 0, usize::MAX).unwrap(),
                "{what}"
            );
        }
        assert_eq!(d.set_len(list_key("j1", "done")), 3);
    }
}
