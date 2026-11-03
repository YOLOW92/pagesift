//! 概率校准模型：逻辑回归（Firth 修正）与保序回归。

use crate::{DecodeError, Reader, Writer};

pub(crate) fn sigmoid(z: f64) -> f64 {
    if z >= 0.0 {
        1.0 / (1.0 + (-z).exp())
    } else {
        let e = z.exp();
        e / (1.0 + e)
    }
}

/// 逻辑回归 `P(y=1|x) = σ(wᵀx)`，用 Firth 偏差修正的似然拟合（D. Firth, "Bias reduction of maximum
/// likelihood estimates", Biometrika 80(1), 1993）：在对数似然上加 `½ ln det I(β)`（Jeffreys 先验），
/// 得分方程变为 `Σ (yᵢ − pᵢ + hᵢ(½ − pᵢ)) xᵢ = 0`，`hᵢ` 是加权帽子矩阵 `V½X(XᵀVX)⁻¹XᵀV½` 的对角元。
/// 普通极大似然在特征近乎把正负例完全分开时权重发散、预测过于自信；Firth 估计总是有限的
/// （Heinze & Schemper, Statistics in Medicine 21, 2002），且不引入任何调节参数。
/// `x` 的第一维应为常数 1（截距）。
#[derive(Debug, Clone, PartialEq)]
pub struct Logistic {
    pub w: Vec<f64>,
}

impl Logistic {
    /// 修正后的 IRLS：工作响应 `z = η + (y − p + h(½ − p))/v`，权重 `v = p(1−p)`（McCullagh & Nelder,
    /// Generalized Linear Models, 1989, §4.4 的迭代加权最小二乘，得分换成 Firth 的修正得分）。
    /// 步长的相对大小小于 `√ε` 时停止：牛顿法二次收敛，下一步的误差已在机器精度以内。
    /// 第一步就解不出（特征共线、某维恒定）时返回 `None`。
    pub fn fit(xs: &[Vec<f64>], ys: &[bool]) -> Option<Self> {
        let k = xs.first()?.len();
        let mut w = vec![0.0; k];
        let tol = f64::EPSILON.sqrt();
        for iter in 0..xs.len().max(k) {
            // 第一遍：当前权重下的 (XᵀVX)⁻¹
            let mut info = crate::regress::RegressionSums::new(k);
            let stats: Vec<(f64, f64, f64)> = xs
                .iter()
                .map(|x| {
                    let eta: f64 = w.iter().zip(x).map(|(a, b)| a * b).sum();
                    let p = sigmoid(eta);
                    let v = (p * (1.0 - p)).max(f64::EPSILON);
                    let s = v.sqrt();
                    info.push(&x.iter().map(|a| a * s).collect::<Vec<_>>(), 0.0);
                    (eta, p, v)
                })
                .collect();
            let Some(inv) = info.fit().map(|f| f.inv) else {
                return if iter == 0 { None } else { Some(Self { w }) };
            };
            // 第二遍：帽子矩阵对角元 hᵢ = vᵢ xᵢᵀ(XᵀVX)⁻¹xᵢ，修正工作响应后做加权最小二乘
            let mut sums = crate::regress::RegressionSums::new(k);
            for ((x, &y), &(eta, p, v)) in xs.iter().zip(ys).zip(&stats) {
                let lev: f64 = (0..k)
                    .map(|i| x[i] * (0..k).map(|j| inv[i * k + j] * x[j]).sum::<f64>())
                    .sum();
                let h = v * lev;
                let z = eta + (f64::from(u8::from(y)) - p + h * (0.5 - p)) / v;
                let s = v.sqrt();
                sums.push(&x.iter().map(|a| a * s).collect::<Vec<_>>(), z * s);
            }
            let Some(next) = sums.fit().map(|f| f.beta) else {
                return if iter == 0 { None } else { Some(Self { w }) };
            };
            let step = next
                .iter()
                .zip(&w)
                .map(|(a, b)| (a - b).abs() / b.abs().max(1.0))
                .fold(0.0, f64::max);
            w = next;
            if step < tol {
                break;
            }
        }
        Some(Self { w })
    }

