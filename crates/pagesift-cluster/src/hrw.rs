//! 站点亲和：加权 rendezvous 哈希（HRW）。
//!
//! D. G. Thaler, C. V. Ravishankar, "Using Name-Based Mappings to Increase Hit Rates",
//! IEEE/ACM Transactions on Networking 6(1), 1998：每个站点 `s` 归得分 `h(s, m)` 最高的成员 `m`。
//! 成员增减时只有归属于（或改归）变动成员的站点换负责人，期望迁移比例是 `1/N`，是任何映射的下界。
//!
//! 加权版本用 C. Schindelhauer, G. Schomaker, "Weighted Distributed Hash Tables", SPAA 2005 的
//! 对数法：得分 `w_m / (−ln u)`，`u = h(s, m) ∈ (0,1)` 均匀；成员 `m` 拿到站点的概率正好是
//! `w_m / Σw`，迁移量仍然最小。权重相同时退化为原始 HRW（`argmax u`）。
//!
//! `h` 用 SplitMix64 的终结器混合站点键与成员键（成员键是成员 id 的 BLAKE3 摘要前 8 字节），
//! 网关与工人用同一个函数，算出的归属一致。

use pagesift_stat::{DecodeError, Reader, Writer};

use pagesift_stat::rng::splitmix64 as mix;

use crate::types::{Owner, SiteKey, WorkerId};

/// 一个成员。
#[derive(Debug, Clone, PartialEq)]
struct Member {
    id: WorkerId,
    /// 相对容量（硬件档案给出），必须为正。
    weight: f64,
    key: u64,
}

impl Member {
    fn new(id: WorkerId, weight: f64) -> Self {
        let key = crate::types::hash64(id.as_str().as_bytes());
        Self { id, weight, key }
    }
}

/// 成员表（网关下发的 `HostMap{epoch, 成员表}`）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HostMap {
    epoch: u64,
    members: Vec<Member>,
}

impl HostMap {
    /// 成员按 id 排序，保证网关和工人看到的表逐字一致；权重非正的成员不参与。
    pub fn new(epoch: u64, members: impl IntoIterator<Item = (WorkerId, f64)>) -> Self {
        let mut members: Vec<Member> = members
            .into_iter()
            .filter(|(_, w)| *w > 0.0 && w.is_finite())
            .map(|(id, w)| Member::new(id, w))
            .collect();
        members.sort_by(|a, b| a.id.cmp(&b.id));
        members.dedup_by(|a, b| a.id == b.id);
        Self { epoch, members }
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn contains(&self, id: &WorkerId) -> bool {
        self.members.iter().any(|m| &m.id == id)
    }

    /// 站点的负责人；没有成员时为 `None`。
    pub fn owner(&self, site: SiteKey) -> Option<&WorkerId> {
        let s = mix(site.0);
        self.members
            .iter()
            .map(|m| (score(s, m), m))
            .max_by(|a, b| a.0.total_cmp(&b.0).then_with(|| b.1.id.cmp(&a.1.id)))
            .map(|(_, m)| &m.id)
    }

    /// 站点的负责者：单主机（`local`）全归本进程；否则按 HRW，没有成员时为 `Unassigned`。
    pub fn owner_of(&self, site: SiteKey, local: bool) -> Owner {
        if local {
            return Owner::Local;
        }
        self.owner(site)
            .map_or(Owner::Unassigned, |w| Owner::Worker(w.clone()))
    }

    /// 从 `self` 换到 `next` 时换了负责人的站点：`(站点, 旧负责人, 新负责人)`。
    pub fn moved(
        &self,
        next: &HostMap,
        sites: impl IntoIterator<Item = SiteKey>,
    ) -> Vec<(SiteKey, Option<WorkerId>, Option<WorkerId>)> {
        sites
            .into_iter()
            .filter_map(|s| {
                let (a, b) = (self.owner(s), next.owner(s));
                (a != b).then(|| (s, a.cloned(), b.cloned()))
            })
            .collect()
    }

    pub fn encode(&self, w: &mut Writer) {
        w.u64(self.epoch);
        w.u32(self.members.len() as u32);
        for m in &self.members {
            w.bytes(m.id.as_str().as_bytes());
            w.f64(m.weight);
        }
    }

    pub fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let epoch = r.u64()?;
        let n = r.u32()?;
        let mut members = Vec::new();
        for _ in 0..n {
            let id =
                std::str::from_utf8(r.bytes()?).map_err(|_| DecodeError::Invalid("member id"))?;
            let w = r.f64()?;
            members.push((WorkerId::new(id), w));
        }
        Ok(Self::new(epoch, members))
    }
}

