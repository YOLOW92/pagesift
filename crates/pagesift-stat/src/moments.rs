//! 均值和方差的在线计算与并行合并。

use crate::{DecodeError, Reader, Stat, Writer};

/// 计数、均值和离差平方和 `M₂`。
///
/// 更新：Welford（Technometrics 4(3), 1962）。合并：Chan, Golub, LeVeque（1979；
/// The American Statistician 37(3), 1983）的成对公式
/// `n = n₁+n₂，d = μ₂−μ₁，μ = μ₁ + d·n₂/n，M₂ = M₂₁ + M₂₂ + d²·n₁n₂/n`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Moments {
    n: u64,
    mean: f64,
    m2: f64,
}

impl Moments {
    pub fn push(&mut self, x: f64) {
        self.n += 1;
        let d = x - self.mean;
        self.mean += d / self.n as f64;
        self.m2 += d * (x - self.mean);
    }

    pub fn count(&self) -> u64 {
        self.n
    }

    pub fn mean(&self) -> f64 {
        self.mean
    }

    /// 无偏样本方差 `M₂/(n−1)`；少于 2 个样本时为 NaN（信息不足，不是 0）。
    pub fn variance(&self) -> f64 {
        if self.n < 2 {
            return f64::NAN;
        }
        self.m2 / (self.n - 1) as f64
    }

    pub fn std_dev(&self) -> f64 {
        self.variance().sqrt()
    }

    /// 均值的标准误 `s/√n`。
    pub fn std_err(&self) -> f64 {
        (self.variance() / self.n as f64).sqrt()
    }
}

impl Stat for Moments {
    fn merge(&mut self, o: &Self) {
        if o.n == 0 {
            return;
        }
        if self.n == 0 {
            *self = o.clone();
            return;
        }
        let n = self.n + o.n;
        let (n1, n2, nf) = (self.n as f64, o.n as f64, n as f64);
        let d = o.mean - self.mean;
        self.mean += d * n2 / nf;
        self.m2 += o.m2 + d * d * n1 * n2 / nf;
        self.n = n;
    }

    fn encode(&self, w: &mut Writer) {
        w.u64(self.n);
        w.f64(self.mean);
        w.f64(self.m2);
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let s = Self {
            n: r.u64()?,
            mean: r.f64()?,
            m2: r.f64()?,
        };
        if s.m2 < 0.0 || !s.mean.is_finite() {
            return Err(DecodeError::Invalid("moments"));
        }
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_is_associative_and_matches_the_batch() {
        let xs: Vec<f64> = (0..300)
            .map(|i| ((i * 37) % 101) as f64 * 0.5 + 1e6)
            .collect();
        let mut all = Moments::default();
        xs.iter().for_each(|&x| all.push(x));
        let part = |r: std::ops::Range<usize>| {
            let mut m = Moments::default();
            xs[r].iter().for_each(|&x| m.push(x));
            m
        };
        let (a, b, c) = (part(0..10), part(10..200), part(200..300));
        let mut left = a.clone();
        left.merge(&b);
        left.merge(&c);
        let mut bc = b.clone();
        bc.merge(&c);
        let mut right = a.clone();
        right.merge(&bc);
        for m in [&left, &right] {
            assert_eq!(m.count(), all.count());
            assert!((m.mean() - all.mean()).abs() < 1e-9);
            assert!((m.variance() - all.variance()).abs() < 1e-9 * all.variance());
        }
        let mut w = Writer::new();
        all.encode(&mut w);
        assert_eq!(
            Moments::decode(&mut Reader::new(w.as_slice())).unwrap(),
            all
        );
    }
}
