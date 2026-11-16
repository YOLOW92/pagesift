//! 爬取任务状态的持久化原语（F-6.4）：KV、追加列表（队列）、集合。
//!
//! [`Tasks`] 上的单个操作各自原子提交；需要把多条写入合并为全有或全无事务时使用 [`Tasks::atomic`]。

use std::time::SystemTime;

use crate::engine::{AtomicTx, TxOps, group_key, ns};
use crate::{Result, Store, to_ms};

pub struct Tasks<'a> {
    store: &'a Store,
}

/// 缓冲多项任务状态写入的全有或全无事务视图，由 [`Tasks::atomic`] 提交。
pub struct TasksTx<'a> {
    tx: AtomicTx<'a>,
}

impl<'a> Tasks<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// 打开一个可手动提交或丢弃回滚的任务事务视图（给跨模块镜像封装使用）。
    pub fn begin_tx(&self) -> TasksTx<'a> {
        TasksTx {
            tx: self.store.atomic_tx(),
        }
    }

    /// 在单个全有或全无的 [`AtomicTx`] 内执行一组任务存储读写，任意写入失败时不提交任何改动。
    pub fn atomic<R>(&self, f: impl FnOnce(&mut TasksTx<'_>) -> Result<R>) -> Result<R> {
        let mut batch = self.begin_tx();
        let out = f(&mut batch)?;
        batch.commit()?;
        Ok(out)
    }

    // ---- KV ----

    pub fn kv_get(&self, key: impl AsRef<[u8]>) -> Result<Option<Vec<u8>>> {
        Ok(self.store.get(ns::KV, key.as_ref())?.map(|(_, v)| v))
    }

    pub fn kv_put(&self, key: impl AsRef<[u8]>, value: &[u8]) -> Result<()> {
        self.store
            .put(ns::KV, key.as_ref(), value, now_ms())
            .map(drop)
    }

    /// 返回键之前是否存在。
    pub fn kv_delete(&self, key: impl AsRef<[u8]>) -> Result<bool> {
        self.store.delete(ns::KV, key.as_ref())
    }

    /// 以 `prefix` 开头的所有键，按字节序排序。要逐条读盘，适合启动时列出未完成任务这类低频操作。
    pub fn kv_keys(&self, prefix: impl AsRef<[u8]>) -> Result<Vec<Vec<u8>>> {
        let p = prefix.as_ref();
        self.store.keys(ns::KV, |k| k.starts_with(p))
    }

    // ---- 列表 ----

    /// 追加到列表尾部，返回序号。
    pub fn list_push(&self, list: impl AsRef<[u8]>, item: &[u8]) -> Result<u64> {
        let r = list_push(
            &mut self.store.tx(),
            ns::LIST_ITEM,
            list.as_ref(),
            item,
            now_ms(),
        );
        self.store.finish();
        r
    }

    /// 弹出列表头部。
    pub fn list_pop_front(&self, list: impl AsRef<[u8]>) -> Result<Option<Vec<u8>>> {
        let list = list.as_ref();
        let mut tx = self.store.tx();
        let Some((seq, head, item)) = list_front_tx(&tx, ns::LIST_ITEM, list)? else {
            return Ok(None);
        };
        if seq != head {
            tx.set_head(ns::LIST_ITEM, list, seq);
        }
        tx.delete(ns::LIST_ITEM, &group_key(list, &seq.to_be_bytes())?)?;
        drop(tx);
        self.store.finish();
        Ok(Some(item))
    }

    pub fn list_front(&self, list: impl AsRef<[u8]>) -> Result<Option<Vec<u8>>> {
        let tx = self.store.tx();
        let Some((seq, head, item)) = list_front_tx(&tx, ns::LIST_ITEM, list.as_ref())? else {
            return Ok(None);
        };
        if seq != head {
            tx.set_head(ns::LIST_ITEM, list.as_ref(), seq);
        }
        Ok(Some(item))
    }

    pub fn list_len(&self, list: impl AsRef<[u8]>) -> u64 {
        self.store
            .group(ns::LIST_ITEM, list.as_ref())
            .map_or(0, |g| g.live)
    }

    /// 从头部开始最多 `limit` 个元素。
    pub fn list_items(&self, list: impl AsRef<[u8]>, limit: usize) -> Result<Vec<Vec<u8>>> {
        Ok(list_scan(self.store, ns::LIST_ITEM, list.as_ref(), limit)?
            .into_iter()
            .map(|(_, _, v)| v)
            .collect())
    }

    /// 序号恰好为 `seq` 的元素；已删除或还没有时为 `None`。
    /// （[`Self::list_range`] 遇到已删除的序号会跳到后面还在的元素，按下标逐条读时用这个。）
    pub fn list_get(&self, list: impl AsRef<[u8]>, seq: u64) -> Result<Option<Vec<u8>>> {
        let list = list.as_ref();
        Ok(self
            .store
            .get(ns::LIST_ITEM, &group_key(list, &seq.to_be_bytes())?)?
            .map(|(_, v)| v))
    }

    /// 从序号 `from` 开始最多 `limit` 个元素，跳过已弹出的。只追加、不弹出的列表里序号就是下标，
    /// 可以用来分页。
    pub fn list_range(
        &self,
        list: impl AsRef<[u8]>,
        from: u64,
        limit: usize,
    ) -> Result<Vec<Vec<u8>>> {
        let list = list.as_ref();
        let mut out = Vec::new();
        let Some(g) = self.store.group(ns::LIST_ITEM, list) else {
            return Ok(out);
        };
        for seq in from.max(g.head)..g.next {
            if out.len() >= limit {
                break;
            }
            if let Some((_, v)) = self
                .store
                .get(ns::LIST_ITEM, &group_key(list, &seq.to_be_bytes())?)?
            {
                out.push(v);
            }
        }
        Ok(out)
    }

    /// 清空列表，返回删除的元素个数。
    pub fn list_clear(&self, list: impl AsRef<[u8]>) -> Result<u64> {
        let mut tx = self.store.tx();
        let n = list_clear_tx(&mut tx, ns::LIST_ITEM, list.as_ref())?;
        drop(tx);
        self.store.finish();
        Ok(n)
    }

    /// 列表下一个元素的序号（追加过的总数）；只截断头部时不变，列表删空后从 0 重新计数。
    pub fn list_next(&self, list: impl AsRef<[u8]>) -> u64 {
        self.store
            .group(ns::LIST_ITEM, list.as_ref())
            .map_or(0, |g| g.next)
    }

    /// 截断头部：删掉序号小于 `before` 的元素，返回删除个数。
    pub fn list_trim(&self, list: impl AsRef<[u8]>, before: u64) -> Result<u64> {
        let list = list.as_ref();
        let mut tx = self.store.tx();
        let mut n = 0;
        if let Some(g) = tx.group(ns::LIST_ITEM, list) {
            for seq in g.head..before.min(g.next) {
                if tx.delete(ns::LIST_ITEM, &group_key(list, &seq.to_be_bytes())?)? {
                    n += 1;
                }
            }
            if before > g.head && before < g.next {
                tx.set_head(ns::LIST_ITEM, list, before);
            }
        }
        drop(tx);
        self.store.finish();
        Ok(n)
    }

    // ---- 集合 ----

    /// 返回是否是新加入的成员。
    pub fn set_insert(&self, set: impl AsRef<[u8]>, member: impl AsRef<[u8]>) -> Result<bool> {
        let mut tx = self.store.tx();
        let inserted = set_insert_tx(
            &mut tx,
            ns::SET_MEMBER,
            set.as_ref(),
            member.as_ref(),
            now_ms(),
        )?;
        drop(tx);
        if inserted {
            self.store.finish();
        }
        Ok(inserted)
    }

    pub fn set_contains(&self, set: impl AsRef<[u8]>, member: impl AsRef<[u8]>) -> Result<bool> {
        let key = group_key(set.as_ref(), member.as_ref())?;
        self.store.contains(ns::SET_MEMBER, &key)
    }

    pub fn set_remove(&self, set: impl AsRef<[u8]>, member: impl AsRef<[u8]>) -> Result<bool> {
        let key = group_key(set.as_ref(), member.as_ref())?;
        self.store.delete(ns::SET_MEMBER, &key)
    }

    pub fn set_len(&self, set: impl AsRef<[u8]>) -> u64 {
        self.store
            .group(ns::SET_MEMBER, set.as_ref())
            .map_or(0, |g| g.live)
    }

    /// 所有成员（逐条读盘，低频使用）。
    pub fn set_members(&self, set: impl AsRef<[u8]>) -> Result<Vec<Vec<u8>>> {
        let prefix = group_key(set.as_ref(), &[])?;
        Ok(self
            .store
            .keys(ns::SET_MEMBER, |k| k.starts_with(&prefix))?
            .into_iter()
            .map(|k| k[prefix.len()..].to_vec())
            .collect())
    }

    /// 清空集合，返回删除的成员个数。
    pub fn set_clear(&self, set: impl AsRef<[u8]>) -> Result<u64> {
        let set = set.as_ref();
        let mut n = 0;
        for m in self.set_members(set)? {
            if self.set_remove(set, &m)? {
                n += 1;
            }
        }
        Ok(n)
    }
}

