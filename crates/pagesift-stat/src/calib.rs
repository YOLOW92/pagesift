//! 概率校准模型：逻辑回归（Firth 修正）与保序回归。

use crate::{DecodeError, Reader, Writer};

pub(crate) fn sigmoid(z: f64) -> f64 {
    if !z.is_finite() {
        return if z.is_nan() {
            f64::NAN
        } else if z > 0.0 {
            1.0
        } else {
            0.0
        };
    }
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
    /// 第一步就解不出（特征共线、某维恒定、输入含非有限值或维度不一致）时返回 `None`。
    pub fn fit(xs: &[Vec<f64>], ys: &[bool]) -> Option<Self> {
        if xs.is_empty() || xs.len() != ys.len() {
            return None;
        }
        let k = xs.first()?.len();
        if k == 0
            || xs
                .iter()
                .any(|x| x.len() != k || x.iter().any(|v| !v.is_finite()))
        {
            return None;
        }
        let mut w = vec![0.0; k];
        let mut scaled_x = vec![0.0; k];
        let mut stats = Vec::with_capacity(xs.len());
        let tol = f64::EPSILON.sqrt();
        // [策略] IRLS 牛顿迭代上限设为 max(min(N, 64), k)，正常 5~12 步内达到机器精度收敛
        let max_iters = xs.len().min(64).max(k);
        for iter in 0..max_iters {
            // 第一遍：当前权重下的 (XᵀVX)⁻¹
            let mut info = crate::regress::RegressionSums::new(k);
            stats.clear();
            for x in xs {
                let eta: f64 = w.iter().zip(x).map(|(a, b)| a * b).sum();
                if !eta.is_finite() {
                    return if iter == 0 { None } else { Some(Self { w }) };
                }
                let p = sigmoid(eta);
                let v = (p * (1.0 - p)).max(f64::EPSILON);
                let s = v.sqrt();
                for (dst, &xi) in scaled_x.iter_mut().zip(x) {
                    *dst = xi * s;
                }
                info.push(&scaled_x, 0.0);
                stats.push((eta, p, v));
            }
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
                if !z.is_finite() {
                    return if iter == 0 { None } else { Some(Self { w }) };
                }
                let s = v.sqrt();
                for (dst, &xi) in scaled_x.iter_mut().zip(x) {
                    *dst = xi * s;
                }
                sums.push(&scaled_x, z * s);
            }
            let Some(next) = sums.fit().map(|f| f.beta) else {
                return if iter == 0 { None } else { Some(Self { w }) };
            };
            if next.iter().any(|v| !v.is_finite()) {
                return if iter == 0 { None } else { Some(Self { w }) };
            }
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
        if self.w.is_empty()
            || x.len() != self.w.len()
            || x.iter().any(|v| !v.is_finite())
            || self.w.iter().any(|v| !v.is_finite())
        {
            return f64::NAN;
        }
        sigmoid(self.w.iter().zip(x).map(|(a, b)| a * b).sum())
    }

    pub fn encode(&self, w: &mut Writer) {
        w.f64s(&self.w);
    }

    pub fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let w = r.f64s()?;
        if w.iter().any(|v| !v.is_finite()) {
            return Err(DecodeError::Invalid("logistic weights"));
        }
        Ok(Self { w })
    }
}

/// 保序回归校准（B. Zadrozny, C. Elkan, KDD 2002）：在分数上拟合单调不减的阶梯函数，
/// 用相邻违例合并算法（PAV；Barlow 等, Statistical Inference under Order Restrictions, 1972）。
/// 不假设校准曲线的形状，能画出台阶（例如失败集中在最低一段分数、其余几乎都成功）；
/// Platt 这类光滑曲线画不出台阶。代价是样本少的一端方差大，要配合交叉拟合用足校准数据。
#[derive(Debug, Clone, PartialEq)]
pub struct Isotonic {
    /// 各台阶的（分数上界，校准后的概率），分数严格升序且均为有限值。
    steps: Vec<(f64, f64)>,
}

