//! 账本里多服务器才用到的部分：成员表与站点交接、统计汇聚、
//! 温备一侧的日志回放。单主机只用到统计合并。

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use pagesift_stat::Stat;

use super::{Ledger, Members};
use crate::hrw::HostMap;
use crate::journal::{Entry, Journal, Origin, Record};
use crate::types::lock;
use crate::types::{
    Deadline, Error, Fence, Owner, Priors, Scope, SiteKey, SiteState, StatKind, StatsDelta,
    WorkerId,
};

impl Ledger {
    // ---- 成员与站点归属 ----

    pub fn host_map(&self) -> Arc<HostMap> {
        Arc::clone(&lock(&self.members).map)
    }

    pub fn owner_of(&self, site: SiteKey) -> Owner {
        self.host_map().owner_of(site, self.local)
    }

    /// 工人加入（或更新权重、往返上界），返回新的成员表。
    pub fn join(&self, worker: &WorkerId, weight: f64, rtt_hi: Duration) -> Arc<HostMap> {
        let mut m = lock(&self.members);
        m.rtt.insert(worker.clone(), rtt_hi);
        m.weights.insert(worker.clone(), weight);
        // 第一次拉取之前，容量取握手时报告的权重
        m.capacity.entry(worker.clone()).or_insert(weight);
        self.remap(&mut m)
    }

    /// 工人离开（断开或被判定失联），返回新的成员表。
    pub fn leave(&self, worker: &WorkerId) -> Arc<HostMap> {
        let mut m = lock(&self.members);
        m.capacity.remove(worker);
        if m.weights.remove(worker).is_none() {
            return Arc::clone(&m.map);
        }
        self.remap(&mut m)
    }

    /// 更新工人报告的准入容量（拉取时上报）；不在成员表里的工人不记。
    pub fn set_capacity(&self, worker: &WorkerId, capacity: f64) {
        let mut m = lock(&self.members);
        if m.weights.contains_key(worker) && capacity.is_finite() && capacity >= 0.0 {
            m.capacity.insert(worker.clone(), capacity);
        }
    }

    /// 每个在线工人最近报告的准入容量，按工人 id 排序。
    pub fn capacities(&self) -> Vec<(WorkerId, f64)> {
        let m = lock(&self.members);
        let mut v: Vec<_> = m.capacity.iter().map(|(w, c)| (w.clone(), *c)).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// 在线工人报告的准入容量之和（掉线的工人已移出成员表，不计入）。
    pub fn capacity(&self) -> f64 {
        lock(&self.members).capacity.values().sum()
    }

    /// 更新工人往返时间的上界（续约、拉取时上报）。
    pub fn set_rtt(&self, worker: &WorkerId, rtt_hi: Duration) {
        lock(&self.members).rtt.insert(worker.clone(), rtt_hi);
    }

    fn remap(&self, m: &mut Members) -> Arc<HostMap> {
        m.changes += 1;
        // [推导] 成员表编号跨任期单调：高 32 位是任期、低 32 位是任期内的变更次数
        let epoch = (self.term() << 32) | (m.changes & u64::from(u32::MAX));
        let map = Arc::new(HostMap::new(
            epoch,
            m.weights.iter().map(|(w, x)| (w.clone(), *x)),
        ));
        m.map = Arc::clone(&map);
        let now = self.clock.now_ms();
        // 交接等待：旧负责人仍在线时，等成员表送到它、它交回礼貌状态，合计一个往返上界
        let wait_for = |w: &WorkerId| {
            if map.contains(w) {
                m.rtt.get(w).copied().unwrap_or_default()
            } else {
                Duration::ZERO
            }
        };
        for slot in self.slots.iter() {
            let mut s = lock(&slot.shard);
            s.set_map(Arc::clone(&map), now, &wait_for);
            self.publish(slot, &s);
        }
        map
    }

    /// 旧负责人交来站点的礼貌状态。
    pub fn handoff(&self, from: &WorkerId, state: SiteState) {
        let (_, slot) = self.slot_of_site(state.site);
        let mut s = lock(&slot.shard);
        s.handoff(from, state, self.clock.now_ms());
    }

    /// 站点暂停到的时刻（测试与监控用）。
    pub fn paused_until(&self, site: SiteKey) -> Option<Deadline> {
        let (_, slot) = self.slot_of_site(site);
        lock(&slot.shard).pause_until(site).map(Deadline)
    }

    // ---- 统计 ----

    pub fn register_stat<T: Stat + 'static>(&self, kind: StatKind) {
        lock(&self.stats).register::<T>(kind);
    }

    /// 合并一个统计增量；重复的（同来源、同种类、同序号）返回 `false`。
    pub fn merge_stats(&self, origin: &Origin, delta: StatsDelta) -> Result<bool, Error> {
        let term = self.writable()?;
        let mut hub = lock(&self.stats);
        if !hub.check(origin, &delta)? {
            return Ok(false);
        }
        let rec = Record::Stats {
            origin: origin.clone(),
            delta,
        };
        self.log(term, &rec)?;
        if let Record::Stats { origin, delta } = &rec {
            hub.apply(origin, delta);
        }
        drop(hub);
        self.maybe_snapshot();
        Ok(true)
    }

    pub fn priors(&self, scope: Scope) -> Priors {
        lock(&self.stats).priors(scope)
    }

    pub fn stat_table(&self, kind: StatKind) -> Option<Vec<u8>> {
        lock(&self.stats).table(kind)
    }

    // ---- 日志传送（温备一侧） ----

    /// 下一条期望的日志下标。
    pub fn shipped_len(&self) -> u64 {
        self.journal.as_ref().map_or(0, Journal::len)
    }

    /// 回放主网关的一条日志。下标必须连续；任期低于本账本任期的记录被栅栏拒绝
    /// （接管后旧网关的写入）。
    pub fn apply_shipped(&self, index: u64, raw: &[u8]) -> Result<(), Error> {
        let e = Entry::decode(raw)?;
        let term = self.term();
        if e.term < term || (!self.is_follower() && e.term <= term) {
            return Err(Error::Fenced {
                current: Fence { term, epoch: 0 },
            });
        }
        let expect = self.shipped_len();
        if index != expect {
            // 落后太多、主网关已截掉中间的日志：从它最近的快照整体重新同步
            let resync = index > expect && matches!(e.rec, Record::SnapBegin { .. });
            match &self.journal {
                Some(j) if resync => {
                    self.reset_state();
                    j.reset_to(index)?;
                }
                _ => {
                    return Err(Error::Transport(format!(
                        "journal gap: got {index}, expected {expect}"
                    )));
                }
            }
        }
        if let Some(j) = &self.journal {
            j.append_raw(raw)?;
        }
        self.term.fetch_max(e.term, Ordering::SeqCst);
        self.apply_entry(&e);
        self.after_shipped(index, &e.rec, raw.len() as u64)
    }
}