impl<'a> TasksTx<'a> {
    /// 读取本事务或已提交视图里的 KV 值。
    pub fn kv_get(&self, key: impl AsRef<[u8]>) -> Result<Option<Vec<u8>>> {
        Ok(self.tx.get(ns::KV, key.as_ref())?.map(|(_, v)| v))
    }

    /// 在事务内写入 KV 键值。
    pub fn kv_put(&mut self, key: impl AsRef<[u8]>, value: &[u8]) -> Result<()> {
        self.tx.put(ns::KV, key.as_ref(), value, now_ms()).map(drop)
    }

    /// 在事务内删除 KV 键，返回之前是否存在。
    pub fn kv_delete(&mut self, key: impl AsRef<[u8]>) -> Result<bool> {
        self.tx.delete(ns::KV, key.as_ref())
    }

    /// 在事务内向列表尾部追加元素，返回序号。
    pub fn list_push(&mut self, list: impl AsRef<[u8]>, item: &[u8]) -> Result<u64> {
        list_push(&mut self.tx, ns::LIST_ITEM, list.as_ref(), item, now_ms())
    }

    /// 在事务内弹出列表头部元素。
    pub fn list_pop_front(&mut self, list: impl AsRef<[u8]>) -> Result<Option<Vec<u8>>> {
        let list = list.as_ref();
        let Some((seq, _, item)) = list_front_tx(&self.tx, ns::LIST_ITEM, list)? else {
            return Ok(None);
        };
        self.tx
            .delete(ns::LIST_ITEM, &group_key(list, &seq.to_be_bytes())?)?;
        Ok(Some(item))
    }

