//! 渲染决策的学习表：每个路径族"渲染有没有增益"的 Beta 计数、族的键，以及候选合并时用的质量分。
//! 引擎（`pagesift` 的 `engine/planner.rs`）在请求路径上调用它。
//!
//! **要不要开浏览器**：每个路径族记一个"渲染带来质量增益"的 Beta 计数。
//! - 族里还没有数据时照旧按启发式判断（引擎的启发式）：先验包 v0 就是 0.81 的规则，冷启动行为不变；
//! - 有数据后按 Thompson 采样（Russo 等, FnT ML 2018, §3）：抽 `p ~ Beta(1 + 增益次数, 1 + 无增益次数)`，
//!   渲染 ⇔ `p · (1 − Q_http) > E[渲染秒数] · (λ_browser + v_t)`。`Q_http` 是 HTTP 结果的校准质量分，
//!   `1 − Q_http` 是最多还能补回的质量；`v_t = 1 / 剩余期限` 是时间价值（耗尽整个期限记一个质量单位），
//!   `λ_browser` 是浏览器的影子价格（`pagesift_stat::decide::shadow_price`，空闲时为 0）。
//! - "有增益"= 渲染（或合并）结果的字比 HTTP 的多（0.81 的规则），或者质量分更高：质量分只能多算增益，
//!   不能否决 0.81 会算的增益（它不看篇幅）。
//!
//! 先验取 `Beta(1,1)`，不向站点、全局收缩（这里没有用 `pagesift_stat::shrink` 的层级收缩）。
//!
//! 计数表按族哈希分片，分片数随核数增长：大机器上不让所有请求抢同一把锁。
//! 族的键是族名的 BLAKE3（跨进程稳定），整张表可以存进存储、重启后读回（[`RenderPlanner::encode`]）。

use std::collections::HashMap;
use std::sync::Mutex;

use pagesift_stat::beta::{BetaCounts, Prior};
use pagesift_stat::rng::Rng;
use pagesift_stat::{Reader, Stat, Writer};

/// [协议] 表的编码版本：键或计数的含义一变就加一，旧表整体丢弃
const TABLE_VERSION: u32 = 1;

/// 一次渲染决策的依据（写进 `metadata.pagesift.plan`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderWhy {
    /// 是否来自学到的后验（否则是冷启动的启发式）。
    pub learned: bool,
    pub p: f64,
    pub value: f64,
    pub charge: f64,
}

/// 一次观测的去处（多服务器工人：攒成统计增量报给网关，见 `crate::cluster`）：（族的键，有没有增益）。
pub type PlanObserver = std::sync::Arc<dyn Fn(u64, bool) + Send + Sync>;

pub struct RenderPlanner {
    shards: Box<[Mutex<HashMap<u64, BetaCounts>>]>,
    observer: std::sync::OnceLock<PlanObserver>,
}

impl Default for RenderPlanner {
    fn default() -> Self {
        // 分片数取不小于核数的 2 的幂：锁冲突概率随分片数下降，表本身很小
        let n = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .next_power_of_two();
        Self {
            shards: (0..n).map(|_| Mutex::new(HashMap::new())).collect(),
            observer: std::sync::OnceLock::new(),
        }
    }
}

thread_local! {
    static RNG: std::cell::RefCell<Rng> = std::cell::RefCell::new(Rng::from_entropy());
}

impl RenderPlanner {
    fn with<R>(&self, family: &str, f: impl FnOnce(&mut HashMap<u64, BetaCounts>, u64) -> R) -> R {
        let key = family_key(family);
        let shard = &self.shards[(key as usize) & (self.shards.len() - 1)];
        f(&mut shard.lock().unwrap_or_else(|e| e.into_inner()), key)
    }

    /// 是否渲染。`heuristic` 是 0.81 规则的结论；`render_secs` 是预计渲染耗时（秒）；
    /// `remaining_secs` 是这个请求剩下的期限；`browser_price` 是浏览器的影子价格（每标签秒）。
    pub fn decide(
        &self,
        family: &str,
        heuristic: bool,
        http_quality: f64,
        render_secs: f64,
        remaining_secs: f64,
        browser_price: f64,
    ) -> (bool, RenderWhy) {
        let counts = self.with(family, |m, k| m.get(&k).copied());
        let value = 1.0 - http_quality.clamp(0.0, 1.0);
        let charge = render_secs * (browser_price + 1.0 / remaining_secs.max(f64::MIN_POSITIVE));
        match counts.filter(|c| c.trials() > 0.0) {
            None => (
                heuristic,
                RenderWhy {
                    learned: false,
                    p: f64::from(u8::from(heuristic)),
                    value,
                    charge,
                },
            ),
            Some(c) => {
                let p = RNG.with(|r| c.posterior(Prior::UNIFORM).sample(&mut r.borrow_mut()));
                (
                    p * value > charge,
                    RenderWhy {
                        learned: true,
                        p,
                        value,
                        charge,
                    },
                )
            }
        }
    }

