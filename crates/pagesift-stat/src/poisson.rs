//! 页面变化率的贝叶斯估计与下一次检查时刻（0.95 monitor）。
//!
//! 把一个网址的内容变化看成速率 λ 的泊松过程（Cho & Garcia-Molina, "Estimating Frequency of
//! Change", ACM TOIT 3(3), 2003 的模型）。每次检查只知道“距上次检查 Δ 之内变没变”：
//! - 没变：似然 e^{−λΔ}；
//! - 变了：似然 1 − e^{−λΔ}（至少一次）。
//!
//! λ 的先验取伽马 Gamma(α₀, β₀)。“没变”的观测和伽马共轭（β 加上 Δ），“变了”的不共轭，后验在
//! ln λ 的等距网格上数值求（下面的 [`GRID`] 个点覆盖先验的 [10⁻⁹, 1−10⁻⁹] 分位以外很宽的范围，
//! 是数值积分的离散化，不是判决阈值）。
//!
//! 下一次检查：上次检查后过了 t 还没再查，“这段时间里已经变了、我们却没发现”的后验概率是
//! P(t) = 1 − E[e^{−λt} | 观测]。取 P(t) = δ 的 t（唯一的元参数 δ = 0.05），再由调用方夹在用户给的
//! 最短、最长间隔之间。没变的检查越多，λ 的后验越小，间隔越长；常变的页面间隔越短。

/// [策略] 先验：形状 α₀ = 1（指数分布，相当于一个伪观测，最弱的真伽马先验），均值每天变一次
/// （β₀ = 1 天）。依据：Cho & Garcia-Molina (VLDB 2000, "The Evolution of the Web and Implications
/// for an Incremental Crawler") 测得 .com 页面约 23% 每天都变、过半在一周内变过。
pub const PRIOR_SHAPE: f64 = 1.0;
/// [策略] 见 [`PRIOR_SHAPE`]：先验均值 1 / 天，单位秒。
pub const PRIOR_RATE_SECS: f64 = 86_400.0;
/// [推导] 后验网格点数：在 ln λ 上等距，Simpson 积分误差随点数四次方下降，2⁸ 个点时相对误差远小于 δ。
const GRID: usize = 256;

/// 一个网址的变化观测。
#[derive(Debug, Clone, PartialEq)]
pub struct ChangeRate {
    /// 先验形状 α₀。
    pub shape: f64,
    /// 先验速率 β₀ 加上所有“没变”区间的总长（秒）。
    pub exposure: f64,
    /// 每个“变了”的区间长度（秒）。
    pub changed: Vec<f64>,
}

impl Default for ChangeRate {
    fn default() -> Self {
        Self {
            shape: PRIOR_SHAPE,
            exposure: PRIOR_RATE_SECS,
            changed: Vec::new(),
        }
    }
}

impl ChangeRate {
    /// 记一次检查：距上次检查 `dt` 秒，内容变没变。
    pub fn observe(&mut self, dt: f64, changed: bool) {
        if !(dt > 0.0 && dt.is_finite()) {
            return;
        }
        if changed {
            self.changed.push(dt);
        } else {
            self.exposure += dt;
        }
    }

    /// 网格上的 (λ, 归一化权重)。
    fn posterior(&self) -> Vec<(f64, f64)> {
        // 网格范围：以“没变”的共轭后验为中心，两边各放宽到很远（含变了的区间所指向的高速率）
        let base = self.shape / self.exposure;
        let fastest = self.changed.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = if fastest.is_finite() {
            (base * 1e6).max(10.0 / fastest) // [推导] 覆盖到最短变化区间所对应速率的 10 倍
        } else {
            base * 1e6 // [推导] 先验均值的 10⁶ 倍：伽马(1, β) 在那里的尾概率 e^{-10⁶} 可忽略
        };
        let lo = base * 1e-6; // [推导] 同上，另一侧
        let (llo, lhi) = (lo.ln(), hi.ln());
        let step = (lhi - llo) / (GRID - 1) as f64;
        let mut logw: Vec<(f64, f64)> = (0..GRID)
            .map(|i| {
                let l = (llo + step * i as f64).exp();
                // 先验 ∝ λ^{α−1} e^{−βλ}；在 ln λ 上积分多一个雅可比因子 λ
                let mut lw = self.shape * l.ln() - self.exposure * l;
                for d in &self.changed {
                    lw += (-(-l * d).exp_m1()).max(f64::MIN_POSITIVE).ln();
                }
                (l, lw)
            })
            .collect();
        let m = logw.iter().map(|x| x.1).fold(f64::NEG_INFINITY, f64::max);
        let mut sum = 0.0;
        for x in &mut logw {
            x.1 = (x.1 - m).exp();
            sum += x.1;
        }
        for x in &mut logw {
            x.1 /= sum;
        }
        logw
    }

    /// 后验均值 E[λ]（每秒）。
    pub fn mean(&self) -> f64 {
        self.posterior().iter().map(|(l, w)| l * w).sum()
    }

    /// 上次检查后过了 `t` 秒，期间已经变了的后验概率。
    pub fn p_missed(&self, t: f64) -> f64 {
        1.0 - self
            .posterior()
            .iter()
            .map(|(l, w)| w * (-l * t).exp())
            .sum::<f64>()
    }

    /// 使 [`p_missed`](Self::p_missed) = `delta` 的 t（秒）：P(t) 单调递增，对 ln t 二分。
    pub fn next_check(&self, delta: f64) -> f64 {
        let post = self.posterior();
        let p = |t: f64| 1.0 - post.iter().map(|(l, w)| w * (-l * t).exp()).sum::<f64>();
        let (mut lo, mut hi) = (1e-3_f64, 1e12_f64); // [推导] 1 毫秒到约 3 万年：远宽于任何监控间隔
        // [推导] 对 ln t 二分 64 次：区间缩到 2⁻⁶⁴ 倍，远小于秒级
        for _ in 0..64 {
            let mid = (lo * hi).sqrt();
            if p(mid) < delta {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo * hi).sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conjugate_case_matches_closed_form() {
        // 只有“没变”：后验 Gamma(α, β)，E[e^{−λt}] = (β/(β+t))^α，P(t) = δ 的解 β((1−δ)^{−1/α} − 1)
        let mut r = ChangeRate::default();
        r.observe(3.0 * 86_400.0, false);
        let (a, b) = (r.shape, r.exposure);
        let want = b * ((1.0f64 - 0.05).powf(-1.0 / a) - 1.0);
        let got = r.next_check(0.05);
        assert!((got / want - 1.0).abs() < 0.02, "{got} vs {want}");
        assert!((r.mean() * b / a - 1.0).abs() < 0.02);
    }

    #[test]
    fn frequent_changes_shorten_the_interval() {
        let mut stable = ChangeRate::default();
        let mut busy = ChangeRate::default();
        for _ in 0..10 {
            stable.observe(3600.0, false);
            busy.observe(3600.0, true);
        }
        assert!(busy.next_check(0.05) < stable.next_check(0.05) / 10.0);
        assert!(busy.mean() > 10.0 * stable.mean());
        assert!(stable.p_missed(60.0) < busy.p_missed(60.0));
    }
}
