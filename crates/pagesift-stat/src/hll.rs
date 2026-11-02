//! HyperLogLog 基数估计。键是调用方算好的 64 位哈希。

use crate::{DecodeError, Reader, Stat, Writer};

/// HyperLogLog（P. Flajolet, É. Fusy, O. Gandouet, F. Meunier, AofA 2007）。
///
/// `m = 2^b` 个寄存器，标准误约 `1.04/√m`（原文 Theorem 1）。64 位哈希下不需要大范围修正；
/// 小范围（`E ≤ 5m/2` 且有空寄存器）用线性计数 `m·ln(m/V)`（原文 Fig. 3）。合并逐格取最大。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HyperLogLog {
    bits: u8,
    regs: Vec<u8>,
}

impl HyperLogLog {
    /// 目标相对标准误 `err`：取最小的 `b` 使 `1.04/√(2^b) ≤ err`。
    pub fn with_error(err: f64) -> Self {
        // [推导] 原文 Theorem 1 的常数 1.04；b 的下限 4 是原文 α_m 表的起点（m = 16）
        let m = (1.04 / err).powi(2);
        let bits = (m.log2().ceil() as u8).clamp(4, 18); // [推导] 下限见上；上限 18 位时标准误 0.2%，2¹⁸ 字节
        Self {
            bits,
            regs: vec![0; 1 << bits],
        }
    }

    /// 加一个已哈希的键。调用方的哈希要充分混合（例如对字符串用 SipHash）。
    pub fn add(&mut self, hash: u64) {
        let idx = (hash >> (u64::BITS - self.bits as u32)) as usize;
        let rest = hash << self.bits;
        // 剩余 64−b 位里第一个 1 的位置；全 0 时取最大值 64−b+1
        let rank = (rest.leading_zeros() + 1).min(u64::BITS - self.bits as u32 + 1) as u8;
        if rank > self.regs[idx] {
            self.regs[idx] = rank;
        }
    }

    pub fn estimate(&self) -> f64 {
        let m = self.regs.len() as f64;
        // [推导] α_m：m = 16/32/64 用原文表值 0.673/0.697/0.709，更大时 0.7213/(1 + 1.079/m)
        let alpha = match self.regs.len() {
            // [推导] 原文表值与渐近式
            16 => 0.673,
            32 => 0.697,
            64 => 0.709,
            _ => 0.7213 / (1.0 + 1.079 / m),
        };
        let sum: f64 = self.regs.iter().map(|&r| (-(r as f64)).exp2()).sum();
        let e = alpha * m * m / sum;
        let zeros = self.regs.iter().filter(|&&r| r == 0).count();
        // [推导] 原文 Fig. 3 的小范围修正阈值 5m/2
        if e <= 2.5 * m && zeros > 0 {
            m * (m / zeros as f64).ln()
        } else {
            e
        }
    }
}

impl Stat for HyperLogLog {
    fn merge(&mut self, o: &Self) {
        if o.regs.is_empty() {
            return;
        }
        if self.regs.is_empty() {
            *self = o.clone();
            return;
        }
        assert_eq!(
            self.bits, o.bits,
            "merging HyperLogLogs of different precision"
        );
        self.regs
            .iter_mut()
            .zip(&o.regs)
            .for_each(|(a, &b)| *a = (*a).max(b));
    }

    fn encode(&self, w: &mut Writer) {
        w.u8(self.bits);
        w.bytes(&self.regs);
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let bits = r.u8()?;
        let regs = r.bytes()?.to_vec();
        if u32::from(bits) >= u64::BITS || regs.len() != 1usize << bits {
            return Err(DecodeError::Invalid("hyperloglog dims"));
        }
        Ok(Self { bits, regs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    fn mix(x: u64) -> u64 {
        Rng::seed(x).next_u64()
    }

    #[test]
    fn hll_error_and_merge() {
        let mut a = HyperLogLog::with_error(0.02);
        let mut b = HyperLogLog::with_error(0.02);
        for k in 0..100_000u64 {
            if k % 2 == 0 {
                a.add(mix(k))
            } else {
                b.add(mix(k))
            }
        }
        a.merge(&b);
        let est = a.estimate();
        assert!((est - 100_000.0).abs() < 3.0 * 0.02 * 100_000.0, "{est}");
        let mut small = HyperLogLog::with_error(0.02);
        (0..50u64).for_each(|k| small.add(mix(k)));
        assert!(
            (small.estimate() - 50.0).abs() < 3.0,
            "{}",
            small.estimate()
        );
        let mut w = Writer::new();
        a.encode(&mut w);
        assert_eq!(
            HyperLogLog::decode(&mut Reader::new(w.as_slice())).unwrap(),
            a
        );
    }
}
