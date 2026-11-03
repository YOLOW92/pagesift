//! 等待的"剩余完成概率"与分位数的无分布置信界。
//!
//! [`Remaining`]：同一类等待（网络静默、iframe 抓取、空闲浏览器被复用……）的完成时间，含右截尾。
//! 已经等到 `t` 还没完成的等待里，能在上限 `cap` 之前完成的条件概率
//!
//! ```text
//! p(t) = 1 − Ŝ(cap) / Ŝ(t)          （Ŝ 是 Kaplan–Meier 乘积限估计，JASA 53, 1958）
//! ```
//!
//! 就是 [`crate::survival`] 里条件完成概率 `h(t, Δ)` 取 `Δ = cap−t`。[`Remaining::give_up_at`] 取最早的 `t`，使 `p(t)` 的
//! Wilson 上界（JASA 22, 1927）低于 `δ`：再等下去，仍在等的里面至多 `δ` 能等到结果，
//! 而且这个判断在 `1−δ` 置信水平上成立（只在显著时更新）。受影响的只占全部等待的
//! `δ·Ŝ(t)`，比直接取完成时间的 `q(1−δ)`（砍掉 `δ`）保守得多。
//!
//! 和原文的差别：Wilson 区间用于二项比例，这里的 `k = n·p̂`（`n` 是 `t` 时刻的风险集大小）
//! 是 KM 调整后的有效完成数——截尾在 `(t, cap]` 之间的个体按 KM 的独立截尾假设分摊，
//! 不当作"没完成"。时间按 DDSketch 的对数桶量化（与 [`crate::survival`] 相同），候选时刻是桶的上界。
//!
//! [`quantile_bounds`]：分位数 `q(p)` 的无分布置信界（W. J. Conover, Practical Nonparametric
//! Statistics, 3rd ed., 1999, §3.2）：秩 `np ± z(1−δ)·√(np(1−p))` 处的次序统计量；秩越出样本时
//! 返回 `None`——样本还不够回答这个分位数（信息不足），而不是设最小样本数。

use crate::beta::wilson;
use crate::sketch::Sketch;
use crate::{Delta, Stat};

/// 一类等待的完成时间（单位由调用方定，常用"秒"或"占上限的比例"）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Remaining {
    done: Sketch,
    cut: Sketch,
}

impl Remaining {
    /// 在 `t` 等到了结果。
    pub fn done(&mut self, t: f64) {
        self.done.push(t);
    }

    /// 等到 `t` 放弃了（超时、被别的条件结束）：右截尾。
    pub fn cut(&mut self, t: f64) {
        self.cut.push(t);
    }

    pub fn count(&self) -> u64 {
        self.done.count() + self.cut.count()
    }

    pub fn merge(&mut self, o: &Self) {
        self.done.merge(&o.done);
        self.cut.merge(&o.cut);
    }

    /// 合并后的时间轴：`(时刻, 事件数, 截尾数)`，时刻升序，零值在最前。
    fn steps(&self) -> Vec<(f64, f64, f64)> {
        let mut out = vec![(0.0, self.done.zeros() as f64, self.cut.zeros() as f64)];
        let mut e = self.done.buckets().peekable();
        let mut c = self.cut.buckets().peekable();
        loop {
            let i = match (e.peek(), c.peek()) {
                (None, None) => break,
                (Some(&(a, _)), None) => a,
                (None, Some(&(b, _))) => b,
                (Some(&(a, _)), Some(&(b, _))) => a.min(b),
            };
            let d = e.next_if(|&(k, _)| k == i).map_or(0, |(_, n)| n);
            let k = c.next_if(|&(k, _)| k == i).map_or(0, |(_, n)| n);
            out.push((self.done.upper(i), d as f64, k as f64));
        }
        out
    }

