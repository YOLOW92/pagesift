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

    /// Inspect which tenant scopes (e.g. `"acct:<id>"`) already hold persisted data across
    /// Context (`ns::CONTEXT`), Loop Sync (`ns::SYNC_STATE`), or DataGraph (`ns::GRAPH_*`)
    /// in a single index pass without reading values.
    pub fn existing_tenants(
        &self,
        tenants: &[String],
    ) -> Result<std::collections::HashSet<String>> {
        let mut found = std::collections::HashSet::new();
        if tenants.is_empty() {
            return Ok(found);
        }
        let mut plain_targets: Vec<(&str, Vec<u8>)> = Vec::with_capacity(tenants.len());
        let mut graph_targets: Vec<(&str, Vec<u8>)> = Vec::with_capacity(tenants.len());
        let mut seen = std::collections::HashSet::with_capacity(tenants.len());
        for t in tenants {
            if t.is_empty() || !seen.insert(t.as_str()) {
                continue;
            }
            plain_targets.push((t.as_str(), format!("t:{t}|").into_bytes()));
            graph_targets.push((
                t.as_str(),
                crate::graph::encode_tenant_prefix(t).into_bytes(),
            ));
        }
        if plain_targets.is_empty() {
            return Ok(found);
        }
        let target_count = plain_targets.len();
        let st = self.state_r();
        for loc in st.index.iter() {
            if found.len() == target_count {
                break;
            }
            let (targets, grouped) = match loc.ns() {
                ns::CONTEXT | ns::SYNC_STATE => (&plain_targets, false),
                ns::GRAPH_NODE | ns::GRAPH_EDGE => (&graph_targets, true),
                ns::GRAPH_FACT | ns::GRAPH_JOB => (&graph_targets, false),
                _ => continue,
            };
            let key = st.read_key(loc)?;
            let logical = if grouped && key.len() >= 2 {
                &key[2..]
            } else {
                &key[..]
            };
            for (tenant, prefix) in targets {
                if !found.contains(*tenant)
                    && (logical.starts_with(prefix) || key.starts_with(prefix))
                {
                    found.insert((*tenant).to_owned());
                    break;
                }
            }
        }
        Ok(found)
    }
}
