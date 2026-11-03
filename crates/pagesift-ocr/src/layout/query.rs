//! 两阶段 DETR 的查询选取：编码器输出的每个位置过一层线性 + 层归一化、打分，取分最高的若干个作
//! 解码器的初始查询；初始参考框取各查询掩码（查询向量 · 掩码特征 > 0）的外接框（`mask_enhanced`）。

use pagesift_infer::{LayerNorm, Linear, Tensor, Weights, ops};

use super::deform::Level;
use super::layers::Mlp;

/// [协议] 一个框的四个分量：(cx, cy, w, h) 或 (x0, y0, x1, y1)
pub(crate) type Bbox = [f32; 4];

/// σ(x)。
#[inline]
pub(crate) fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// [协议] 源码 `inverse_sigmoid` 的 eps
const INV_SIGMOID_EPS: f32 = 1e-5;

/// σ 的反函数（`inverse_sigmoid`）：先截到 0–1，分子分母各不低于 eps。
#[inline]
pub(crate) fn inverse_sigmoid(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    (x.max(INV_SIGMOID_EPS) / (1.0 - x).max(INV_SIGMOID_EPS)).ln()
}

/// 分数最高的 `k` 个下标，从高到低；同分时下标小的在前。
pub(crate) fn topk(scores: &[f32], k: usize) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..scores.len()).collect();
    let k = k.min(idx.len());
    let by_score = |a: &usize, b: &usize| scores[*b].total_cmp(&scores[*a]).then(a.cmp(b));
    if k < idx.len() {
        idx.select_nth_unstable_by(k, by_score);
        idx.truncate(k);
    }
    idx.sort_unstable_by(by_score);
    idx
}

/// 一行掩码 logit（`h × w`，按行）里大于 0 的外接框，归一化成 (cx, cy, w, h)；全空时全 0
/// （`mask_to_box_coordinate`）。
pub(crate) fn mask_box(row: &[f32], h: usize, w: usize) -> Bbox {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for (y, r) in row.chunks_exact(w).enumerate() {
        let mut any = false;
        for (x, &v) in r.iter().enumerate() {
            if v > 0.0 {
                x0 = x0.min(x);
                x1 = x1.max(x + 1);
                any = true;
            }
        }
        if any {
            y0 = y0.min(y);
            y1 = y + 1;
        }
    }
    if x0 == usize::MAX {
        return Bbox::default();
    }
    let (fw, fh) = (w as f32, h as f32);
    let (xa, xb) = (x0 as f32 / fw, x1 as f32 / fw);
    let (ya, yb) = (y0 as f32 / fh, y1 as f32 / fh);
    [(xa + xb) / 2.0, (ya + yb) / 2.0, xb - xa, yb - ya]
}

/// [协议] 源码里的 eps（在 f32 上比较，边界上的点算无效）
const ANCHOR_EPS: f64 = 1e-2;

/// 每个位置的锚点是否有效（`_cached_generate_anchors` 的 `valid_mask`）：中心和宽高都在 (eps, 1 − eps)。
/// 宽高 0.05·2^层号，对这个模型总在界内，所以只看中心。
pub(crate) fn anchor_valid(levels: &[Level]) -> Vec<bool> {
    let eps = ANCHOR_EPS as f32;
    let hi = (1.0 - ANCHOR_EPS) as f32;
    let ok = |v: f32| v > eps && v < hi;
    let mut out = Vec::new();
    for lv in levels {
        for y in 0..lv.h {
            let cy = (y as f32 + 0.5) / lv.h as f32;
            for x in 0..lv.w {
                let cx = (x as f32 + 0.5) / lv.w as f32;
                out.push(ok(cx) && ok(cy));
            }
        }
    }
    out
}

/// 查询选取的几层。
#[derive(Debug, Clone)]
pub(crate) struct QuerySelect {
    linear: Linear,
    norm: LayerNorm,
    score: Linear,
    decoder_norm: LayerNorm,
    mask_query: Mlp,
    num_queries: usize,
}

impl QuerySelect {
    pub(crate) fn load(w: &Weights, eps: f32, num_queries: usize) -> Result<QuerySelect, String> {
        Ok(QuerySelect {
            linear: Linear::load(w, "model.enc_output.0")?,
            norm: LayerNorm::load(w, "model.enc_output.1", eps)?,
            score: Linear::load(w, "model.enc_score_head")?,
            decoder_norm: LayerNorm::load(w, "model.decoder_norm", eps)?,
            mask_query: Mlp::load(w, "model.mask_query_head")?,
            num_queries,
        })
    }