    /// 最早的放弃时刻 `t ≤ cap`：仍在等的个体里，`cap` 之前能完成的比例的 Wilson 上界 `< δ`。
    /// 找不到（数据不够或确实值得等到 `cap`）时返回 `None`，调用方保持上限不变。
    pub fn give_up_at(&self, cap: f64, delta: Delta) -> Option<f64> {
        let steps = self.steps();
        // 每个台阶之后的 KM 生存值与风险集
        let mut at_risk = self.count() as f64;
        let mut s = 1.0;
        let mut after = Vec::with_capacity(steps.len());
        let mut s_cap = 1.0;
        for &(time, d, c) in &steps {
            if at_risk > 0.0 && d > 0.0 {
                s *= 1.0 - d / at_risk;
            }
            at_risk -= d + c;
            if time <= cap {
                s_cap = s;
            }
            after.push((time, s, at_risk.max(0.0)));
        }
        // 候选 t = 0（还没开始等）和每个台阶时刻：在 t 时刻仍在风险集里的是完成/截尾时间 > t 的个体
        let start = (0.0, 1.0, self.count() as f64);
        std::iter::once(start)
            .chain(after.iter().copied())
            .take_while(|&(time, _, _)| time < cap)
            .find(|&(_, st, n)| {
                if n <= 0.0 || st <= 0.0 {
                    return false;
                }
                let p = (1.0 - s_cap / st).clamp(0.0, 1.0);
                wilson(p * n, n, delta).1 < delta.get()
            })
            .map(|(time, _, _)| time)
    }

    /// 在 `t` 处放弃是否已被数据显著否定：仍在等的个体里 `cap` 之前能完成的比例的 Wilson 下界 `> δ`。
    pub fn too_early(&self, t: f64, cap: f64, delta: Delta) -> bool {
        let steps = self.steps();
        let mut at_risk = self.count() as f64;
        let (mut s, mut s_t, mut n_t, mut s_cap) = (1.0, 1.0, at_risk, 1.0);
        for &(time, d, c) in &steps {
            if at_risk > 0.0 && d > 0.0 {
                s *= 1.0 - d / at_risk;
            }
            at_risk -= d + c;
            if time <= t {
                s_t = s;
                n_t = at_risk.max(0.0);
            }
            if time <= cap {
                s_cap = s;
            }
        }
        if n_t <= 0.0 || s_t <= 0.0 {
            return false;
        }
        let p = (1.0 - s_cap / s_t).clamp(0.0, 1.0);
        wilson(p * n_t, n_t, delta).0 > delta.get()
    }
}

/// 分位数 `q(p)` 的 `(下界, 估计, 上界)`，置信水平各为单侧 `1−δ`；秩越出样本的一侧为 `None`。
pub fn quantile_bounds(s: &Sketch, p: f64, delta: Delta) -> (Option<f64>, f64, Option<f64>) {
    let n = s.count() as f64;
    if n <= 0.0 {
        return (None, f64::NAN, None);
    }
    let half = delta.z() * (p * (1.0 - p) / n).sqrt();
    let side = |q: f64| (0.0..=1.0).contains(&q).then(|| s.quantile(q));
    // 秩恰好落在最后一个（第一个）样本之外时次序统计量不存在
    let hi = p + half;
    let lo = p - half;
    let upper = if hi * n > n - 1.0 { None } else { side(hi) };
    let lower = if lo * n < 0.0 { None } else { side(lo) };
    (lower, s.quantile(p), upper)
}

/// 按精度截窗的分位数（与 [`crate::window::WindowMean`] 同一约定）：
/// 窗口里 `q(p)` 的标准误（由置信界宽度换算，`(上界−下界)/(2z)`）小于 `δ·q̂` 时发布并开新窗口。
#[derive(Debug, Clone)]
pub struct WindowQuantile {
    p: f64,
    sketch: Sketch,
}

/// 一个窗口发布的分位数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Published {
    pub lower: f64,
    pub estimate: f64,
    pub upper: f64,
}

impl WindowQuantile {
    pub fn new(p: f64) -> Self {
        Self {
            p,
            sketch: Sketch::default(),
        }
    }

    pub fn count(&self) -> u64 {
        self.sketch.count()
    }

    /// 加一个样本，不检查是否发布（分片各自攒、汇总时再检查）。
    pub fn add(&mut self, x: f64) {
        self.sketch.push(x);
    }

