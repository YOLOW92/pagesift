//! 写事务：持有写锁的一组写入。

use super::*;

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