impl Isotonic {
    pub fn fit(scores: &[f64], ys: &[bool]) -> Self {
        let n_in = scores.len().min(ys.len());
        let mut idx: Vec<usize> = (0..n_in).filter(|&i| scores[i].is_finite()).collect();
        idx.sort_by(|&a, &b| scores[a].total_cmp(&scores[b]));
        // 块：（分数上界，正例和，权重）
        let mut blocks: Vec<(f64, f64, f64)> = Vec::with_capacity(idx.len());
        for i in idx {
            blocks.push((scores[i], f64::from(u8::from(ys[i])), 1.0));
            while blocks.len() >= 2 {
                let n = blocks.len();
                let (a, b) = (blocks[n - 2], blocks[n - 1]);
                if a.0 < b.0 && a.1 / a.2 <= b.1 / b.2 {
                    break;
                }
                blocks.truncate(n - 2);
                blocks.push((b.0, a.1 + b.1, a.2 + b.2));
            }
        }
        Self {
            steps: blocks
                .into_iter()
                .map(|(s, y, w)| (s, (y / w).clamp(0.0, 1.0)))
                .collect(),
        }
    }

    /// 分数 `s` 的校准概率：第一个上界 ≥ `s` 的台阶；超出最大分数时取最后一级。
    /// 当 `s` 为 `NaN` 或模型尚未拟合任何有效样本时返回 `NaN`。
    pub fn predict(&self, s: f64) -> f64 {
        if s.is_nan() || self.steps.is_empty() {
            return f64::NAN;
        }
        let pos = self.steps.partition_point(|&(hi, _)| hi < s);
        if pos < self.steps.len() {
            self.steps[pos].1
        } else {
            self.steps[self.steps.len() - 1].1
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
        if hi.len() != p.len()
            || hi.iter().any(|x| !x.is_finite())
            || p.iter().any(|x| !x.is_finite() || !(0.0..=1.0).contains(x))
            || hi.windows(2).any(|w| w[0] >= w[1])
            || p.windows(2).any(|w| w[0] > w[1])
        {
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

    #[test]
    fn isotonic_and_logistic_guard_nan_inf_empty_single_class_and_mismatched_lengths() {
        // Empty input
        let empty = Isotonic::fit(&[], &[]);
        assert!(empty.predict(0.5).is_nan());

        // Single-class samples (all true / all false)
        let all_true = Isotonic::fit(&[0.1, 0.5, 0.9], &[true, true, true]);
        assert_eq!(all_true.predict(0.0), 1.0);
        assert_eq!(all_true.predict(0.5), 1.0);
        assert_eq!(all_true.predict(2.0), 1.0);

        let all_false = Isotonic::fit(&[0.2, 0.8], &[false, false]);
        assert_eq!(all_false.predict(0.5), 0.0);

        // NaN / Inf in training scores & mismatched slice lengths must not panic or poison steps
        let dirty = Isotonic::fit(
            &[f64::NAN, 0.2, f64::INFINITY, 0.8, f64::NEG_INFINITY, 0.99],
            &[true, false, false, true, true],
        );
        assert!((dirty.predict(0.2) - 0.0).abs() < 1e-12);
        assert!((dirty.predict(0.8) - 1.0).abs() < 1e-12);
        // Predicting on NaN must return NaN rather than silently returning the top step!
        assert!(dirty.predict(f64::NAN).is_nan());
        assert!((dirty.predict(f64::INFINITY) - 1.0).abs() < 1e-12);
        assert!((dirty.predict(f64::NEG_INFINITY) - 0.0).abs() < 1e-12);

        // Logistic guards ragged / NaN / mismatched inputs without panicking
        assert!(Logistic::fit(&[], &[]).is_none());
        assert!(Logistic::fit(&[vec![]], &[true]).is_none());
        assert!(Logistic::fit(&[vec![1.0, 0.5], vec![1.0]], &[true, false]).is_none());
        assert!(Logistic::fit(&[vec![1.0, f64::NAN], vec![1.0, 2.0]], &[true, false]).is_none());
        assert!(Logistic::fit(&[vec![1.0, 0.5]], &[true, false]).is_none());
    }
}
