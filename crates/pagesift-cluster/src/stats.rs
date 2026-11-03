//! 统计汇聚：工人发来的统计增量在网关合并，再按作用域下发先验。
//!
//! 合并用 `pagesift_stat::Stat::merge`，满足结合律和交换律，所以到达顺序不影响结果；
//! 重复发送按（来源，种类，序号）去重：每个来源每种统计记一个"以下全部见过"的水位和水位之上
//! 零散见过的序号，乱序到达时水位逐步前推，内存只与乱序窗口有关。

use std::collections::{BTreeSet, HashMap};

use pagesift_stat::Stat;
use pagesift_stat::table::{LoadError, Mode, Scope, StatTable};

use crate::journal::Origin;
use crate::ledger::snapshot::SeenSnap;
use crate::types::{Error, Priors, StatKind, StatsDelta};

/// 一种统计表的合并器（类型擦除，网关按 [`StatKind`] 查找）。
trait Merger: Send {
    fn validate(&self, table: &[u8]) -> Result<(), LoadError>;
    fn merge(&mut self, table: &[u8]) -> Result<(), LoadError>;
    /// 全局层加上 `scope` 层的条目。
    fn select(&self, scope: Scope) -> Vec<u8>;
    fn encode(&self) -> Vec<u8>;
    fn clear(&mut self);
}

struct Typed<T: Stat>(StatTable<T>);

impl<T: Stat + 'static> Merger for Typed<T> {
    fn validate(&self, table: &[u8]) -> Result<(), LoadError> {
        StatTable::<T>::decode(table, Mode::Live).map(drop)
    }

    fn merge(&mut self, table: &[u8]) -> Result<(), LoadError> {
        let t = StatTable::<T>::decode(table, Mode::Live)?;
        self.0.merge(&t);
        Ok(())
    }

    fn select(&self, scope: Scope) -> Vec<u8> {
        let mut out = StatTable::<T>::new(Mode::Live);
        for (k, v) in self.0.iter() {
            if k.scope == Scope::Global || k.scope == scope {
                out.update(*k, |slot| *slot = v.clone());
            }
        }
        out.encode()
    }

    fn encode(&self) -> Vec<u8> {
        self.0.encode()
    }

    fn clear(&mut self) {
        self.0 = StatTable::new(Mode::Live);
    }
}

/// 见过的序号集合：`< below` 的全部见过，`above` 是水位之上零散见过的。
#[derive(Debug, Default, Clone)]
struct SeqSet {
    below: u64,
    above: BTreeSet<u64>,
}

impl SeqSet {
    fn contains(&self, seq: u64) -> bool {
        seq < self.below || self.above.contains(&seq)
    }

    fn insert(&mut self, seq: u64) {
        if seq < self.below {
            return;
        }
        self.above.insert(seq);
        while self.above.remove(&self.below) {
            self.below += 1;
        }
    }
}

/// 网关（或单主机进程）的统计合并中心。
#[derive(Default)]
pub struct StatsHub {
    mergers: HashMap<StatKind, Box<dyn Merger>>,
    seen: HashMap<(Origin, StatKind), SeqSet>,
    /// 回放时种类还没登记（账本先打开、统计后登记）的增量，登记时再合并。
    pending: HashMap<StatKind, Vec<Vec<u8>>>,
}