/// `w / (−ln u)`，`u ∈ (0,1)` 由混合后的高 53 位得到（取格点中点，避开 0 和 1）。
fn score(site_mix: u64, m: &Member) -> f64 {
    // [协议] IEEE 754 双精度有 53 位有效数字，64 − 53 = 11 位右移后恰好能精确表示
    const MANTISSA: u32 = 53;
    let h = mix(site_mix ^ m.key) >> (u64::BITS - MANTISSA);
    let u = (h as f64 + 0.5) / (1u64 << MANTISSA) as f64;
    m.weight / -u.ln()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(epoch: u64, ids: &[&str]) -> HostMap {
        HostMap::new(epoch, ids.iter().map(|i| (WorkerId::new(*i), 1.0)))
    }

    fn sites(n: u64) -> impl Iterator<Item = SiteKey> {
        (0..n).map(SiteKey)
    }

    /// 成员增减时迁移量接近最小：加一个成员只把约 `1/(N+1)` 的站点迁给新成员，
    /// 减一个成员只迁走它原来的站点。界取二项分布的 `z(1−δ)` 单侧上界。
    #[test]
    fn minimal_movement_on_membership_change() {
        let n_sites = 20_000;
        let ids: Vec<String> = (0..10).map(|i| format!("w{i}")).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let before = map(1, &refs);
        let mut grown = refs.clone();
        grown.push("w10");
        let after = map(2, &grown);
        let moved = before.moved(&after, sites(n_sites));
        assert!(
            moved
                .iter()
                .all(|(_, _, to)| to.as_ref().map(WorkerId::as_str) == Some("w10")),
            "every moved site goes to the new member"
        );
        let p = 1.0 / grown.len() as f64;
        let mean = n_sites as f64 * p;
        let sd = (n_sites as f64 * p * (1.0 - p)).sqrt();
        let z = pagesift_stat::Delta::DEFAULT.z();
        let k = moved.len() as f64;
        assert!(
            (k - mean).abs() < z * sd * 2.0,
            "moved {k}, expected {mean}"
        );

        let shrunk = map(3, &refs[1..]);
        let moved = before.moved(&shrunk, sites(n_sites));
        assert!(
            moved
                .iter()
                .all(|(_, from, _)| from.as_ref().map(WorkerId::as_str) == Some("w0")),
            "only the removed member's sites move"
        );
        let owned_by_w0 = sites(n_sites)
            .filter(|s| before.owner(*s).map(WorkerId::as_str) == Some("w0"))
            .count();
        assert_eq!(moved.len(), owned_by_w0);
    }

    #[test]
    fn weights_shift_share_and_encoding_round_trips() {
        let m = HostMap::new(
            7,
            [(WorkerId::new("big"), 3.0), (WorkerId::new("small"), 1.0)],
        );
        let n = 40_000u64;
        let big = sites(n)
            .filter(|s| m.owner(*s).map(WorkerId::as_str) == Some("big"))
            .count() as f64;
        let p = 0.75;
        let sd = (n as f64 * p * (1.0 - p)).sqrt();
        assert!((big - n as f64 * p).abs() < pagesift_stat::Delta::DEFAULT.z() * sd * 2.0);
        let mut w = Writer::new();
        m.encode(&mut w);
        let back = HostMap::decode(&mut Reader::new(w.as_slice())).unwrap();
        assert_eq!(back, m);
        assert!(HostMap::default().owner(SiteKey(1)).is_none());
    }
}