    /// 并进另一个窗口（例如别的分片），不检查是否发布。
    pub fn merge(&mut self, o: &Self) {
        self.sketch.merge(&o.sketch);
    }

    /// 加一个样本；窗口满足精度时返回这个窗口的分位数并清空。
    pub fn push(&mut self, x: f64, delta: Delta) -> Option<Published> {
        self.add(x);
        self.ready(delta)
    }

    /// 窗口满足精度时返回这个窗口的分位数并清空。
    pub fn ready(&mut self, delta: Delta) -> Option<Published> {
        let (lo, q, hi) = quantile_bounds(&self.sketch, self.p, delta);
        let (lower, upper) = (lo?, hi?);
        let se = (upper - lower) / (2.0 * delta.z());
        if q > 0.0 && se < delta.get() * q {
            self.sketch = Sketch::new(self.sketch.alpha());
            return Some(Published {
                lower,
                estimate: q,
                upper,
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    fn exp(rng: &mut Rng, mean: f64) -> f64 {
        -mean * rng.uniform().max(1e-300).ln()
    }

    /// 混合：90% 的等待在 Exp(0.2) 内完成，10% 永远不完成（长轮询），上限 5：
    /// 等过 ~1.5 之后仍在等的几乎都是不会完成的，放弃时刻应远早于上限；
    /// 放弃造成的损失（本可在上限内完成的）不超过全部的 δ。
    #[test]
    fn gives_up_when_the_remaining_mass_is_negligible() {
        let d = Delta::DEFAULT;
        let mut rng = Rng::seed(7);
        let mut r = Remaining::default();
        let cap = 5.0;
        let mut times = Vec::new();
        for _ in 0..20_000 {
            if rng.uniform() < 0.9 {
                let t: f64 = exp(&mut rng, 0.2);
                times.push(t);
                if t < cap { r.done(t) } else { r.cut(cap) }
            } else {
                r.cut(cap);
            }
        }
        let t = r.give_up_at(cap, d).expect("should give up early");
        assert!(t < 2.0 && t > 0.3, "{t}");
        let lost = times.iter().filter(|&&x| x > t && x < cap).count() as f64 / 20_000.0;
        assert!(lost < d.get(), "{lost}");
        assert!(!r.too_early(t, cap, d));
        assert!(r.too_early(0.05, cap, d));
    }

    /// 全部会在上限前完成、且均匀铺满整个区间：任何 t 都不该放弃。
    #[test]
    fn keeps_waiting_when_completions_continue_until_the_cap() {
        let d = Delta::DEFAULT;
        let mut rng = Rng::seed(8);
        let mut r = Remaining::default();
        for _ in 0..5_000 {
            r.done(5.0 * rng.uniform());
        }
        let t = r.give_up_at(5.0, d);
        // 只在最后极小的一段（剩余风险集里完成的比例本身就小于 δ）才可能放弃
        assert!(t.is_none_or(|t| t > 4.7), "{t:?}");
        // 冷启动：没有数据就不放弃
        assert_eq!(Remaining::default().give_up_at(5.0, d), None);
    }

    #[test]
    fn quantile_bounds_cover_and_window_publishes() {
        let d = Delta::DEFAULT;
        let mut rng = Rng::seed(9);
        let mut s = Sketch::default();
        for _ in 0..10 {
            s.push(rng.uniform());
        }
        // 10 个样本答不了 q(0.95) 的上界
        assert!(quantile_bounds(&s, 0.95, d).2.is_none());
        let mut w = WindowQuantile::new(0.95);
        let mut got = None;
        for _ in 0..200_000 {
            if let Some(p) = w.push(exp(&mut rng, 1.0), d) {
                got = Some(p);
                break;
            }
        }
        let p = got.expect("window should publish");
        let exact = 20f64.ln();
        assert!(p.lower <= exact * 1.02 && exact <= p.upper * 1.02, "{p:?}");
        assert!((p.estimate - exact).abs() < 0.2 * exact, "{p:?}");
        assert_eq!(w.count(), 0);
    }
}
