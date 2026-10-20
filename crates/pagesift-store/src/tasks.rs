//! 爬取任务状态的持久化原语（F-6.4）：KV、追加列表（队列）、集合。
//!
//! 每次调用是一条独立记录，单条写入是原子的，多条之间不是。
//! 例如爬虫应先 `set_insert` 已完成，再 `list_pop_front`，崩溃时最多重复抓一个页面。

use std::time::SystemTime;

use crate::engine::{Tx, group_key, ns};
use crate::{Result, Store, to_ms};

pub struct Tasks<'a> {
    store: &'a Store,
}

impl<'a> Tasks<'a> {
    pub(crate) fn new(store: &'a Store) -> Self {
        Self { store }
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
        let Some((seq, item)) = list_front(&tx, ns::LIST_ITEM, list)? else {
            return Ok(None);
        };
        tx.delete(ns::LIST_ITEM, &group_key(list, &seq.to_be_bytes())?)?;
        drop(tx);
        self.store.finish();
        Ok(Some(item))
    }

    pub fn list_front(&self, list: impl AsRef<[u8]>) -> Result<Option<Vec<u8>>> {
        let tx = self.store.tx();
        Ok(list_front(&tx, ns::LIST_ITEM, list.as_ref())?.map(|(_, v)| v))
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

    /// 清空列表，返回删除的元素个数。
    pub fn list_clear(&self, list: impl AsRef<[u8]>) -> Result<u64> {
        let list = list.as_ref();
        let mut tx = self.store.tx();
        let mut n = 0;
        if let Some(g) = tx.group(ns::LIST_ITEM, list) {
            for seq in g.head..g.next {
                if tx.delete(ns::LIST_ITEM, &group_key(list, &seq.to_be_bytes())?)? {
                    n += 1;
                }
            }
        }
        drop(tx);
        self.store.finish();
        Ok(n)
    }

    // ---- 集合 ----

    /// 返回是否是新加入的成员。
    pub fn set_insert(&self, set: impl AsRef<[u8]>, member: impl AsRef<[u8]>) -> Result<bool> {
        let key = group_key(set.as_ref(), member.as_ref())?;
        let mut tx = self.store.tx();
        if tx.contains(ns::SET_MEMBER, &key)? {
            return Ok(false);
        }
        tx.put(ns::SET_MEMBER, &key, &[], now_ms())?;
        drop(tx);
        self.store.finish();
        Ok(true)
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

fn now_ms() -> u64 {
    to_ms(SystemTime::now())
}

pub(crate) fn list_push(tx: &mut Tx<'_>, n: u8, list: &[u8], item: &[u8], ts: u64) -> Result<u64> {
    let seq = tx.group(n, list).map_or(0, |g| g.next);
    tx.put(n, &group_key(list, &seq.to_be_bytes())?, item, ts)?;
    Ok(seq)
}

/// 头部元素 (序号, 值)。
fn list_front(tx: &Tx<'_>, n: u8, list: &[u8]) -> Result<Option<(u64, Vec<u8>)>> {
    let Some(g) = tx.group(n, list) else {
        return Ok(None);
    };
    for seq in g.head..g.next {
        if let Some((_, v)) = tx.get(n, &group_key(list, &seq.to_be_bytes())?)? {
            if seq != g.head {
                tx.set_head(n, list, seq);
            }
            return Ok(Some((seq, v)));
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