    /// 记一次渲染的结果：渲染（或合并）后的质量分是否高于 HTTP 的。
    pub fn record(&self, family: &str, gain: bool) {
        let key = self.with(family, |m, k| {
            m.entry(k).or_default().observe(gain);
            k
        });
        if let Some(o) = self.observer.get() {
            o(key, gain);
        }
    }

    /// 装上观测的去处（只能装一次）。
    pub fn observe_with(&self, o: PlanObserver) -> bool {
        self.observer.set(o).is_ok()
    }

    /// 导入别处学到的计数（网关下发的先验）。本地已经有这个族的观测时不导入：本地的观测已经报给过网关，
    /// 再加上网关的合并结果会重复计数。
    pub fn import(&self, key: u64, counts: BetaCounts) {
        let shard = &self.shards[(key as usize) & (self.shards.len() - 1)];
        let mut m = shard.lock().unwrap_or_else(|e| e.into_inner());
        let e = m.entry(key).or_default();
        if e.trials() == 0.0 {
            *e = counts;
        }
    }

    /// 族里"渲染有增益"的后验均值（`Beta(1,1)` 先验）；族里还没有数据时为 `None`。
    pub fn mean(&self, family: &str) -> Option<f64> {
        self.with(family, |m, k| m.get(&k).copied())
            .filter(|c| c.trials() > 0.0)
            .map(|c| c.posterior(Prior::UNIFORM).mean())
    }

    /// 整张表编码成字节（引擎把它写进存储）。
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(TABLE_VERSION);
        let entries: Vec<(u64, BetaCounts)> = self
            .shards
            .iter()
            .flat_map(|s| {
                let m = s.lock().unwrap_or_else(|e| e.into_inner());
                m.iter().map(|(k, v)| (*k, *v)).collect::<Vec<_>>()
            })
            .collect();
        w.u64(entries.len() as u64);
        for (k, v) in &entries {
            w.u64(*k);
            v.encode(&mut w);
        }
        w.into_inner()
    }

    /// 读回 [`Self::encode`] 的字节；版本不同或损坏时保持空表（冷启动）。读回的计数与已有计数合并。
    pub fn decode_merge(&self, buf: &[u8]) {
        let mut r = Reader::new(buf);
        let read = |r: &mut Reader<'_>| -> Option<Vec<(u64, BetaCounts)>> {
            if r.u32().ok()? != TABLE_VERSION {
                return None;
            }
            let n = r.u64().ok()?;
            (0..n)
                .map(|_| Some((r.u64().ok()?, BetaCounts::decode(r).ok()?)))
                .collect()
        };
        for (k, v) in read(&mut r).unwrap_or_default() {
            let shard = &self.shards[(k as usize) & (self.shards.len() - 1)];
            shard
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(k)
                .or_default()
                .merge(&v);
        }
    }
}

pub fn family_key(family: &str) -> u64 {
    let h = blake3::hash(family.as_bytes());
    // [协议] 取 BLAKE3 摘要的前 8 字节作 u64 键
    let (head, _) = h
        .as_bytes()
        .split_first_chunk::<8>()
        .expect("BLAKE3 digest is 32 bytes");
    u64::from_le_bytes(*head)
}

/// 渲染决策用的族：路径族再按"有没有客户端代码"分开。0.81 的规则是静态页的经验不能压过带脚本的页面，
/// 分开记就不会互相影响。
pub fn render_family(url: &str, client_code: bool) -> String {
    format!(
        "{}#{}",
        family(url),
        if client_code { "js" } else { "static" }
    )
}

/// 换内核决策的族：同一路径族、从第 `engine` 个内核换到下一个。
pub fn fallback_family(url: &str, engine: usize) -> String {
    format!("{}#after{engine}", family(url))
}

/// 路径族：来源 + 首级路径（与 0.81 `learn.rs` 的划分一致）。
pub fn family(raw: &str) -> String {
    let Ok(url) = url::Url::parse(raw) else {
        return raw.to_string();
    };
    let section = url
        .path()
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("");
    format!("{}/{}", url.origin().ascii_serialization(), section)
}

/// 一页提取结果的校准质量分（内置模型和权重）；模型没加载上时为 `None`。
pub fn quality(doc: &pagesift_content::Document, html_bytes: usize) -> Option<f64> {
    let model = crate::quality::QualityModel::builtin()?;
    let weights = crate::script::ScriptWeights::builtin();
    Some(model.predict(&crate::quality::features(doc, html_bytes, weights)))
}
