//! 内存里的索引状态：按键查找、读记录、追加计数、分组计数和压缩时的搬迁。

use super::*;

impl State {
    pub(super) fn read_raw(&self, loc: &Loc, len: usize) -> Result<Vec<u8>> {
        let seg = self
            .segs
            .get(&loc.seg)
            .ok_or_else(|| Error::Corrupt(format!("segment {} missing", loc.seg)))?;
        let mut buf = vec![0; len];
        read_exact_at(&seg.file, &mut buf, u64::from(loc.off))?;
        Ok(buf)
    }

    /// 找到键的位置（已经核对过键）。
    pub(super) fn find(&self, ns: u8, key: &[u8]) -> Result<Option<(Slot, &Loc)>> {
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
    pub(super) fn get_full(&self, ns: u8, key: &[u8]) -> Result<Option<(&Loc, u64, Vec<u8>)>> {
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

    pub(super) fn read_key(&self, loc: &Loc) -> Result<Vec<u8>> {
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

    pub(super) fn account_append(&mut self, seg: u32, len: u64, tomb: bool) {
        let s = self.segs.get_mut(&seg).expect("active segment registered");
        s.total += len;
        if tomb {
            s.tomb += len;
        }
        self.disk += len;
    }

    /// 让键指向新位置（`Some`）或删除（`None`），返回之前是否存在。
    pub(super) fn apply(&mut self, ns: u8, key: &[u8], new: Option<Loc>) -> Result<bool> {
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
    pub(super) fn relocate(&mut self, ns: u8, key: &[u8], seg: u32, off: u32) {
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

    pub(super) fn is_at(&self, ns: u8, key: &[u8], seg: u32, off: u32) -> bool {
        self.index
            .candidate(ns, key)
            .is_some_and(|(_, loc, _)| loc.seg == seg && loc.off == off)
    }
}
