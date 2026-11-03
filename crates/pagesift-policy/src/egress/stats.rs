//! 出口池的统计表：（站点，出口）与（注册域，出口）的折扣被拒计数，出口类别的收缩先验。
//!
//! **收缩**（经验贝叶斯：H. Robbins, 1956；B. Efron, C. Morris, JASA 70(350), 1975）：
//! - 类别先验 `Beta(μ_c m_c, (1−μ_c) m_c)`：该类别所有（注册域，出口）格子的计数用
//!   [`pagesift_stat::shrink::beta_prior`] 做矩估计；少于两组时是 `Beta(1,1)`（冷启动）。
//! - 注册域层：`Beta(先验_c + 域计数 − 本站计数)` 的均值（减去本站，免得本站的证据算两遍）。
//! - 站点层：以注册域层均值为中心、强度 `m_c` 的先验，加本站计数。两层用同一个由矩估计得出的
//!   强度：它度量的是同类别里各组之间的离散程度，不论组是站点还是注册域。
//! - 先验强度不超过一个格子在折扣下能积累的有效样本数 `N_eff`：否则站点自己的证据永远压不过先验。
//!
//! **分片**：按注册域哈希分片，站点和它的注册域在同一分片里，一次选择只锁一把锁；
//! 分片数随核数（不小于逻辑核数的 2 的幂），分片独占缓存行。类别先验放在原子量里，
//! 热路径只读；重估在后台按摊还 O(1) 的节奏做（观测数累计到格子总数时一次）。
//!
//! **有界**：每个分片的站点数有上限（内存预算 / 分片数 / 每站估算字节），超出时淘汰到一半
//! （摊还 O(1)，动态表，CLRS §17.4）：先淘汰非 Cookie 绑定、样本少的站点；没有站点引用的注册域随之删除。

use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use pagesift_stat::beta::{BetaCounts, Posterior, Prior};
use pagesift_stat::rng::{Rng, splitmix64};
use pagesift_stat::shrink::beta_prior;

use super::{EgressClass, PoolConfig, Site};

/// 一个站点在各出口上的折扣计数（`success` 是被拒次数，`failure` 是成功次数）。
struct SiteEntry {
    domain: u64,
    cells: Box<[BetaCounts]>,
    /// Cookie 会话固定的出口。
    sticky: Option<usize>,
    /// 最近一次使用（分片内的逻辑时钟）。
    last: u64,
}

struct ShardState {
    sites: HashMap<u64, SiteEntry>,
    domains: HashMap<u64, Box<[BetaCounts]>>,
    rng: Rng,
    tick: u64,
    since_refit: usize,
}

/// 独占缓存行的分片（[推导] 128 字节：相邻缓存行成对预取，按两行对齐）。
#[repr(align(128))]
struct Shard(Mutex<ShardState>);

pub(super) struct Stats {
    shards: Box<[Shard]>,
    hasher: std::collections::hash_map::RandomState,
    /// 每个出口的类别。
    classes: Vec<EgressClass>,
    /// 每个类别的先验，打包成 `(α as f32, β as f32)`，一次原子读拿到一致的一对。
    class_prior: [AtomicU64; EgressClass::COUNT],
    /// 折扣因子 `γ = 1 − 1/N_eff`。
    gamma: f64,
    n_eff: f64,
    /// 每个分片最多多少站点。
    cap: usize,
    /// 上次重估时的注册域总数。
    domains_seen: AtomicUsize,
    refitting: AtomicBool,
}

fn pack(p: Prior) -> u64 {
    // [协议] 两个 IEEE 754 单精度（32 位）拼成一个 64 位字
    (u64::from((p.alpha as f32).to_bits()) << 32) | u64::from((p.beta as f32).to_bits())
}

fn unpack(v: u64) -> Prior {
    Prior {
        alpha: f64::from(f32::from_bits((v >> 32) as u32)), // [协议] 高 32 位
        beta: f64::from(f32::from_bits(v as u32)),
    }
}

