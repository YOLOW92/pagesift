//! 决策用的伪随机数：xoshiro256**，以及正态、伽马、贝塔抽样。
//!
//! 只用于 Thompson 采样和扰动，不用于安全用途。

/// xoshiro256**（D. Blackman, S. Vigna, "Scrambled Linear Pseudorandom Number Generators",
/// ACM TOMS 2021）。种子由 SplitMix64 展开（同文 §5 推荐的做法）。
#[derive(Debug, Clone)]
pub struct Rng {
    s: [u64; 4], // [推导] 256 位状态
}

/// [推导] SplitMix64 的状态增量（黄金比例 2^64/φ，Steele, Lea, Flood, OOPSLA 2014）
pub const SPLITMIX64_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// SplitMix64 的一步（G. L. Steele, D. Lea, C. H. Flood, "Fast Splittable Pseudorandom Number
/// Generators", OOPSLA 2014；常数取自 S. Vigna 的参考实现 splitmix64.c）：状态加增量后混合输出。
/// 输出只由输入决定，也当作 64 位整数的混合哈希用。
pub fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(SPLITMIX64_GAMMA);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9); // [推导] SplitMix64 的移位与乘子
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB); // [推导] 同上
    z ^ (z >> 31) // [推导] 同上
}

impl Rng {
    pub fn seed(seed: u64) -> Self {
        let mut x = seed;
        let mut next = || {
            let z = splitmix64(x);
            x = x.wrapping_add(SPLITMIX64_GAMMA);
            z
        };
        Self {
            s: [next(), next(), next(), next()],
        }
    }

    /// 用当前时间和地址熵做种子；每个工人线程各一个。
    pub fn from_entropy() -> Self {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or_default();
        let local = 0u8;
        Self::seed(t ^ (&local as *const u8 as u64))
    }

    pub fn next_u64(&mut self) -> u64 {
        // [推导] xoshiro256** 的输出变换 rotl(s1·5, 7)·9 与状态更新的移位 17、45（原文 §3）
        let out = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17; // [推导] 同上
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45); // [推导] 同上
        out
    }

    /// `[0, 1)` 上的均匀分布：取高 53 位（f64 尾数位数）。
    pub fn uniform(&mut self) -> f64 {
        // [协议] IEEE 754 双精度尾数 53 位，2^-53
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// 开区间 `(0, 1)`，给取对数用。
    fn open_uniform(&mut self) -> f64 {
        loop {
            let u = self.uniform();
            if u > 0.0 {
                return u;
            }
        }
    }

    /// 标准正态：Marsaglia 极坐标法（Marsaglia, Bray, SIAM Review 1964）。
    pub fn normal(&mut self) -> f64 {
        loop {
            let u = 2.0 * self.uniform() - 1.0;
            let v = 2.0 * self.uniform() - 1.0;
            let s = u * u + v * v;
            if s > 0.0 && s < 1.0 {
                return u * (-2.0 * s.ln() / s).sqrt();
            }
        }
    }

    /// `Gamma(shape, 1)`：Marsaglia–Tsang 方法（ACM TOMS 26(3), 2000）。
    /// `shape < 1` 时用 `Gamma(a) = Gamma(a+1)·U^{1/a}`（同文 §6）。
    pub fn gamma(&mut self, shape: f64) -> f64 {
        if shape < 1.0 {
            let u = self.open_uniform();
            return self.gamma(shape + 1.0) * u.powf(1.0 / shape);
        }
        // [推导] 原文的 d = a − 1/3、c = 1/√(9d)，挤压检验常数 0.0331
        let d = shape - 1.0 / 3.0;
        let c = 1.0 / (9.0 * d).sqrt(); // [推导] 同上
        loop {
            let x = self.normal();
            let v = 1.0 + c * x;
            if v <= 0.0 {
                continue;
            }
            let v = v * v * v;
            let u = self.open_uniform();
            let x2 = x * x;
            // [推导] 挤压检验
            if u < 1.0 - 0.0331 * x2 * x2 || u.ln() < 0.5 * x2 + d * (1.0 - v + v.ln()) {
                return d * v;
            }
        }
    }

    /// `Beta(a, b) = X/(X+Y)`，`X ~ Gamma(a)`、`Y ~ Gamma(b)`。
    pub fn beta(&mut self, a: f64, b: f64) -> f64 {
        let x = self.gamma(a);
        let y = self.gamma(b);
        x / (x + y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beta_samples_match_the_mean_and_variance() {
        let mut r = Rng::seed(7);
        let (a, b) = (2.5, 0.7);
        let n = 200_000;
        let xs: Vec<f64> = (0..n).map(|_| r.beta(a, b)).collect();
        let mean = xs.iter().sum::<f64>() / n as f64;
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n as f64;
        let m = a / (a + b);
        let v = a * b / ((a + b).powi(2) * (a + b + 1.0));
        // 均值的标准误 √(v/n) ≈ 4e-4，留 5 倍
        assert!(
            (mean - m).abs() < 5.0 * (v / n as f64).sqrt(),
            "{mean} vs {m}"
        );
        assert!((var - v).abs() < 0.02 * v, "{var} vs {v}");
    }
}
