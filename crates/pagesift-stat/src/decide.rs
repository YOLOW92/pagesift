//! 决策：影子价格、"价值 − 价格 × 成本"的取舍、Thompson 选择、信息价值、等待。

use crate::beta::Posterior;
use crate::rng::Rng;
use crate::special::beta_inc;
use crate::survival::Survival;

/// 准入器管理的四种资源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resource {
    /// CPU 秒。
    Cpu,
    /// 内存字节秒。
    Memory,
    /// 浏览器标签秒。
    Browser,
    /// 某站点的礼貌额度（请求数）。
    Politeness,
}

/// [推导] 资源种类数，等于 [`Resource`] 的变体数
const RESOURCES: usize = 4;

impl Resource {
    pub const ALL: [Resource; RESOURCES] = [
        Resource::Cpu,
        Resource::Memory,
        Resource::Browser,
        Resource::Politeness,
    ];

    fn slot(self) -> usize {
        self as usize
    }
}

/// 每种资源的用量（预计或实际）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Cost([f64; RESOURCES]);

impl Cost {
    pub fn with(mut self, r: Resource, amount: f64) -> Self {
        self.0[r.slot()] = amount;
        self
    }

    pub fn get(&self, r: Resource) -> f64 {
        self.0[r.slot()]
    }
}

/// 每种资源的影子价格 `λ_r`（多占用一单位带来的额外排队代价，单位是"价值 / 资源单位"）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ShadowPrices([f64; RESOURCES]);

impl ShadowPrices {
    pub fn with(mut self, r: Resource, price: f64) -> Self {
        self.0[r.slot()] = price;
        self
    }

    pub fn get(&self, r: Resource) -> f64 {
        self.0[r.slot()]
    }

    /// `Σ_r λ_r · 成本_r`。
    pub fn charge(&self, cost: &Cost) -> f64 {
        self.0.iter().zip(cost.0.iter()).map(|(p, c)| p * c).sum()
    }
}

/// 影子价格的排队近似：`λ ≈ W/(1−ρ)`，`ρ` 是利用率、`W` 是平均服务时间。
///
/// M/M/1 的平均逗留时间是 `W/(1−ρ)`（Kleinrock, Queueing Systems Vol. 1, 1975, §3.2）；
/// 用它对占用量的导数量级作为边际排队代价。多服务台本应按 M/M/c，这里对每种资源取
/// 单服务台形式，c 台时 `ρ` 用总利用率，误差只在中等负载、且只影响价格的大小不影响方向。
/// 空闲时价格趋于 `W`，饱和时趋于无穷。
pub fn shadow_price(utilization: f64, service_time: f64) -> f64 {
    if utilization >= 1.0 {
        return f64::INFINITY;
    }
    service_time / (1.0 - utilization.max(0.0))
}

/// 一次决策的依据，写进 `PlanTrace`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Why {
    /// 用来决策的成功概率（Thompson 抽样值或后验均值）。
    pub p: f64,
    /// 后验均值。
    pub mean: f64,
    pub value: f64,
    pub charge: f64,
    /// 是否来自抽样（探索）。
    pub sampled: bool,
}

/// 做 `a` ⇔ `P(有增益)·价值 > Σ λ_r·成本_r(a)`；`P` 用 Thompson 采样从后验抽取。
pub fn decide(
    post: &Posterior,
    value: f64,
    cost: &Cost,
    prices: &ShadowPrices,
    rng: &mut Rng,
) -> (bool, Why) {
    let p = post.sample(rng);
    let charge = prices.charge(cost);
    let why = Why {
        p,
        mean: post.mean(),
        value,
        charge,
        sampled: true,
    };
    (p * value > charge, why)
}

/// 不抽样、按后验均值的决策：冻结模式和需要可复现的地方用。
pub fn decide_mean(
    post: &Posterior,
    value: f64,
    cost: &Cost,
    prices: &ShadowPrices,
) -> (bool, Why) {
    let p = post.mean();
    let charge = prices.charge(cost);
    (
        p * value > charge,
        Why {
            p,
            mean: p,
            value,
            charge,
            sampled: false,
        },
    )
}

