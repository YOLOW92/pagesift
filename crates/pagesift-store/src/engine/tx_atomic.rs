//! 全有或全无（All-or-Nothing）的缓冲写事务：支持事务内读写一致性，失败或丢弃时零落盘。

use std::collections::{BTreeSet, HashMap};
use std::sync::MutexGuard;
use std::time::{Instant, SystemTime};

use super::*;

struct StagedOp {
    ns: u8,
    op: Op,
    ts_ms: u64,
    key: Vec<u8>,
    value: Vec<u8>,
}

type WriteKey = (u8, Vec<u8>);
type PendingWrite = Option<(u64, Vec<u8>)>;

/// 缓冲多条 KV / 分组 / 列表写入并在 `commit` 时一次性顺序落盘的原子写事务。
pub(crate) struct AtomicTx<'a> {
    store: &'a Store,
    w: MutexGuard<'a, Writer>,
    writes: HashMap<WriteKey, PendingWrite>,
    groups: HashMap<(u8, Box<[u8]>), Group>,
    ops: Vec<StagedOp>,
}

impl<'a> AtomicTx<'a> {
    pub(super) fn new(store: &'a Store, w: MutexGuard<'a, Writer>) -> Self {
        Self {
            store,
            w,
            writes: HashMap::new(),
            groups: HashMap::new(),
            ops: Vec::new(),
        }
    }

    pub fn get(&self, ns: u8, key: &[u8]) -> Result<Option<(u64, Vec<u8>)>> {
        if let Some(entry) = self.writes.get(&(ns, key.to_vec())) {
            return Ok(entry.clone());
        }
        self.store.get(ns, key)
    }

    pub fn contains(&self, ns: u8, key: &[u8]) -> Result<bool> {
        if let Some(entry) = self.writes.get(&(ns, key.to_vec())) {
            return Ok(entry.is_some());
        }
        self.store.contains(ns, key)
    }

    pub fn group(&self, ns: u8, name: &[u8]) -> Option<Group> {
        let gk = (ns, Box::<[u8]>::from(name));
        if let Some(g) = self.groups.get(&gk) {
            return (g.live > 0).then_some(*g);
        }
        self.store.group(ns, name)
    }