impl Stats {
    pub(super) fn new(classes: Vec<EgressClass>, config: &PoolConfig) -> Self {
        let z = config.delta.z();
        // [推导] 有效样本数 N_eff = z(1−δ)²·¼/δ²：比例在最坏方差 ¼ 下单侧 (1−δ) 误差界为 δ 所需的样本量
        // （W. G. Cochran, Sampling Techniques, 3rd ed., 1977, §4.4）。折扣和 Σγᵏ = 1/(1−γ) = N_eff
        let n_eff = (z * z / 4.0 / (config.delta.get() * config.delta.get())).max(2.0);
        let n = classes.len().max(1);
        let shards = std::thread::available_parallelism()
            .map_or(1, |p| p.get())
            .next_power_of_two();
        // 每站估算：站点条目 + 一份注册域条目（最坏每站一个注册域），各带 n 个计数；
        // [推导] 哈希表最大装载率 7/8（hashbrown 的 SwissTable），按 8/7 放大
        let per_site = (std::mem::size_of::<(u64, SiteEntry)>()
            + std::mem::size_of::<(u64, Box<[BetaCounts]>)>()
            + 2 * n * std::mem::size_of::<BetaCounts>())
            * 8
            / 7;
        let cap = (config.memory_budget / shards / per_site).max(2);
        let seed = config
            .seed
            .unwrap_or_else(|| Rng::from_entropy().next_u64());
        Self {
            shards: (0..shards)
                .map(|i| {
                    Shard(Mutex::new(ShardState {
                        sites: HashMap::new(),
                        domains: HashMap::new(),
                        rng: Rng::seed(splitmix64(seed ^ i as u64)),
                        tick: 0,
                        since_refit: 0,
                    }))
                })
                .collect(),
            hasher: Default::default(),
            classes,
            class_prior: std::array::from_fn(|_| AtomicU64::new(pack(Prior::UNIFORM))),
            gamma: 1.0 - 1.0 / n_eff,
            n_eff,
            cap,
            domains_seen: AtomicUsize::new(0),
            refitting: AtomicBool::new(false),
        }
    }

    fn keys(&self, site: Site<'_>) -> (u64, u64) {
        (
            self.hasher.hash_one(site.domain),
            self.hasher.hash_one(site.host),
        )
    }

    fn shard(&self, domain: u64) -> &Mutex<ShardState> {
        &self.shards[(domain as usize) & (self.shards.len() - 1)].0
    }

    fn prior(&self, class: EgressClass) -> Prior {
        unpack(self.class_prior[class.index()].load(Ordering::Relaxed))
    }

    /// 站点在出口 `e` 上被拒概率的收缩后验。
    fn posterior(
        &self,
        e: usize,
        site: Option<&[BetaCounts]>,
        domain: Option<&[BetaCounts]>,
    ) -> Posterior {
        let pc = self.prior(self.classes[e]);
        let own = site.map_or_else(BetaCounts::default, |c| c[e]);
        let dom = domain.map_or_else(BetaCounts::default, |c| c[e]);
        let rest = BetaCounts {
            success: (dom.success - own.success).max(0.0),
            failure: (dom.failure - own.failure).max(0.0),
        };
        let center = rest.posterior(pc).mean();
        own.posterior(Prior::from_mean(center, pc.alpha + pc.beta))
    }

    pub(super) fn posterior_of(&self, site: Site<'_>, e: usize) -> Posterior {
        let (dk, sk) = self.keys(site);
        let st = self.shard(dk).lock().expect("lock poisoned");
        self.posterior(
            e,
            st.sites.get(&sk).map(|s| &*s.cells),
            st.domains.get(&dk).map(|d| &**d),
        )
    }

    /// 选出口：`cookies` 且站点已有绑定时直接用绑定；否则 `choose(后验, rng)`，`cookies` 时绑定结果。
    /// 返回（出口，是否绑定）。
    pub(super) fn pick(
        &self,
        site: Site<'_>,
        cookies: bool,
        choose: impl FnOnce(&dyn Fn(usize) -> Posterior, &mut Rng) -> Option<usize>,
    ) -> Option<(usize, bool)> {
        let (dk, sk) = self.keys(site);
        let mut guard = self.shard(dk).lock().expect("lock poisoned");
        let st = &mut *guard;
        st.tick += 1;
        let tick = st.tick;
        if cookies
            && let Some(entry) = st.sites.get_mut(&sk)
            && let Some(bound) = entry.sticky
        {
            entry.last = tick;
            return Some((bound, true));
        }
        let id = {
            let site_cells = st.sites.get(&sk).map(|s| &*s.cells);
            let dom_cells = st.domains.get(&dk).map(|d| &**d);
            let post = |e: usize| self.posterior(e, site_cells, dom_cells);
            choose(&post, &mut st.rng)?
        };
        if cookies {
            let entry = self.site_entry(st, dk, sk);
            entry.sticky = Some(id);
            entry.last = tick;
            self.evict(st);
        }
        Some((id, cookies))
    }