    /// 在事务内清空列表，返回删除元素个数。
    pub fn list_clear(&mut self, list: impl AsRef<[u8]>) -> Result<u64> {
        list_clear_tx(&mut self.tx, ns::LIST_ITEM, list.as_ref())
    }

    /// 在事务内向集合插入成员，返回是否新加入。
    pub fn set_insert(&mut self, set: impl AsRef<[u8]>, member: impl AsRef<[u8]>) -> Result<bool> {
        set_insert_tx(
            &mut self.tx,
            ns::SET_MEMBER,
            set.as_ref(),
            member.as_ref(),
            now_ms(),
        )
    }

    /// 在事务内清空集合，返回删除的成员个数。
    pub fn set_clear(&mut self, set: impl AsRef<[u8]>) -> Result<u64> {
        let prefix = group_key(set.as_ref(), &[])?;
        let mut n = 0;
        for key in self.tx.keys_matching(ns::SET_MEMBER, &prefix)? {
            if self.tx.delete(ns::SET_MEMBER, &key)? {
                n += 1;
            }
        }
        Ok(n)
    }

    /// 原子提交缓冲的全部写入与删除。
    pub fn commit(self) -> Result<()> {
        self.tx.commit()
    }
}

fn now_ms() -> u64 {
    to_ms(SystemTime::now())
}

pub(crate) fn list_push<T: TxOps + ?Sized>(
    tx: &mut T,
    n: u8,
    list: &[u8],
    item: &[u8],
    ts: u64,
) -> Result<u64> {
    let seq = tx.group(n, list).map_or(0, |g| g.next);
    tx.put(n, &group_key(list, &seq.to_be_bytes())?, item, ts)?;
    Ok(seq)
}

fn list_clear_tx<T: TxOps + ?Sized>(tx: &mut T, n: u8, list: &[u8]) -> Result<u64> {
    let mut count = 0;
    if let Some(g) = tx.group(n, list) {
        for seq in g.head..g.next {
            if tx.delete(n, &group_key(list, &seq.to_be_bytes())?)? {
                count += 1;
            }
        }
    }
    Ok(count)
}

fn set_insert_tx<T: TxOps + ?Sized>(
    tx: &mut T,
    n: u8,
    set: &[u8],
    member: &[u8],
    ts: u64,
) -> Result<bool> {
    let key = group_key(set, member)?;
    if tx.contains(n, &key)? {
        return Ok(false);
    }
    tx.put(n, &key, &[], ts)?;
    Ok(true)
}

/// 头部元素 `(序号, 原记录 head, 值)`。
fn list_front_tx<T: TxOps + ?Sized>(
    tx: &T,
    n: u8,
    list: &[u8],
) -> Result<Option<(u64, u64, Vec<u8>)>> {
    let Some(g) = tx.group(n, list) else {
        return Ok(None);
    };
    for seq in g.head..g.next {
        if let Some((_, v)) = tx.get(n, &group_key(list, &seq.to_be_bytes())?)? {
            return Ok(Some((seq, g.head, v)));
        }
    }
    Ok(None)
}

/// 从头部开始顺序读，返回 (序号, 写入时间, 值)。
pub(crate) fn list_scan(
    store: &Store,
    n: u8,
    list: &[u8],
    limit: usize,
) -> Result<Vec<(u64, u64, Vec<u8>)>> {
    let mut out = Vec::new();
    let Some(g) = store.group(n, list) else {
        return Ok(out);
    };
    for seq in g.head..g.next {
        if out.len() >= limit || out.len() as u64 >= g.live {
            break;
        }
        if let Some((ts, v)) = store.get(n, &group_key(list, &seq.to_be_bytes())?)? {
            out.push((seq, ts, v));
        }
    }
    Ok(out)
}
