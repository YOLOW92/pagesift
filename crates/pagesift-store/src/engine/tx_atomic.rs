//! 全有或全无（All-or-Nothing）的缓冲写事务：支持事务内读写一致性，失败或崩溃恢复时零部分提交。

use std::collections::{BTreeSet, HashMap};
use std::io;
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

/// 缓冲多条 KV / 分组 / 列表写入并在 `commit` 时追加 `TxCommit` 标记的崩溃原子写事务。
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
        if ns >= 16 {
            return Err(Error::Corrupt(format!("unknown namespace {ns}")));
        }
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
            op: Op::TxPut,
            ts_ms,
            key: key.to_vec(),
            value: value.to_vec(),
        });
        Ok(existed)
    }

    pub fn delete(&mut self, ns: u8, key: &[u8]) -> Result<bool> {
        if ns >= 16 {
            return Err(Error::Corrupt(format!("unknown namespace {ns}")));
        }
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
            op: Op::TxDel,
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
            return Err(
                io::Error::new(io::ErrorKind::StorageFull, "injected write failure").into(),
            );
        }

        let op_count = u32::try_from(self.ops.len()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "atomic transaction op count overflow",
            )
        })?;

        let mut total_bytes = record::tx_commit_encoded_len();
        for op in &self.ops {
            total_bytes = total_bytes
                .checked_add(record::encoded_len(&op.key, &op.value))
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "atomic transaction size overflow",
                    )
                })?;
        }
        let len_u64 = u64::try_from(total_bytes).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "atomic transaction size overflow",
            )
        })?;

        let needs_roll = self.w.len > SEG_HEADER
            && self.w.len.saturating_add(len_u64) > self.store.0.cfg.max_segment_bytes;
        let projected_base = if needs_roll { SEG_HEADER } else { self.w.len };
        let projected_end = projected_base
            .checked_add(len_u64)
            .filter(|&e| e <= u64::from(u32::MAX));
        if projected_end.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "atomic transaction exceeds segment 32-bit offset limit",
            )
            .into());
        }

        let mut batch = Vec::with_capacity(total_bytes);
        let mut metas = Vec::with_capacity(self.ops.len());
        let mut hasher = blake3::Hasher::new();
        hasher.update(record::TX_HASH_DOMAIN);
        let tx_id = self.store.0.next_tx_id.fetch_add(1, Ordering::Relaxed);

        for op in &self.ops {
            let rel_off = batch.len() as u32;
            record::encode_into(&mut batch, op.ns, op.op, op.ts_ms, &op.key, &op.value);
            let rec_slice = &batch[rel_off as usize..];
            hasher.update(rec_slice);
            let rec_len = rec_slice.len() as u32;
            metas.push((op.ns, op.op, &op.key, rel_off, rec_len, op.op.is_del()));
        }

        let commit_rel = batch.len();
        let commit_ts_ms = to_ms(SystemTime::now());
        record::encode_tx_commit(&mut batch, tx_id, commit_ts_ms, op_count, hasher);
        let commit_len = (batch.len() - commit_rel) as u32;
        debug_assert_eq!(batch.len(), total_bytes);

        if needs_roll {
            self.store.roll(&mut self.w)?;
        }

        #[cfg(test)]
        if let Some(limit) = self.store.take_crash_after_tx_bytes() {
            let n = limit.min(batch.len());
            if n > 0 {
                write_all_at(&self.w.file, &batch[..n], self.w.len)?;
            }
            return Err(
                io::Error::new(io::ErrorKind::Interrupted, "simulated crash mid-tx").into(),
            );
        }

        if let Err(e) = write_all_at(&self.w.file, &batch, self.w.len) {
            let _ = self.w.file.set_len(self.w.len);
            return Err(e.into());
        }

        let sync_res = if self.store.should_fail_sync() {
            Err(io::Error::other("injected sync failure"))
        } else {
            match self.store.0.cfg.sync {
                SyncPolicy::Always => self.w.file.sync_data(),
                SyncPolicy::Interval(d) if self.w.last_sync.elapsed() >= d => {
                    let r = self.w.file.sync_data();
                    if r.is_ok() {
                        self.w.last_sync = Instant::now();
                    }
                    r
                }
                _ => Ok(()),
            }
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
            if op.is_put() {
                st.apply(
                    ns,
                    key,
                    Some(Loc::new(seg, base_off + rel_off, rec_len, ns, now_secs())),
                )?;
            } else {
                st.apply(ns, key, None)?;
            }
        }
        st.account_append(seg, u64::from(commit_len), true);
        self.w.len += len_u64;
        let store = self.store;
        drop(st);
        drop(self.w);
        store.maybe_maintain();
        Ok(())
    }
}

impl Store {
    #[cfg(not(test))]
    fn should_fail_sync(&self) -> bool {
        false
    }

    #[cfg(test)]
    pub(crate) fn set_fail_sync(&self, on: bool) {
        self.0.fail_sync.store(on, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn should_fail_sync(&self) -> bool {
        self.0.fail_sync.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn set_crash_after_tx_bytes(&self, n: Option<usize>) {
        let val = n.map_or(-1, |v| i64::try_from(v).unwrap_or(i64::MAX));
        self.0.crash_after_tx_bytes.store(val, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn take_crash_after_tx_bytes(&self) -> Option<usize> {
        let prev = self.0.crash_after_tx_bytes.swap(-1, Ordering::Relaxed);
        (prev >= 0).then_some(prev as usize)
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