impl StatsHub {
    /// 登记一种统计表：之后收到这个种类的增量按 `T` 解码合并。
    pub fn register<T: Stat + 'static>(&mut self, kind: StatKind) {
        let m = self
            .mergers
            .entry(kind)
            .or_insert_with(|| Box::new(Typed::<T>(StatTable::new(Mode::Live))));
        for t in self.pending.remove(&kind).unwrap_or_default() {
            // 回放时已经校验过来源；表损坏时丢掉这一份
            let _ = m.merge(&t);
        }
    }

    /// 这个增量是否是第一次见到、且内容合法（不改变状态）。
    pub(crate) fn check(&self, origin: &Origin, d: &StatsDelta) -> Result<bool, Error> {
        if self
            .seen
            .get(&(origin.clone(), d.kind))
            .is_some_and(|s| s.contains(d.seq))
        {
            return Ok(false);
        }
        let m = self
            .mergers
            .get(&d.kind)
            .ok_or_else(|| Error::Rejected(format!("unknown stat kind {}", d.kind.0)))?;
        m.validate(&d.table)
            .map_err(|e| Error::Rejected(format!("bad stats table: {e:?}")))?;
        Ok(true)
    }

    /// 合并并记下序号；重复的返回 `false`，不合并。
    pub(crate) fn apply(&mut self, origin: &Origin, d: &StatsDelta) -> bool {
        let seen = self.seen.entry((origin.clone(), d.kind)).or_default();
        if seen.contains(d.seq) {
            return false;
        }
        match self.mergers.get_mut(&d.kind) {
            Some(m) => {
                if m.merge(&d.table).is_err() {
                    return false;
                }
            }
            None => self
                .pending
                .entry(d.kind)
                .or_default()
                .push(d.table.clone()),
        }
        seen.insert(d.seq);
        true
    }

    /// 快照：每种统计合并后的表（含尚未登记种类的原始增量）与各来源见过的序号。
    pub(crate) fn export(&self) -> (Vec<(StatKind, Vec<u8>)>, Vec<SeenSnap>) {
        let mut tables: Vec<(StatKind, Vec<u8>)> =
            self.mergers.iter().map(|(k, m)| (*k, m.encode())).collect();
        for (k, v) in &self.pending {
            tables.extend(v.iter().map(|t| (*k, t.clone())));
        }
        let seen = self
            .seen
            .iter()
            .map(|((origin, kind), s)| SeenSnap {
                origin: origin.clone(),
                kind: *kind,
                below: s.below,
                above: s.above.iter().copied().collect(),
            })
            .collect();
        (tables, seen)
    }

    pub(crate) fn import(&mut self, tables: &[(StatKind, Vec<u8>)], seen: &[SeenSnap]) {
        for (k, t) in tables {
            match self.mergers.get_mut(k) {
                Some(m) => {
                    let _ = m.merge(t);
                }
                None => self.pending.entry(*k).or_default().push(t.clone()),
            }
        }
        for s in seen {
            let set = SeqSet {
                below: s.below,
                above: s.above.iter().copied().collect(),
            };
            self.seen.insert((s.origin.clone(), s.kind), set);
        }
    }

    /// 清空合并结果与序号（温备从快照重新同步时），已登记的种类保留。
    pub(crate) fn clear(&mut self) {
        self.mergers.values_mut().for_each(|m| m.clear());
        self.seen.clear();
        self.pending.clear();
    }

    /// 某作用域的先验：每种统计一张表（全局层 + 该作用域层）。
    pub fn priors(&self, scope: Scope) -> Priors {
        let mut tables: Vec<(StatKind, Vec<u8>)> = self
            .mergers
            .iter()
            .map(|(k, m)| (*k, m.select(scope)))
            .collect();
        tables.sort_by_key(|(k, _)| *k);
        Priors { tables }
    }

    /// 某种统计合并后的整张表（`/metrics` 汇总、持久化快照用）。
    pub fn table(&self, kind: StatKind) -> Option<Vec<u8>> {
        self.mergers.get(&kind).map(|m| m.encode())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::WorkerId;
    use pagesift_stat::beta::BetaCounts;
    use pagesift_stat::rng::Rng;
    use pagesift_stat::table::{Key, MetricId};

    const KIND: StatKind = StatKind(1);

    fn delta(seq: u64, site: u64, ok: u32, bad: u32) -> StatsDelta {
        let mut t: StatTable<BetaCounts> = StatTable::new(Mode::Live);
        let key = |scope| Key {
            scope,
            metric: MetricId(3),
        };
        for scope in [Scope::Global, Scope::Site(site)] {
            t.update(key(scope), |c| c.observe_weighted(ok as f64, bad as f64));
        }
        StatsDelta {
            kind: KIND,
            seq,
            table: t.encode(),
        }
    }

    /// 增量乱序、重复发送时，合并结果逐字节不变。
    #[test]
    fn out_of_order_and_duplicates_do_not_change_result() {
        let origins: Vec<Origin> = (0..3)
            .map(|i| Origin {
                worker: WorkerId::new(format!("w{i}")),
                boot: 1,
            })
            .collect();
        let mut sent = Vec::new();
        for (i, o) in origins.iter().enumerate() {
            for seq in 0..20u64 {
                sent.push((
                    o.clone(),
                    delta(seq, i as u64 % 2, seq as u32 + 1, i as u32),
                ));
            }
        }
        let mut reference = StatsHub::default();
        reference.register::<BetaCounts>(KIND);
        for (o, d) in &sent {
            assert!(reference.apply(o, d));
        }
        let want = reference.table(KIND).unwrap();

        let mut rng = Rng::seed(7);
        for round in 0..20 {
            let mut shuffled = sent.clone();
            // 约三分之二的增量各重发一次
            let dup: Vec<_> = sent
                .iter()
                .filter(|_| !rng.next_u64().is_multiple_of(3))
                .cloned()
                .collect();
            shuffled.extend(dup);
            for i in (1..shuffled.len()).rev() {
                let j = (rng.next_u64() % (i as u64 + 1)) as usize;
                shuffled.swap(i, j);
            }
            let mut hub = StatsHub::default();
            hub.register::<BetaCounts>(KIND);
            let mut fresh = 0;
            for (o, d) in &shuffled {
                if hub.check(o, d).unwrap() {
                    assert!(hub.apply(o, d));
                    fresh += 1;
                }
            }
            assert_eq!(fresh, sent.len(), "round {round}");
            assert_eq!(hub.table(KIND).unwrap(), want, "round {round}");
        }
        // 先验只含全局层和所请求的站点层
        let p = reference.priors(Scope::Site(1));
        let t: StatTable<BetaCounts> = StatTable::decode(&p.tables[0].1, Mode::Live).unwrap();
        assert_eq!(t.len(), 2);
        assert!(reference.check(&origins[0], &delta(99, 0, 1, 1)).unwrap());
        let mut bad = delta(100, 0, 1, 1);
        bad.kind = StatKind(9);
        assert!(reference.check(&origins[0], &bad).is_err());
    }
}
