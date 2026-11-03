//! 按站点自学习从哪个浏览器内核开始渲染。
//!
//! 每个站、每个内核记一份"这个内核渲染出来就够用"的 Beta 计数。开始渲染时对内核链按顺序做
//! Thompson 采样（Thompson 1933；Chapelle & Li 2011）：从后验里抽一个"够用"的概率，抽到的值
//! 大于 1/2 就从这个内核开始，否则看下一个。探索来自后验的不确定性本身，不再需要 0.81 的
//! "连续 3 次回退才换起点、每 16 次探索一次"。
//!
//! - 先验包 v0：每个内核 [`PRIOR_SUFFICIENT`] 个"够用"的伪观测、没有"不够用"的伪观测。还没见过
//!   某个内核不够用时一定从它开始（和 0.81 一样，冷启动不变）；见过 3 次不够用，后验均值正好
//!   降到 1/2，对应 0.81 连续 3 次换起点的规则。
//! - 遗忘：每次观测前旧计数乘 `1−δ`，有效样本数不超过 `1/δ`，站点改版后能重新学回来。
//! - "开浏览器有没有用"不在这里学：0.87 起由 `engine/planner.rs` 按路径族的后验决定。
//!
//! 表按主机哈希分片（分片只为减少锁争用），总容量 [`MAX_HOSTS`]；某片满了时淘汰这一片里证据量
//! 较少的一半，而不是整表清空：只是少一次优化，不影响结果。

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::sync::Mutex;

use pagesift_stat::Delta;
use pagesift_stat::rng::Rng;

/// [策略] 先验包 v0：0.81 的现值（连续 3 次回退才换起点内核）。
pub const PRIOR_SUFFICIENT: f32 = 3.0;
/// [策略] 先验包 v0：0.81 的现值。每条约 50 字节，满了约 5 MiB。
pub const MAX_HOSTS: usize = 100_000;

/// 一个内核"够用 / 不够用"的计数（已乘过遗忘因子）。
#[derive(Default, Clone, Copy, Debug)]
struct Counts {
    sufficient: f32,
    insufficient: f32,
}

impl Counts {
    fn observe(&mut self, sufficient: bool, keep: f32) {
        self.sufficient *= keep;
        self.insufficient *= keep;
        if sufficient {
            self.sufficient += 1.0;
        } else {
            self.insufficient += 1.0;
        }
    }

    /// 这个内核这次够不够用：没见过不够用时直接算够用，否则 Thompson 采样。
    fn sample_sufficient(&self, rng: &mut Rng) -> bool {
        if self.insufficient <= 0.0 {
            return true;
        }
        let a = f64::from(PRIOR_SUFFICIENT + self.sufficient);
        let b = f64::from(self.insufficient);
        // [推导] 对称损失下的贝叶斯判定点：抽到的"够用"概率过半就用这个内核
        rng.beta(a, b) > 0.5
    }

    fn evidence(&self) -> f32 {
        self.sufficient + self.insufficient
    }
}

#[derive(Default)]
struct Shard {
    hosts: HashMap<u64, Vec<Counts>>,
}

/// 每次记录一个内核的结果时调用：(站点, 内核序号, 结果是否足够)。多服务器时工人用它把观测报给网关。
pub type EngineObserver = std::sync::Arc<dyn Fn(&str, usize, bool) + Send + Sync>;

pub struct HostLearner {
    observer: std::sync::OnceLock<EngineObserver>,
    hasher: RandomState,
    shards: Box<[Mutex<Shard>]>,
    per_shard: usize,
    keep: f32,
}

thread_local! {
    static RNG: RefCell<Rng> = RefCell::new(Rng::from_entropy());
}

impl Default for HostLearner {
    fn default() -> Self {
        Self::with_capacity(MAX_HOSTS, Delta::DEFAULT)
    }
}

impl HostLearner {
    fn with_capacity(capacity: usize, delta: Delta) -> Self {
        let delta = delta.get();
        // 分片数随核数，但每片至少放得下 1/δ 个站：哈希不均时也不会过早淘汰
        let min_per_shard = (1.0 / delta).ceil() as usize;
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        let shards = cores
            .next_power_of_two()
            .min((capacity / min_per_shard).max(1));
        HostLearner {
            observer: std::sync::OnceLock::new(),
            hasher: RandomState::new(),
            shards: (0..shards).map(|_| Mutex::default()).collect(),
            per_shard: capacity.div_ceil(shards),
            keep: (1.0 - delta) as f32,
        }
    }

    fn shard(&self, key: u64) -> std::sync::MutexGuard<'_, Shard> {
        let i = (key as usize) % self.shards.len();
        self.shards[i].lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 这个站从第几个内核开始渲染。
    pub fn start_engine(&self, host: &str) -> usize {
        let key = self.hasher.hash_one(host);
        let shard = self.shard(key);
        let Some(engines) = shard.hosts.get(&key) else {
            return 0;
        };
        RNG.with_borrow_mut(|rng| {
            engines
                .iter()
                .position(|c| c.sample_sufficient(rng))
                .unwrap_or(engines.len())
        })
    }