    /// `src` 是展平的编码器特征 `[S, C]`。返回初始查询 `[Q, C]` 和初始参考框（已过 σ）。
    pub(crate) fn forward(
        &self,
        src: &Tensor,
        levels: &[Level],
        mask_feat: &Tensor,
    ) -> (Tensor, Vec<Bbox>) {
        let (s, c) = src.tc();
        // 锚点无效的位置清零再过线性层（源码 `memory = valid_mask * source_flatten`）
        let mut memory = src.clone();
        for (row, ok) in memory.data.chunks_exact_mut(c).zip(anchor_valid(levels)) {
            if !ok {
                row.fill(0.0);
            }
        }
        debug_assert_eq!(s, memory.data.len() / c);
        let om = self.norm.forward(&self.linear.forward(&memory));
        let scores = self.score.forward(&om);
        let classes = scores.shape[1];
        let best: Vec<f32> = scores
            .data
            .chunks_exact(classes)
            .map(|r| r.iter().copied().fold(f32::NEG_INFINITY, f32::max))
            .collect();
        let picked = topk(&best, self.num_queries);
        let q = picked.len();
        let mut target = Vec::with_capacity(q * c);
        for &i in &picked {
            target.extend_from_slice(&om.data[i * c..(i + 1) * c]);
        }
        let target = Tensor::new(vec![q, c], target);

        // 掩码 = 查询向量 · 掩码特征，外接框作初始参考框
        let mq = self.mask_query.forward(&self.decoder_norm.forward(&target));
        let (pc, mh, mw) = mask_feat.chw();
        let mf = Tensor::new(vec![pc, mh * mw], mask_feat.data.clone());
        let masks = ops::matmul(&mq, &mf);
        let refs = masks
            .data
            .chunks_exact(mh * mw)
            .map(|row| mask_box(row, mh, mw).map(|v| sigmoid(inverse_sigmoid(v))))
            .collect();
        (target, refs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 反函数互逆且截断() {
        for v in [0.1f32, 0.5, 0.9] {
            assert!((sigmoid(inverse_sigmoid(v)) - v).abs() < 1e-5);
        }
        // 全空掩码的框是 0：反 σ = ln(1e-5)
        assert!((inverse_sigmoid(0.0) - 1e-5f32.ln()).abs() < 1e-4);
        assert!((inverse_sigmoid(1.0) + 1e-5f32.ln()).abs() < 1e-4);
    }

    #[test]
    fn topk_从高到低同分取小下标() {
        assert_eq!(topk(&[0.1, 0.9, 0.5, 0.9, 0.2], 3), [1, 3, 2]);
        assert_eq!(topk(&[0.1, 0.2], 5), [1, 0]);
    }

    #[test]
    fn 掩码外接框() {
        // 4×5，只有 (x=1..=3, y=2) 为正
        let mut m = vec![-1f32; 20];
        for x in 1..=3 {
            m[2 * 5 + x] = 1.0;
        }
        let b = mask_box(&m, 4, 5);
        // x: 0.2–0.8，y: 0.5–0.75
        for (a, e) in b.iter().zip([0.5, 0.625, 0.6, 0.25]) {
            assert!((a - e).abs() < 1e-6, "{b:?}");
        }
        assert_eq!(mask_box(&[-1.0; 20], 4, 5), [0.0; 4]);
    }

    #[test]
    fn 锚点有效范围() {
        let lv = [
            Level {
                h: 100,
                w: 100,
                start: 0,
            },
            Level {
                h: 50,
                w: 50,
                start: 10000,
            },
        ];
        let v = anchor_valid(&lv);
        assert_eq!(v.len(), 12500);
        // 100 格：第 0 格中心 0.005 无效，第 1 格 0.015 有效
        assert!(!v[0]);
        assert!(v[100 + 1]);
        assert!(!v[99 * 100 + 50]);
        // 50 格：第 0 格中心正好 0.01，无效
        assert!(!v[10000 + 25]);
        assert!(v[10000 + 50 + 1]);
    }
}