/// 多个候选中选一个：每个候选给（后验、价值、成本），按抽样值的 `p·价值 − 价格·成本` 取最大
/// （Russo 等, "A Tutorial on Thompson Sampling", FnT ML 11(1), 2018, §3 的多臂形式）。
pub fn choose(
    options: &[(Posterior, f64, Cost)],
    prices: &ShadowPrices,
    rng: &mut Rng,
) -> Option<usize> {
    options
        .iter()
        .map(|(post, value, cost)| post.sample(rng) * value - prices.charge(cost))
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// 二元决策"做（收益 `θ·V − c`）或不做（0）"的完全信息期望价值（R. A. Howard, IEEE Trans. SSC 2(1), 1966）：
/// `VOI = E[max(θV − c, 0)] − max(E[θ]V − c, 0)`，`θ ~ Beta(α, β)`。
///
/// 闭式：`E[max(θV−c,0)] = V·E[θ]·(1 − I_{θ*}(α+1, β)) − c·(1 − I_{θ*}(α, β))`，`θ* = c/V`，
/// 用到 `θ·Beta(α,β)` 的密度正比于 `Beta(α+1, β)`。
pub fn value_of_information(post: &Posterior, value: f64, charge: f64) -> f64 {
    if value <= 0.0 {
        return 0.0;
    }
    let cut = (charge / value).clamp(0.0, 1.0);
    let (a, b) = (post.alpha, post.beta);
    let mean = post.mean();
    let perfect =
        value * mean * (1.0 - beta_inc(a + 1.0, b, cut)) - charge * (1.0 - beta_inc(a, b, cut));
    (perfect - (mean * value - charge).max(0.0)).max(0.0)
}

/// 等待决策：已等 `waited`，再等 `step`：
/// 继续 ⇔ `h(t, Δ)·价值 > λ·Δ·占用`。没有足够数据（`h` 为 NaN）时返回 `None`，由调用方用上层分布。
pub fn keep_waiting(
    done: &Survival,
    waited: f64,
    step: f64,
    value: f64,
    price: f64,
    occupancy: f64,
) -> Option<bool> {
    let h = done.finish_within(waited, step);
    if h.is_nan() {
        return None;
    }
    Some(h * value > price * step * occupancy)
}

/// 下次重试的时刻：故障持续时长分布上恢复概率达到 `1−δ` 的时刻。
pub fn retry_after(outage: &Survival, delta: crate::Delta) -> Option<f64> {
    outage.time_to(1.0 - delta.get())
}

/// Little 定律 `L = λW`（J. D. C. Little, Operations Research 9(3), 1961）。
pub fn little(arrival_rate: f64, time_in_system: f64) -> f64 {
    arrival_rate * time_in_system
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beta::{BetaCounts, Prior};

    /// 合成伯努利赌博机上 Thompson 采样的累积后悔呈对数增长（Agrawal & Goyal, COLT 2012）：
    /// 时长翻 10 倍，后悔的增量远小于 10 倍。
    #[test]
    fn thompson_regret_grows_logarithmically() {
        let probs = [0.3, 0.5, 0.55];
        let best = 0.55;
        let mut rng = Rng::seed(13);
        let mut arms = [BetaCounts::default(); 3];
        let mut regret = 0.0;
        let mut checkpoints = Vec::new();
        for t in 1..=20_000 {
            let opts: Vec<_> = arms
                .iter()
                .map(|a| (a.posterior(Prior::UNIFORM), 1.0, Cost::default()))
                .collect();
            let i = choose(&opts, &ShadowPrices::default(), &mut rng).unwrap();
            arms[i].observe(rng.uniform() < probs[i]);
            regret += best - probs[i];
            if t == 2_000 || t == 20_000 {
                checkpoints.push(regret);
            }
        }
        let (r1, r2) = (checkpoints[0], checkpoints[1]);
        assert!(r2 < 3.0 * r1, "regret {r1} → {r2} is not logarithmic");
        assert!(r2 < 0.02 * 20_000.0, "{r2}");
    }

    #[test]
    fn prices_voi_and_decisions() {
        assert_eq!(shadow_price(0.0, 2.0), 2.0);
        assert_eq!(shadow_price(0.5, 2.0), 4.0);
        assert!(shadow_price(1.0, 2.0).is_infinite());
        let post = Posterior {
            alpha: 2.0,
            beta: 2.0,
        };
        // 门槛恰在均值处时 VOI 最大；确定性的后验 VOI 趋于 0
        let v_mid = value_of_information(&post, 1.0, 0.5);
        let v_far = value_of_information(&post, 1.0, 0.95);
        assert!(v_mid > v_far && v_far >= 0.0);
        assert!(
            value_of_information(
                &Posterior {
                    alpha: 5e4,
                    beta: 5e4
                },
                1.0,
                0.5
            ) < 2e-3
        );
        let cost = Cost::default().with(Resource::Browser, 3.0);
        let prices = ShadowPrices::default().with(Resource::Browser, 0.1);
        assert!(decide_mean(&post, 1.0, &cost, &prices).0);
        assert!(!decide_mean(&post, 0.5, &cost, &prices.with(Resource::Browser, 1.0)).0);
        assert_eq!(little(10.0, 0.5), 5.0);
    }
}
