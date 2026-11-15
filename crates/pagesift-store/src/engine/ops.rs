//! Internal transaction constructors and namespace operations for `Store`.

use super::*;

impl Store {
    /// 拿到写锁，期间的读写对其他写者是原子的。
    pub(crate) fn tx(&self) -> Tx<'_> {
        Tx {
            store: self,
            w: self.writer(),
        }
    }

    /// 缓冲所有写入并仅在 `commit` 时一次性落盘与更新索引的原子写事务（C06）。
    pub(crate) fn atomic_tx(&self) -> AtomicTx<'_> {
        AtomicTx::new(self, self.writer())
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