    /// 记录一次渲染：从 `from` 开始，最后用的是 `used`。`from..used` 的内核不够用，`used` 够用。
    pub fn record_engine(&self, host: &str, from: usize, used: usize) {
        let key = self.hasher.hash_one(host);
        let mut shard = self.shard(key);
        if shard.hosts.len() >= self.per_shard && !shard.hosts.contains_key(&key) {
            evict_low_evidence(&mut shard.hosts);
        }
        let engines = shard.hosts.entry(key).or_default();
        if engines.len() <= used {
            engines.resize(used + 1, Counts::default());
        }
        for (i, c) in engines.iter_mut().enumerate().take(used + 1).skip(from) {
            c.observe(i == used, self.keep);
        }
        drop(shard);
        if let Some(o) = self.observer.get() {
            for i in from..=used {
                o(host, i, i == used);
            }
        }
    }

    /// 接上观测回调（只能接一次）。
    pub fn observe_with(&self, o: EngineObserver) -> bool {
        self.observer.set(o).is_ok()
    }

    /// 导入别处学到的计数（网关下发的先验）。本地这个站点的这个内核已经有观测时不导入：本地的观测
    /// 已经报给过网关，再加上网关的合并结果会重复计数。
    pub fn import(&self, host: &str, engine: usize, sufficient: f64, insufficient: f64) {
        let key = self.hasher.hash_one(host);
        let mut shard = self.shard(key);
        if shard.hosts.len() >= self.per_shard && !shard.hosts.contains_key(&key) {
            evict_low_evidence(&mut shard.hosts);
        }
        let engines = shard.hosts.entry(key).or_default();
        if engines.len() <= engine {
            engines.resize(engine + 1, Counts::default());
        }
        let c = &mut engines[engine];
        if c.evidence() == 0.0 {
            c.sufficient = sufficient as f32;
            c.insufficient = insufficient as f32;
        }
    }
}

/// 淘汰证据量较少的一半：[推导] 每次淘汰一半，摊到每次插入上是 O(1)，留下的是信息量多的那一半。
fn evict_low_evidence(hosts: &mut HashMap<u64, Vec<Counts>>) {
    let evidence = |v: &Vec<Counts>| v.iter().map(Counts::evidence).sum::<f32>();
    let mut keys: Vec<(f32, u64)> = hosts.iter().map(|(k, v)| (evidence(v), *k)).collect();
    let half = keys.len() / 2;
    keys.select_nth_unstable_by(half, |a, b| a.0.total_cmp(&b.0));
    for (_, k) in &keys[..half] {
        hosts.remove(k);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_start_always_starts_at_the_first_engine() {
        let l = HostLearner::default();
        for _ in 0..1000 {
            assert_eq!(l.start_engine("new.test"), 0);
        }
        // 只见过够用，也一直从第一个开始
        l.record_engine("ok.test", 0, 0);
        for _ in 0..1000 {
            assert_eq!(l.start_engine("ok.test"), 0);
        }
    }

    #[test]
    fn learns_the_starting_engine_and_keeps_exploring() {
        let l = HostLearner::default();
        for _ in 0..20 {
            l.record_engine("spa.test", 0, 1);
        }
        let n = 2000;
        let skipped = (0..n).filter(|_| l.start_engine("spa.test") == 1).count();
        // 20 次都不够用：绝大多数从第二个内核开始，但仍偶尔试第一个（探索）
        assert!(skipped > n * 9 / 10, "{skipped}");
        assert!(skipped < n, "{skipped}");
    }

    #[test]
    fn three_misses_put_the_posterior_mean_at_one_half() {
        let mut c = Counts::default();
        for _ in 0..3 {
            c.observe(false, 1.0);
        }
        let mean = (PRIOR_SUFFICIENT + c.sufficient) / (PRIOR_SUFFICIENT + c.evidence());
        assert_eq!(mean, 0.5);
    }

    #[test]
    fn relearns_after_the_site_changes() {
        let l = HostLearner::default();
        for _ in 0..30 {
            l.record_engine("spa.test", 0, 1);
        }
        // 改版后第一个内核又够用了：探索时记下的"够用"很快把起点拉回来
        for _ in 0..30 {
            l.record_engine("spa.test", 0, 0);
        }
        let n = 2000;
        let first = (0..n).filter(|_| l.start_engine("spa.test") == 0).count();
        assert!(first > n * 9 / 10, "{first}");
    }

    #[test]
    fn full_shard_evicts_the_less_informed_half() {
        let l = HostLearner::with_capacity(40, Delta::DEFAULT);
        assert!(l.shards.len() <= 2);
        l.record_engine("busy.test", 0, 1);
        l.record_engine("busy.test", 0, 1);
        for i in 0..200 {
            l.record_engine(&format!("h{i}.test"), 0, 0);
        }
        let total: usize = l.shards.iter().map(|s| s.lock().unwrap().hosts.len()).sum();
        assert!(total <= 40, "{total}");
        let busy = l.hasher.hash_one("busy.test");
        assert!(l.shard(busy).hosts.contains_key(&busy));
    }
}