    fn site_entry<'s>(&self, st: &'s mut ShardState, dk: u64, sk: u64) -> &'s mut SiteEntry {
        let n = self.classes.len();
        st.sites.entry(sk).or_insert_with(|| SiteEntry {
            domain: dk,
            cells: vec![BetaCounts::default(); n].into_boxed_slice(),
            sticky: None,
            last: 0,
        })
    }

    /// 记一次观测（折扣后加上），然后在锁内用站点的计数做决定。
    pub(super) fn observe<R>(
        &self,
        site: Site<'_>,
        e: usize,
        denied: bool,
        decide: impl FnOnce(&[BetaCounts]) -> R,
    ) -> R {
        let (dk, sk) = self.keys(site);
        let n = self.classes.len();
        let gamma = self.gamma;
        let discount_add = |cells: &mut [BetaCounts]| {
            for c in cells.iter_mut() {
                c.success *= gamma;
                c.failure *= gamma;
            }
            cells[e].observe(denied);
        };
        let (out, due) = {
            let mut guard = self.shard(dk).lock().expect("lock poisoned");
            let st = &mut *guard;
            st.tick += 1;
            let tick = st.tick;
            discount_add(
                st.domains
                    .entry(dk)
                    .or_insert_with(|| vec![BetaCounts::default(); n].into_boxed_slice()),
            );
            let entry = self.site_entry(st, dk, sk);
            entry.last = tick;
            discount_add(&mut entry.cells);
            let out = decide(&entry.cells);
            self.evict(st);
            st.since_refit += 1;
            let due = st.since_refit
                >= self
                    .domains_seen
                    .load(Ordering::Relaxed)
                    .max(self.shards.len());
            if due {
                st.since_refit = 0;
            }
            (out, due)
        };
        if due {
            self.refit();
        }
        out
    }

    /// 站点数超出上限时淘汰到一半：非绑定的按样本数、绑定的按最近使用，从小到大。
    fn evict(&self, st: &mut ShardState) {
        if st.sites.len() <= self.cap {
            return;
        }
        let mut order: Vec<(bool, f64, u64)> = st
            .sites
            .iter()
            .map(|(k, s)| {
                let info = match s.sticky {
                    Some(_) => s.last as f64,
                    None => s.cells.iter().map(BetaCounts::trials).sum(),
                };
                (s.sticky.is_some(), info, *k)
            })
            .collect();
        order.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        let excess = st.sites.len() - self.cap / 2;
        for (_, _, k) in order.into_iter().take(excess) {
            st.sites.remove(&k);
        }
        let live: HashSet<u64> = st.sites.values().map(|s| s.domain).collect();
        st.domains.retain(|d, _| live.contains(d));
    }

    /// 重估各类别的先验。同一时间只有一个线程做；逐个分片短暂加锁，不同时持有两把。
    fn refit(&self) {
        if self.refitting.swap(true, Ordering::Acquire) {
            return;
        }
        let mut groups: [Vec<BetaCounts>; EgressClass::COUNT] = Default::default();
        let mut domains = 0;
        for shard in self.shards.iter() {
            let st = shard.0.lock().expect("lock poisoned");
            domains += st.domains.len();
            for cells in st.domains.values() {
                for (e, c) in cells.iter().enumerate() {
                    if c.trials() > 0.0 {
                        groups[self.classes[e].index()].push(*c);
                    }
                }
            }
        }
        for (slot, g) in self.class_prior.iter().zip(groups.iter()) {
            let prior = beta_prior(g).map_or(Prior::UNIFORM, |p| {
                let m = p.alpha + p.beta;
                if m > self.n_eff {
                    Prior::from_mean(p.alpha / m, self.n_eff)
                } else {
                    p
                }
            });
            slot.store(pack(prior), Ordering::Relaxed);
        }
        self.domains_seen.store(domains, Ordering::Relaxed);
        self.refitting.store(false, Ordering::Release);
    }

    #[cfg(test)]
    pub(super) fn class_prior(&self, class: EgressClass) -> Prior {
        self.prior(class)
    }

    #[cfg(test)]
    pub(super) fn sites(&self) -> usize {
        self.shards
            .iter()
            .map(|s| s.0.lock().unwrap().sites.len())
            .sum()
    }

    #[cfg(test)]
    pub(super) fn cap(&self) -> usize {
        self.cap
    }
}
