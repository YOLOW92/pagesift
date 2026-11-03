//! 可合并的分位数草图：DDSketch。
//!
//! C. Masson, J. E. Rim, H. K. Lee, "DDSketch: A Fast and Fully-Mergeable Quantile Sketch with
//! Relative-Error Guarantees", PVLDB 12(12), 2019。
//!
//! 正值 `x` 落进桶 `i = ⌈log_γ x⌉`，`γ = (1+α)/(1−α)`；桶 `i` 的代表值
//! `2γ^i/(γ+1)` 与桶内任意值的相对误差不超过 `α`（原文 Lemma 2）。
//! 两个 `α` 相同的草图按桶计数相加即合并（原文 §2.2）。本实现只收非负值（延迟、大小、字数）。

use crate::special::normal_quantile;
use crate::{DecodeError, Delta, Reader, Stat, Writer};

#[derive(Debug, Clone, PartialEq)]
pub struct Sketch {
    alpha: f64,
    ln_gamma: f64,
    /// `counts[k]` 是桶 `offset + k` 的计数。
    offset: i32,
    counts: Vec<u64>,
    zeros: u64,
    count: u64,
    min: f64,
    max: f64,
}

impl Default for Sketch {
    /// 默认相对误差取 [`default_alpha`]。
    fn default() -> Self {
        Self::new(default_alpha())
    }
}

/// [推导] 默认 `α`：取 [`matched_alpha`] 的公式 `ε ≈ z(1−δ)·√(p(1−p)/n) / (f(q)·q)` 在 `p = 1−δ`、
/// `n` 为单个路径族常见样本量量级时的值。对指数型分布 `f(q)·q = (1−p)·ln(1/(1−p))`，
/// `δ = 0.05`、`n ≈ 10³` 时 `ε ≈ 0.6`；分位数本身的统计误差已经远大于 1%，
/// 取 `α = 0.01` 保证草图误差可以忽略，且每十倍跨度只需约 115 个桶。
pub fn default_alpha() -> f64 {
    0.01 // [推导] 见上
}

/// 与样本分位数本身统计误差同量级的相对误差
/// `ε ≈ z(1−δ)·√(p(1−p)/n) / (f(q)·q)`；`density_times_q` 是分位点处的 `f(q)·q`。
pub fn matched_alpha(p: f64, n: f64, density_times_q: f64, delta: Delta) -> f64 {
    normal_quantile(1.0 - delta.get()) * (p * (1.0 - p) / n).sqrt() / density_times_q
}

impl Sketch {
    /// `alpha ∈ (0, 1)`：分位数估计的相对误差上界。
    pub fn new(alpha: f64) -> Self {
        assert!(
            alpha > 0.0 && alpha < 1.0,
            "DDSketch alpha must be in (0,1), got {alpha}"
        );
        let gamma = (1.0 + alpha) / (1.0 - alpha);
        Self {
            alpha,
            ln_gamma: gamma.ln(),
            offset: 0,
            counts: Vec::new(),
            zeros: 0,
            count: 0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
        }
    }

    pub fn alpha(&self) -> f64 {
        self.alpha
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn min(&self) -> f64 {
        self.min
    }

    pub fn max(&self) -> f64 {
        self.max
    }

    /// 非空桶 `(下标, 计数)`，按下标升序；零值另计（[`zeros`](Self::zeros)）。
    pub(crate) fn buckets(&self) -> impl Iterator<Item = (i32, u64)> + '_ {
        self.counts
            .iter()
            .enumerate()
            .filter(|(_, c)| **c > 0)
            .map(|(k, &c)| (self.offset + k as i32, c))
    }

    pub(crate) fn zeros(&self) -> u64 {
        self.zeros
    }

    /// 桶 `i` 的上界 `γ^i`。
    pub(crate) fn upper(&self, i: i32) -> f64 {
        (i as f64 * self.ln_gamma).exp()
    }

    pub(crate) fn index(&self, x: f64) -> i32 {
        (x.ln() / self.ln_gamma).ceil() as i32
    }

    fn add_to_bucket(&mut self, i: i32, n: u64) {
        if self.counts.is_empty() {
            self.offset = i;
            self.counts.push(0);
        } else if i < self.offset {
            let grow = (self.offset - i) as usize;
            self.counts.splice(0..0, std::iter::repeat_n(0, grow));
            self.offset = i;
        } else if (i - self.offset) as usize >= self.counts.len() {
            self.counts.resize((i - self.offset) as usize + 1, 0);
        }
        self.counts[(i - self.offset) as usize] += n;
    }

