//! 可合并的线性回归（充分统计量 `XᵀX`、`Xᵀy`、`yᵀy`）与预测区间。

use crate::special::t_quantile;
use crate::{DecodeError, Delta, Reader, Stat, Writer};

/// `k` 维普通最小二乘的充分统计量；合并就是各项相加。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RegressionSums {
    k: usize,
    n: u64,
    /// 行主序 `k×k`。
    xtx: Vec<f64>,
    xty: Vec<f64>,
    yty: f64,
}

/// 拟合结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Fit {
    pub beta: Vec<f64>,
    /// `(XᵀX)⁻¹`，行主序。
    pub inv: Vec<f64>,
    /// 残差方差 `s² = (yᵀy − βᵀXᵀy)/(n−k)`。
    pub s2: f64,
    /// 残差自由度 `n − k`。
    pub dof: f64,
}

impl RegressionSums {
    pub fn new(k: usize) -> Self {
        Self {
            k,
            n: 0,
            xtx: vec![0.0; k * k],
            xty: vec![0.0; k],
            yty: 0.0,
        }
    }

    pub fn dims(&self) -> usize {
        self.k
    }

    pub fn count(&self) -> u64 {
        self.n
    }

    pub fn push(&mut self, x: &[f64], y: f64) {
        assert_eq!(x.len(), self.k, "regression input has wrong dimension");
        for i in 0..self.k {
            for j in 0..self.k {
                self.xtx[i * self.k + j] += x[i] * x[j];
            }
            self.xty[i] += x[i] * y;
        }
        self.yty += y * y;
        self.n += 1;
    }

    /// 解正规方程。样本数不超过维数或 `XᵀX` 奇异（数据没覆盖到某个方向）时返回 `None`：
    /// 信息不足，不靠人为加的正则项凑出答案。
    pub fn fit(&self) -> Option<Fit> {
        if self.n as usize <= self.k {
            return None;
        }
        let inv = invert_spd(&self.xtx, self.k)?;
        let beta: Vec<f64> = (0..self.k)
            .map(|i| (0..self.k).map(|j| inv[i * self.k + j] * self.xty[j]).sum())
            .collect();
        let explained: f64 = beta.iter().zip(&self.xty).map(|(b, v)| b * v).sum();
        let dof = (self.n as usize - self.k) as f64;
        let s2 = ((self.yty - explained) / dof).max(0.0);
        Some(Fit { beta, inv, s2, dof })
    }
}

impl Fit {
    pub fn predict(&self, x: &[f64]) -> f64 {
        self.beta.iter().zip(x).map(|(b, v)| b * v).sum()
    }

    /// `xᵀ(XᵀX)⁻¹x`。
    fn leverage(&self, x: &[f64]) -> f64 {
        let k = x.len();
        (0..k)
            .map(|i| x[i] * (0..k).map(|j| self.inv[i * k + j] * x[j]).sum::<f64>())
            .sum()
    }

    /// 单个新观测的预测区间上界：`ŷ + t(1−δ; n−k) · s · √(1 + xᵀ(XᵀX)⁻¹x)`。
    pub fn upper_prediction(&self, x: &[f64], delta: Delta) -> f64 {
        let t = t_quantile(1.0 - delta.get(), self.dof);
        self.predict(x) + t * (self.s2 * (1.0 + self.leverage(x))).sqrt()
    }

    /// 系数 `i`、`j` 的协方差 `s²·(XᵀX)⁻¹ᵢⱼ`。
    pub fn cov(&self, i: usize, j: usize) -> f64 {
        self.s2 * self.inv[i * self.beta.len() + j]
    }
}

/// 对称正定矩阵求逆：Cholesky 分解 `A = LLᵀ`，再解 `LLᵀ X = I`。
/// 主元不为正（奇异或数值上不正定）时返回 `None`。
fn invert_spd(a: &[f64], k: usize) -> Option<Vec<f64>> {
    let mut l = vec![0.0; k * k];
    let scale = (0..k).map(|i| a[i * k + i].abs()).fold(0.0, f64::max);
    for i in 0..k {
        for j in 0..=i {
            let mut s = a[i * k + j];
            for p in 0..j {
                s -= l[i * k + p] * l[j * k + p];
            }
            if i == j {
                // 主元相对最大对角元小到机器精度量级就当作奇异
                if s <= scale * f64::EPSILON * k as f64 {
                    return None;
                }
                l[i * k + i] = s.sqrt();
            } else {
                l[i * k + j] = s / l[j * k + j];
            }
        }
    }
    let mut inv = vec![0.0; k * k];
    for col in 0..k {
        // 前代 L z = e_col，回代 Lᵀ x = z
        let mut z = vec![0.0; k];
        for i in 0..k {
            let mut s = if i == col { 1.0 } else { 0.0 };
            for p in 0..i {
                s -= l[i * k + p] * z[p];
            }
            z[i] = s / l[i * k + i];
        }
        for i in (0..k).rev() {
            let mut s = z[i];
            for p in i + 1..k {
                s -= l[p * k + i] * inv[p * k + col];
            }
            inv[i * k + col] = s / l[i * k + i];
        }
    }
    Some(inv)
}

impl Stat for RegressionSums {
    fn merge(&mut self, o: &Self) {
        if o.n == 0 {
            return;
        }
        if self.n == 0 && self.k != o.k {
            *self = o.clone();
            return;
        }
        assert_eq!(self.k, o.k, "merging regressions of different dimension");
        self.xtx.iter_mut().zip(&o.xtx).for_each(|(a, b)| *a += b);
        self.xty.iter_mut().zip(&o.xty).for_each(|(a, b)| *a += b);
        self.yty += o.yty;
        self.n += o.n;
    }

    fn encode(&self, w: &mut Writer) {
        w.u64(self.k as u64);
        w.u64(self.n);
        w.f64s(&self.xtx);
        w.f64s(&self.xty);
        w.f64(self.yty);
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let k = r.u64()? as usize;
        let s = Self {
            k,
            n: r.u64()?,
            xtx: r.f64s()?,
            xty: r.f64s()?,
            yty: r.f64()?,
        };
        if s.xtx.len() != k * k || s.xty.len() != k {
            return Err(DecodeError::Invalid("regression dims"));
        }
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    #[test]
    fn regression_and_prediction_interval() {
        let mut rng = Rng::seed(11);
        let mut r = RegressionSums::new(2);
        for i in 0..2000 {
            let x = i as f64 / 100.0;
            r.push(&[1.0, x], 3.0 + 2.0 * x + rng.normal());
        }
        let f = r.fit().unwrap();
        assert!(
            (f.beta[0] - 3.0).abs() < 0.15 && (f.beta[1] - 2.0).abs() < 0.02,
            "{:?}",
            f.beta
        );
        assert!((f.s2 - 1.0).abs() < 0.1);
        // 单侧 95% 上界约为 ŷ + 1.645σ
        let up = f.upper_prediction(&[1.0, 10.0], Delta::DEFAULT);
        assert!((up - f.predict(&[1.0, 10.0]) - 1.645).abs() < 0.1, "{up}");
        assert!(RegressionSums::new(2).fit().is_none());
    }
}