    pub fn keys_matching(&self, ns: u8, prefix: &[u8]) -> Result<Vec<Vec<u8>>> {
        let mut set: BTreeSet<Vec<u8>> = self
            .store
            .keys(ns, |k| k.starts_with(prefix))?
            .into_iter()
            .collect();
        for ((kns, k), val) in &self.writes {
            if *kns == ns && k.starts_with(prefix) {
                if val.is_some() {
                    set.insert(k.clone());
                } else {
                    set.remove(k);
                }
            }
        }
        Ok(set.into_iter().collect())
    }

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
        let existed = self.contains(ns, key)?;
        if !existed {
            self.stage_group_delta(ns, key, true);
        }
        self.writes
            .insert((ns, key.to_vec()), Some((ts_ms, value.to_vec())));
        self.ops.push(StagedOp {
            ns,
            op: Op::Put,
            ts_ms,
            key: key.to_vec(),
            value: value.to_vec(),
        });
        Ok(existed)
    }

    pub fn delete(&mut self, ns: u8, key: &[u8]) -> Result<bool> {
        if key.len() > MAX_KEY {
            return Err(Error::KeyTooLarge(key.len()));
        }
        if !self.contains(ns, key)? {
            return Ok(false);
        }
        self.stage_group_delta(ns, key, false);
        let ts_ms = to_ms(SystemTime::now());
        self.writes.insert((ns, key.to_vec()), None);
        self.ops.push(StagedOp {
            ns,
            op: Op::Del,
            ts_ms,
            key: key.to_vec(),
            value: Vec::new(),
        });
        Ok(true)
    }

    fn stage_group_delta(&mut self, ns: u8, key: &[u8], added: bool) {
        let is_list = match ns {
            ns::LIST_ITEM | ns::SNAP_ITEM | ns::GRAPH_NODE | ns::GRAPH_EDGE => true,
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
        let entry = self.groups.entry(gk.clone()).or_insert_with(|| {
            self.store.group(ns, name).unwrap_or(Group {
                live: 0,
                head: seq,
                next: seq,
            })
        });
        if added {
            if entry.live == 0 {
                entry.head = seq;
                entry.next = seq + 1;
            } else {
                entry.head = entry.head.min(seq);
                entry.next = entry.next.max(seq + 1);
            }
            entry.live += 1;
        } else if entry.live > 0 {
            entry.live -= 1;
            if entry.live > 0 && seq == entry.head {
                entry.head = seq + 1;
            }
        }
    }

    pub fn commit(mut self) -> Result<()> {
        if self.ops.is_empty() {
            return Ok(());
        }
        if self.store.should_fail_write() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                "injected write failure",
            )
            .into());
        }

        let mut total_bytes = 0usize;
        let mut encoded = Vec::with_capacity(self.ops.len());
        for op in &self.ops {
            let raw = record::encode(op.ns, op.op, op.ts_ms, &op.key, &op.value);
            total_bytes = total_bytes.saturating_add(raw.len());
            encoded.push(raw);
        }

        let mut batch = Vec::with_capacity(total_bytes);
        let mut metas = Vec::with_capacity(self.ops.len());
        for (op, raw) in self.ops.iter().zip(encoded.iter()) {
            let rel_off = batch.len() as u32;
            let rec_len = raw.len() as u32;
            batch.extend_from_slice(raw);
            metas.push((op.ns, op.op, &op.key, rel_off, rec_len, op.op == Op::Del));
        }

        let len_u64 = batch.len() as u64;
        if self.w.len > SEG_HEADER && self.w.len + len_u64 > self.store.0.cfg.max_segment_bytes {
            self.store.roll(&mut self.w)?;
        }

        if let Err(e) = write_all_at(&self.w.file, &batch, self.w.len) {
            let _ = self.w.file.set_len(self.w.len);
            return Err(e.into());
        }

        let sync_res = match self.store.0.cfg.sync {
            SyncPolicy::Always => self.w.file.sync_data(),
            SyncPolicy::Interval(d) if self.w.last_sync.elapsed() >= d => {
                let r = self.w.file.sync_data();
                if r.is_ok() {
                    self.w.last_sync = Instant::now();
                }
                r
            }
            _ => Ok(()),
        };
        if let Err(e) = sync_res {
            let _ = self.w.file.set_len(self.w.len);
            return Err(e.into());
        }

        let seg = self.w.active;
        let base_off = self.w.len as u32;
        let mut st = self.store.state_w();
        for (ns, op, key, rel_off, rec_len, tomb) in metas {
            st.account_append(seg, u64::from(rec_len), tomb);
            match op {
                Op::Put => {
                    st.apply(
                        ns,
                        key,
                        Some(Loc::new(seg, base_off + rel_off, rec_len, ns, now_secs())),
                    )?;
                }
                Op::Del => {
                    st.apply(ns, key, None)?;
                }
            }
        }
        self.w.len += len_u64;
        let store = self.store;
        drop(st);
        drop(self.w);
        store.maybe_maintain();
        Ok(())
    }
}

impl super::tx::TxOps for AtomicTx<'_> {
    fn get(&self, ns: u8, key: &[u8]) -> Result<Option<(u64, Vec<u8>)>> {
        AtomicTx::get(self, ns, key)
    }
    fn contains(&self, ns: u8, key: &[u8]) -> Result<bool> {
        AtomicTx::contains(self, ns, key)
    }
    fn group(&self, ns: u8, name: &[u8]) -> Option<Group> {
        AtomicTx::group(self, ns, name)
    }
    fn put(&mut self, ns: u8, key: &[u8], value: &[u8], ts_ms: u64) -> Result<bool> {
        AtomicTx::put(self, ns, key, value, ts_ms)
    }
    fn delete(&mut self, ns: u8, key: &[u8]) -> Result<bool> {
        AtomicTx::delete(self, ns, key)
    }
}