    /// 加一个非负观测；负数和 NaN 忽略。
    pub fn push(&mut self, x: f64) {
        if x.is_nan() || x < 0.0 || !x.is_finite() {
            return;
        }
        self.count += 1;
        self.min = self.min.min(x);
        self.max = self.max.max(x);
        if x < f64::MIN_POSITIVE {
            self.zeros += 1;
        } else {
            let i = self.index(x);
            self.add_to_bucket(i, 1);
        }
    }

    /// `p` 分位数（原文 Algorithm 2）：秩 `⌊p(n−1)⌋` 所在桶的代表值；结果夹在 `[min, max]`。
    pub fn quantile(&self, p: f64) -> f64 {
        if self.count == 0 {
            return f64::NAN;
        }
        let rank = (p.clamp(0.0, 1.0) * (self.count - 1) as f64).floor() as u64;
        if rank < self.zeros {
            return 0.0;
        }
        let mut seen = self.zeros;
        let gamma = self.ln_gamma.exp();
        for (k, &c) in self.counts.iter().enumerate() {
            seen += c;
            if seen > rank {
                let i = self.offset + k as i32;
                let v = 2.0 * (i as f64 * self.ln_gamma).exp() / (gamma + 1.0);
                return v.clamp(self.min, self.max);
            }
        }
        self.max
    }
}

impl Stat for Sketch {
    /// 只有 `α` 相同的草图才能合并；`α` 不同是调用方的错误。
    fn merge(&mut self, o: &Self) {
        if o.count == 0 {
            return;
        }
        assert!(
            self.alpha == o.alpha,
            "merging DDSketches with different alpha"
        );
        for (k, &c) in o.counts.iter().enumerate() {
            if c > 0 {
                self.add_to_bucket(o.offset + k as i32, c);
            }
        }
        self.zeros += o.zeros;
        self.count += o.count;
        self.min = self.min.min(o.min);
        self.max = self.max.max(o.max);
    }

    fn encode(&self, w: &mut Writer) {
        w.f64(self.alpha);
        w.i64(self.offset as i64);
        w.u64s(&self.counts);
        w.u64(self.zeros);
        w.u64(self.count);
        w.f64(self.min);
        w.f64(self.max);
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let alpha = r.f64()?;
        if !(alpha > 0.0 && alpha < 1.0) {
            return Err(DecodeError::Invalid("sketch alpha"));
        }
        let mut s = Self::new(alpha);
        s.offset = i32::try_from(r.i64()?).map_err(|_| DecodeError::Invalid("sketch offset"))?;
        s.counts = r.u64s()?;
        s.zeros = r.u64()?;
        s.count = r.u64()?;
        s.min = r.f64()?;
        s.max = r.f64()?;
        if s.counts.iter().sum::<u64>() + s.zeros != s.count {
            return Err(DecodeError::Invalid("sketch counts"));
        }
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_error_is_within_alpha_and_merge_equals_batch() {
        let mut rng = crate::rng::Rng::seed(1);
        let xs: Vec<f64> = (0..20_000).map(|_| (2.0 * rng.normal()).exp()).collect();
        let mut sorted = xs.clone();
        sorted.sort_by(f64::total_cmp);
        let alpha = 0.01;
        let mut all = Sketch::new(alpha);
        let (mut a, mut b) = (Sketch::new(alpha), Sketch::new(alpha));
        for (i, &x) in xs.iter().enumerate() {
            all.push(x);
            if i % 3 == 0 { a.push(x) } else { b.push(x) }
        }
        a.merge(&b);
        assert_eq!(a, all);
        for p in [0.0, 0.01, 0.25, 0.5, 0.9, 0.95, 0.99, 1.0] {
            let exact = sorted[(p * (sorted.len() - 1) as f64).floor() as usize];
            let est = all.quantile(p);
            assert!(
                (est - exact).abs() <= alpha * exact * (1.0 + 1e-12),
                "p={p} {est} vs {exact}"
            );
        }
        let mut w = Writer::new();
        all.encode(&mut w);
        assert_eq!(Sketch::decode(&mut Reader::new(w.as_slice())).unwrap(), all);
    }
}