    pub fn predict(&self, x: &[f64]) -> f64 {
        sigmoid(self.w.iter().zip(x).map(|(a, b)| a * b).sum())
    }

    pub fn encode(&self, w: &mut Writer) {
        w.f64s(&self.w);
    }

    pub fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self { w: r.f64s()? })
    }
}

/// 保序回归校准（B. Zadrozny, C. Elkan, KDD 2002）：在分数上拟合单调不减的阶梯函数，
/// 用相邻违例合并算法（PAV；Barlow 等, Statistical Inference under Order Restrictions, 1972）。
/// 不假设校准曲线的形状，能画出台阶（例如失败集中在最低一段分数、其余几乎都成功）；
/// Platt 这类光滑曲线画不出台阶。代价是样本少的一端方差大，要配合交叉拟合用足校准数据。
#[derive(Debug, Clone, PartialEq)]
pub struct Isotonic {
    /// 各台阶的（分数上界，校准后的概率），分数升序。
    steps: Vec<(f64, f64)>,
}

impl Isotonic {
    pub fn fit(scores: &[f64], ys: &[bool]) -> Self {
        let mut idx: Vec<usize> = (0..scores.len()).collect();
        idx.sort_by(|&a, &b| scores[a].total_cmp(&scores[b]));
        // 块：（分数上界，正例和，权重）
        let mut blocks: Vec<(f64, f64, f64)> = Vec::new();
        for i in idx {
            blocks.push((scores[i], f64::from(u8::from(ys[i])), 1.0));
            while blocks.len() >= 2 {
                let n = blocks.len();
                let (a, b) = (blocks[n - 2], blocks[n - 1]);
                if a.1 / a.2 <= b.1 / b.2 {
                    break;
                }
                blocks.truncate(n - 2);
                blocks.push((b.0, a.1 + b.1, a.2 + b.2));
            }
        }
        Self {
            steps: blocks.into_iter().map(|(s, y, w)| (s, y / w)).collect(),
        }
    }

    /// 分数 `s` 的校准概率：第一个上界 ≥ `s` 的台阶；超出最大分数时取最后一级。
    pub fn predict(&self, s: f64) -> f64 {
        match self.steps.iter().find(|(hi, _)| s <= *hi) {
            Some(&(_, p)) => p,
            None => self.steps.last().map_or(f64::NAN, |&(_, p)| p),
        }
    }

    pub fn encode(&self, w: &mut Writer) {
        let (hi, p): (Vec<f64>, Vec<f64>) = self.steps.iter().copied().unzip();
        w.f64s(&hi);
        w.f64s(&p);
    }

    pub fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let hi = r.f64s()?;
        let p = r.f64s()?;
        if hi.len() != p.len() || p.windows(2).any(|w| w[0] > w[1]) {
            return Err(DecodeError::Invalid("isotonic steps"));
        }
        Ok(Self {
            steps: hi.into_iter().zip(p).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    #[test]
    fn logistic_recovers_coefficients_and_isotonic_is_monotone() {
        let mut rng = Rng::seed(17);
        // 真实概率 σ(2x − 1)：逻辑回归恢复系数，保序回归单调
        let xs: Vec<Vec<f64>> = (0..4000)
            .map(|_| vec![1.0, rng.uniform() * 4.0 - 2.0])
            .collect();
        let ys: Vec<bool> = xs
            .iter()
            .map(|x| rng.uniform() < sigmoid(2.0 * x[1] - 1.0))
            .collect();
        let lr = Logistic::fit(&xs, &ys).unwrap();
        assert!(
            (lr.w[0] + 1.0).abs() < 0.2 && (lr.w[1] - 2.0).abs() < 0.2,
            "{:?}",
            lr.w
        );
        let probs: Vec<f64> = xs.iter().map(|x| lr.predict(x)).collect();
        let iso = Isotonic::fit(&probs, &ys);
        assert!(iso.steps.windows(2).all(|w| w[0].1 <= w[1].1));
    }
}
