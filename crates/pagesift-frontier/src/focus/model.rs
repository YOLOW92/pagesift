//! 链接打分的线性权重：在线贝叶斯逻辑回归（CONSTANTS"focus 打分"一行，DESIGN §5.4-D）。
//!
//! 目标：链接指向的页是不是相关页（相关度不低于 [`super::MIN_RELEVANCE`]）。特征是链接分数的
//! 五个线性项：锚文本、URL、所在行按当前词权重的覆盖率，所在页面的相关度，锚文本命中两个以上
//! 查询词的指示。模型 `P(相关) = σ(b + w·x)`；排序只用 `w·x`（截距 `b` 不影响先后）。
//!
//! 估计：对角高斯后验的在线拉普拉斯近似（O. Chapelle, L. Li, "An Empirical Evaluation of Thompson
//! Sampling", NIPS 2011, Algorithm 3 的逐样本形式）：每来一个样本，`qᵢ ← qᵢ + p(1−p)xᵢ²`，
//! `mᵢ ← mᵢ + (y − p)xᵢ / qᵢ`，`p = σ(m·x)`。偏离原文：原文按批次做完整的牛顿迭代，这里每个样本
//! 只走一步（在线、O(特征数)），批次之间不重算；样本少时两者接近，因为先验项占主导。
//!
//! - 先验均值（先验包 v0：0.81 的现值）是 0.81 的固定权重，截距 0；先验精度是一份观测在
//!   `p = ½`、`x = 1` 处的费希尔信息 `¼`（DESIGN §5.8 规则 1：最弱的一份观测）。
//! - 只在显著时更新（§5.7）：后验均值和打分正在用的权重之差超过 `z(1−δ)·√(1/qᵢ)` 才换上新值；
//!   换上的就是精度加权后的后验均值。没有数据时打分和 0.81 完全一样。
//! - 权重不低于 0：覆盖了查询词反而扣分不是这个模型该学出来的东西（多半是样本里的偶然），
//!   夹到 0 等于这一项暂时不起作用。

/// 特征数：锚文本、URL、所在行、所在页面、词组指示。
/// [推导] 等于 `Focus::features` 返回的项数（链接分数的五个线性项）
pub const FEATURES: usize = 5;

/// 截距的位置（在特征之后）。
const BIAS: usize = FEATURES;

/// [推导] 一份观测在 p = ½、x = 1 处的费希尔信息 p(1−p) = ¼，作为先验精度（DESIGN §5.8）
const PRIOR_PRECISION: f64 = 0.25;

pub struct LinkModel {
    mean: [f64; FEATURES + 1],
    precision: [f64; FEATURES + 1],
    /// 打分正在用的权重。
    used: [f32; FEATURES],
    z: f64,
}

impl LinkModel {
    pub fn new(prior: [f32; FEATURES]) -> Self {
        let mut mean = [0.0; FEATURES + 1];
        for (m, p) in mean.iter_mut().zip(prior) {
            *m = f64::from(p);
        }
        Self {
            mean,
            precision: [PRIOR_PRECISION; FEATURES + 1],
            used: prior,
            z: pagesift_stat::Delta::DEFAULT.z(),
        }
    }

    pub fn weights(&self) -> &[f32; FEATURES] {
        &self.used
    }

    /// 记下一个样本：链接的特征 `x`，它指向的页是否相关。打分用的权重变了时返回 true
    /// （调用方要把排队的链接重新打分）。
    pub fn observe(&mut self, x: &[f32; FEATURES], hit: bool) -> bool {
        let mut xs = [1.0; FEATURES + 1];
        for (a, b) in xs.iter_mut().zip(x) {
            *a = f64::from(*b);
        }
        let logit: f64 = self.mean.iter().zip(&xs).map(|(m, x)| m * x).sum();
        let p = 1.0 / (1.0 + (-logit).exp());
        let y = if hit { 1.0 } else { 0.0 };
        for (i, &x) in xs.iter().enumerate().take(BIAS + 1) {
            if x == 0.0 {
                continue;
            }
            self.precision[i] += p * (1.0 - p) * x * x;
            self.mean[i] += (y - p) * x / self.precision[i];
        }
        let mut changed = false;
        for i in 0..FEATURES {
            let se = (1.0 / self.precision[i]).sqrt();
            let m = self.mean[i].max(0.0);
            if (m - f64::from(self.used[i])).abs() > self.z * se {
                self.used[i] = m as f32;
                changed = true;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 冷启动用先验（0.81 的权重）；几个样本不显著，权重不动；
    /// 大量样本显示锚文本命中的链接几乎都无关、URL 命中的都相关时，锚文本的权重降下来、URL 的升上去。
    #[test]
    fn starts_at_the_prior_and_moves_only_on_strong_evidence() {
        let prior = [2.0, 1.5, 1.0, 0.5, 0.5];
        let mut m = LinkModel::new(prior);
        assert_eq!(m.weights(), &prior);
        for _ in 0..5 {
            assert!(!m.observe(&[1.0, 0.0, 0.0, 0.0, 0.0], false));
        }
        assert_eq!(m.weights(), &prior);
        for _ in 0..400 {
            m.observe(&[1.0, 0.0, 0.0, 0.0, 0.0], false);
            m.observe(&[0.0, 1.0, 0.0, 0.0, 0.0], true);
        }
        let w = m.weights();
        assert!(w[0] < prior[0] && w[1] > prior[1], "{w:?}");
        assert_eq!(w[2], prior[2], "features never seen keep the prior");
    }
}
