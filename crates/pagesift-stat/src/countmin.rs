//! Count-Min 频率草图。键是调用方算好的 64 位哈希。

use crate::rng::Rng;
use crate::{DecodeError, Delta, Reader, Stat, Writer};

/// [推导] Mersenne 素数 2⁶¹−1：Carter–Wegman 两两独立哈希 `((a·x + b) mod p) mod w` 的模数
const P61_BITS: u32 = 61;

const P61: u64 = (1 << P61_BITS) - 1;

fn mod_p61(x: u128) -> u64 {
    // 2⁶¹ ≡ 1 (mod p)：高位折回低位
    let lo = (x as u64) & P61;
    let hi = (x >> P61_BITS) as u64;
    let s = lo + (hi & P61) + (hi >> P61_BITS);
    let s = (s & P61) + (s >> P61_BITS);
    if s >= P61 { s - P61 } else { s }
}

/// Count-Min 草图（G. Cormode, S. Muthukrishnan, "An Improved Data Stream Summary: The Count-Min
/// Sketch and its Applications", J. Algorithms 55(1), 2005）。
///
/// 宽 `w = ⌈e/ε⌉`、深 `d = ⌈ln(1/δ)⌉` 时，估计值 `â ≥ a`，且以至少 `1−δ` 的概率
/// `â ≤ a + ε‖a‖₁`（原文 Theorem 1）。合并要求两边的哈希种子和尺寸相同，逐格相加。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CountMin {
    width: usize,
    seed: u64,
    hashes: Vec<(u64, u64)>,
    cells: Vec<u64>,
    total: u64,
}

impl CountMin {
    /// 相对误差 `eps`（相对总计数）和失败概率 `δ`。
    pub fn new(eps: f64, delta: Delta, seed: u64) -> Self {
        let width = (std::f64::consts::E / eps).ceil() as usize;
        let depth = (1.0 / delta.get()).ln().ceil() as usize;
        let mut rng = Rng::seed(seed);
        let hashes = (0..depth)
            .map(|_| (rng.next_u64() % (P61 - 1) + 1, rng.next_u64() % P61))
            .collect();
        Self {
            width,
            seed,
            hashes,
            cells: vec![0; width * depth],
            total: 0,
        }
    }

    fn cell(&self, row: usize, key: u64) -> usize {
        let (a, b) = self.hashes[row];
        let h = mod_p61(a as u128 * (key % P61) as u128 + b as u128);
        row * self.width + (h % self.width as u64) as usize
    }

    pub fn add(&mut self, key: u64, n: u64) {
        for row in 0..self.hashes.len() {
            let c = self.cell(row, key);
            self.cells[c] += n;
        }
        self.total += n;
    }

    /// 频率估计（只会高估）。
    pub fn estimate(&self, key: u64) -> u64 {
        (0..self.hashes.len())
            .map(|row| self.cells[self.cell(row, key)])
            .min()
            .unwrap_or(0)
    }

    pub fn total(&self) -> u64 {
        self.total
    }
}

impl Stat for CountMin {
    fn merge(&mut self, o: &Self) {
        if o.total == 0 {
            return;
        }
        if self.cells.is_empty() {
            *self = o.clone();
            return;
        }
        assert!(self.seed == o.seed && self.width == o.width && self.hashes == o.hashes);
        self.cells
            .iter_mut()
            .zip(&o.cells)
            .for_each(|(a, b)| *a += b);
        self.total += o.total;
    }

    fn encode(&self, w: &mut Writer) {
        w.u64(self.width as u64);
        w.u64(self.seed);
        w.u64(self.hashes.len() as u64);
        w.u64s(&self.cells);
        w.u64(self.total);
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let width = r.u64()? as usize;
        let seed = r.u64()?;
        let depth = r.u64()? as usize;
        let cells = r.u64s()?;
        if cells.len() != width.saturating_mul(depth) {
            return Err(DecodeError::Invalid("count-min dims"));
        }
        let mut rng = Rng::seed(seed);
        let hashes = (0..depth)
            .map(|_| (rng.next_u64() % (P61 - 1) + 1, rng.next_u64() % P61))
            .collect();
        Ok(Self {
            width,
            seed,
            hashes,
            cells,
            total: r.u64()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mix(x: u64) -> u64 {
        Rng::seed(x).next_u64()
    }

    #[test]
    fn count_min_never_underestimates_and_bounds_overestimates() {
        let eps = 0.01;
        let mut cm = CountMin::new(eps, Delta::DEFAULT, 9);
        for k in 0..5000u64 {
            cm.add(mix(k), k % 7 + 1);
        }
        let n = cm.total() as f64;
        let over = (0..5000u64)
            .filter(|&k| cm.estimate(mix(k)) as f64 > (k % 7 + 1) as f64 + eps * n)
            .count();
        assert!((0..5000u64).all(|k| cm.estimate(mix(k)) > k % 7));
        assert!((over as f64) < 5000.0 * Delta::DEFAULT.get(), "{over}");
    }
}
