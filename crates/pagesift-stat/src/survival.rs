//! 含截尾数据的完成时间分布：Kaplan–Meier 乘积限估计。
//!
//! E. L. Kaplan, P. Meier, "Nonparametric Estimation from Incomplete Observations",
//! JASA 53(282), 1958：`Ŝ(t) = Π_{tᵢ ≤ t} (1 − dᵢ/nᵢ)`，`dᵢ` 是 `tᵢ` 处的事件数，
//! `nᵢ` 是 `tᵢ` 前仍在风险中的个数；同一时刻的截尾视为发生在事件之后（原文 §2.1 的约定）。
//! 方差用 Greenwood 公式 `Var Ŝ(t) = Ŝ(t)² Σ dᵢ/(nᵢ(nᵢ−dᵢ))`。
//!
//! 为了可合并、内存有界，时间按 DDSketch 的对数桶量化（[`crate::sketch`]）：事件和截尾各一个草图，
//! 桶 `i` 覆盖 `(γ^{i−1}, γ^i]`，在 `γ^i` 处计数。由此带来的时间相对误差不超过 `α`；
//! 这偏离了原文对精确时间点的定义，换来的是合并就是桶计数相加。

use crate::sketch::Sketch;
use crate::{DecodeError, Reader, Stat, Writer};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Survival {
    events: Sketch,
    censored: Sketch,
}

/// 生存函数上的一个台阶。
#[derive(Debug, Clone, Copy, PartialEq)]
struct Step {
    time: f64,
    survival: f64,
    /// Greenwood 和式 `Σ dᵢ/(nᵢ(nᵢ−dᵢ))` 到这一步为止的值。
    greenwood: f64,
}

impl Survival {
    pub fn with_alpha(alpha: f64) -> Self {
        Self {
            events: Sketch::new(alpha),
            censored: Sketch::new(alpha),
        }
    }

    /// 在 `t` 时刻完成（事件发生）。
    pub fn event(&mut self, t: f64) {
        self.events.push(t);
    }

    /// 等到 `t` 仍未完成（超时、取消）：右截尾。
    pub fn censor(&mut self, t: f64) {
        self.censored.push(t);
    }

    pub fn count(&self) -> u64 {
        self.events.count() + self.censored.count()
    }

    pub fn events(&self) -> u64 {
        self.events.count()
    }

    /// 生存函数的全部台阶，时间升序。
    fn steps(&self) -> Vec<Step> {
        let mut at_risk = self.count() as f64;
        let mut s = 1.0;
        let mut g = 0.0;
        let mut out = Vec::new();
        let mut apply = |time: f64, d: f64, c: f64, at_risk: &mut f64| {
            if d > 0.0 && *at_risk > 0.0 {
                s *= 1.0 - d / *at_risk;
                if *at_risk > d {
                    g += d / (*at_risk * (*at_risk - d));
                }
                out.push(Step {
                    time,
                    survival: s,
                    greenwood: g,
                });
            }
            *at_risk -= d + c;
        };
        apply(
            0.0,
            self.events.zeros() as f64,
            self.censored.zeros() as f64,
            &mut at_risk,
        );
        let mut e = self.events.buckets().peekable();
        let mut c = self.censored.buckets().peekable();
        loop {
            let i = match (e.peek(), c.peek()) {
                (None, None) => break,
                (Some(&(a, _)), None) => a,
                (None, Some(&(b, _))) => b,
                (Some(&(a, _)), Some(&(b, _))) => a.min(b),
            };
            let d = e.next_if(|&(k, _)| k == i).map_or(0, |(_, n)| n);
            let cc = c.next_if(|&(k, _)| k == i).map_or(0, |(_, n)| n);
            apply(self.events.upper(i), d as f64, cc as f64, &mut at_risk);
        }
        out
    }

    /// `Ŝ(t)`：等到 `t` 仍未完成的概率。
    pub fn survival(&self, t: f64) -> f64 {
        self.steps()
            .iter()
            .take_while(|s| s.time <= t)
            .last()
            .map_or(1.0, |s| s.survival)
    }

