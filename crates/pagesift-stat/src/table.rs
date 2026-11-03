//! 按（作用域，指标）索引的统计表、持久化格式、冻结模式与先验包。

use std::collections::HashMap;

use crate::{DecodeError, Reader, Stat, Writer};

/// 统计的作用域：全局、站点（注册域的哈希）、路径族（族模板的哈希）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    Global,
    Site(u64),
    Family(u64),
}

/// 指标编号。由使用方定义常量；编号一经发布不改含义，换含义就换编号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MetricId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    pub scope: Scope,
    pub metric: MetricId,
}

/// [协议] 存储格式的魔数 "PSST" 与版本；版本不兼容时整表丢弃，从先验包重来
const MAGIC: [u8; 4] = *b"PSST";
/// [协议] 当前格式版本；改动编码布局时加一
pub const FORMAT_VERSION: u32 = 1;

/// 读表失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// 不是统计表，或格式版本不同：调用方应整体丢弃。
    Incompatible,
    Corrupt(DecodeError),
}

impl From<DecodeError> for LoadError {
    fn from(e: DecodeError) -> Self {
        Self::Corrupt(e)
    }
}

/// 统计表的工作方式。所有新旧对比测速都必须在冻结模式下进行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// 读写：观测会更新统计并持久化。
    #[default]
    Live,
    /// 只读一份快照，不写入（`--stats frozen=<快照>`）。
    Frozen,
    /// 从先验包开始，不读已有的学习状态（`--stats fresh`）。
    Fresh,
}

/// 同一种统计量的表。
#[derive(Debug, Clone, Default)]
pub struct StatTable<T: Stat> {
    entries: HashMap<Key, T>,
    mode: Mode,
}

impl<T: Stat> StatTable<T> {
    pub fn new(mode: Mode) -> Self {
        Self {
            entries: HashMap::new(),
            mode,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: &Key) -> Option<&T> {
        self.entries.get(key)
    }

    /// 写入观测。冻结模式下什么都不做。
    pub fn update(&mut self, key: Key, f: impl FnOnce(&mut T)) {
        if self.mode != Mode::Frozen {
            f(self.entries.entry(key).or_default());
        }
    }

    /// 合并另一张表（另一个工人或另一台机器的增量）。冻结模式下不合并。
    pub fn merge(&mut self, other: &Self) {
        if self.mode == Mode::Frozen {
            return;
        }
        for (k, v) in &other.entries {
            self.entries.entry(*k).or_default().merge(v);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Key, &T)> {
        self.entries.iter()
    }

    /// 表超出预算时淘汰信息量最低的条目（样本少、后验接近上层的先走）。
    /// `info` 给每个条目打分，越大越值得留；全局作用域的条目总是保留。
    pub fn retain_most_informative(&mut self, budget: usize, info: impl Fn(&Key, &T) -> f64) {
        if self.entries.len() <= budget {
            return;
        }
        let mut scored: Vec<(f64, Key)> = self
            .entries
            .iter()
            .filter(|(k, _)| k.scope != Scope::Global)
            .map(|(k, v)| (info(k, v), *k))
            .collect();
        scored.sort_by(|a, b| a.0.total_cmp(&b.0));
        let excess = self.entries.len() - budget;
        for (_, k) in scored.into_iter().take(excess) {
            self.entries.remove(&k);
        }
    }

    /// 编码：魔数、版本、条目数，再按键排序写出（同一内容编码结果相同，便于做快照哈希）。
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        for b in MAGIC {
            w.u8(b);
        }
        w.u32(FORMAT_VERSION);
        let mut keys: Vec<&Key> = self.entries.keys().collect();
        keys.sort();
        w.u64(keys.len() as u64);
        for k in keys {
            let (tag, id) = match k.scope {
                Scope::Global => (0, 0),
                Scope::Site(h) => (1, h),
                // [协议] 作用域标签：0 全局、1 站点、2 路径族
                Scope::Family(h) => (2, h),
            };
            w.u8(tag);
            w.u64(id);
            w.u32(k.metric.0);
            self.entries[k].encode(&mut w);
        }
        w.into_inner()
    }

    pub fn decode(buf: &[u8], mode: Mode) -> Result<Self, LoadError> {
        let mut r = Reader::new(buf);
        let mut magic = [0u8; MAGIC.len()];
        for b in &mut magic {
            *b = r.u8().map_err(|_| LoadError::Incompatible)?;
        }
        if magic != MAGIC || r.u32().map_err(|_| LoadError::Incompatible)? != FORMAT_VERSION {
            return Err(LoadError::Incompatible);
        }
        let n = r.u64()?;
        let mut entries = HashMap::new();
        for _ in 0..n {
            let scope = match (r.u8()?, r.u64()?) {
                (0, _) => Scope::Global,
                (1, h) => Scope::Site(h),
                (2, h) => Scope::Family(h),
                _ => return Err(LoadError::Corrupt(DecodeError::Invalid("scope tag"))),
            };
            let metric = MetricId(r.u32()?);
            entries.insert(Key { scope, metric }, T::decode(&mut r)?);
        }
        if !r.is_empty() {
            return Err(LoadError::Corrupt(DecodeError::Invalid("trailing bytes")));
        }
        Ok(Self { entries, mode })
    }

    /// 先验包：只保留全局作用域的条目（基准集上学到的全局统计，随版本发布）。
    pub fn prior_pack(&self) -> Vec<u8> {
        let global = Self {
            entries: self
                .entries
                .iter()
                .filter(|(k, _)| k.scope == Scope::Global)
                .map(|(k, v)| (*k, v.clone()))
                .collect(),
            mode: Mode::Live,
        };
        global.encode()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beta::BetaCounts;

    #[test]
    fn round_trip_frozen_and_version_mismatch() {
        let key = |scope| Key {
            scope,
            metric: MetricId(7),
        };
        let mut t: StatTable<BetaCounts> = StatTable::new(Mode::Live);
        t.update(key(Scope::Global), |c| c.observe(true));
        t.update(key(Scope::Site(42)), |c| c.observe(false));
        t.update(key(Scope::Family(9)), |c| c.observe_weighted(3.0, 1.0));
        let buf = t.encode();
        let back: StatTable<BetaCounts> = StatTable::decode(&buf, Mode::Frozen).unwrap();
        assert_eq!(back.encode(), buf);
        let mut frozen = back;
        frozen.update(key(Scope::Global), |c| c.observe(true));
        frozen.merge(&t);
        assert_eq!(frozen.encode(), buf, "frozen table must not change");
        let mut bumped = buf.clone();
        bumped[4] ^= 1;
        assert_eq!(
            StatTable::<BetaCounts>::decode(&bumped, Mode::Live).unwrap_err(),
            LoadError::Incompatible
        );
        let pack: StatTable<BetaCounts> = StatTable::decode(&t.prior_pack(), Mode::Fresh).unwrap();
        assert_eq!(pack.len(), 1);
        t.retain_most_informative(2, |_, c| c.trials());
        assert!(t.get(&key(Scope::Site(42))).is_none() && t.len() == 2);
    }
}