    /// `Ŝ(t)` 与 Greenwood 标准误。
    pub fn survival_with_se(&self, t: f64) -> (f64, f64) {
        match self.steps().iter().take_while(|s| s.time <= t).last() {
            Some(s) => (s.survival, s.survival * s.greenwood.sqrt()),
            None => (1.0, 0.0),
        }
    }

    /// 已经等了 `t`，再等 `dt` 能完成的条件概率 `h(t, Δ) = [S(t) − S(t+Δ)] / S(t)`（DESIGN §5.4-W）。
    /// `S(t) = 0`（观测里没有等这么久的）时返回 NaN：信息不足，由调用方按上层先验处理。
    pub fn finish_within(&self, t: f64, dt: f64) -> f64 {
        let steps = self.steps();
        let at = |x: f64| {
            steps
                .iter()
                .take_while(|s| s.time <= x)
                .last()
                .map_or(1.0, |s| s.survival)
        };
        let st = at(t);
        if st <= 0.0 {
            return f64::NAN;
        }
        (st - at(t + dt)) / st
    }

    /// 完成概率第一次达到 `p` 的时刻：最小的 `t` 使 `1 − Ŝ(t) ≥ p`。
    /// 截尾太多、估计到不了 `p` 时返回 `None`。
    pub fn time_to(&self, p: f64) -> Option<f64> {
        self.steps()
            .into_iter()
            .find(|s| 1.0 - s.survival >= p)
            .map(|s| s.time)
    }
}

impl Stat for Survival {
    fn merge(&mut self, o: &Self) {
        self.events.merge(&o.events);
        self.censored.merge(&o.censored);
    }

    fn encode(&self, w: &mut Writer) {
        self.events.encode(w);
        self.censored.encode(w);
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let events = Sketch::decode(r)?;
        let censored = Sketch::decode(r)?;
        if events.alpha() != censored.alpha() {
            return Err(DecodeError::Invalid("survival alpha"));
        }
        Ok(Self { events, censored })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    /// 指数分布 `S(t) = e^{−t}`，独立的指数截尾 `C ~ Exp(½)`：
    /// KM 在截尾下仍应贴近 `e^{−t}`（原文 §3 的相合性）。
    #[test]
    fn km_recovers_the_exponential_under_censoring() {
        let mut rng = Rng::seed(3);
        let mut s = Survival::with_alpha(0.001);
        let mut halves = (Survival::with_alpha(0.001), Survival::with_alpha(0.001));
        for i in 0..40_000 {
            let t = -rng.uniform().max(1e-300).ln();
            let c = -2.0 * rng.uniform().max(1e-300).ln();
            let part = if i % 2 == 0 {
                &mut halves.0
            } else {
                &mut halves.1
            };
            if t <= c {
                s.event(t);
                part.event(t);
            } else {
                s.censor(c);
                part.censor(c);
            }
        }
        for t in [0.1, 0.5, 1.0, 2.0] {
            let (est, se) = s.survival_with_se(t);
            let exact = (-t).exp();
            assert!(
                (est - exact).abs() < 4.0 * se + 0.002,
                "t={t} {est} vs {exact} se={se}"
            );
        }
        let q = s.time_to(0.5).unwrap();
        assert!((q - std::f64::consts::LN_2).abs() < 0.03, "{q}");
        // 指数分布无记忆：h(t, Δ) 与 t 无关，等于 1 − e^{−Δ}
        let h = s.finish_within(1.0, 0.5);
        assert!((h - (1.0 - (-0.5f64).exp())).abs() < 0.03, "{h}");
        halves.0.merge(&halves.1);
        assert_eq!(halves.0, s);
        let mut w = Writer::new();
        s.encode(&mut w);
        assert_eq!(Survival::decode(&mut Reader::new(w.as_slice())).unwrap(), s);
    }
}
