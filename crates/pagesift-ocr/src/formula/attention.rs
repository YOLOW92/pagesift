//! 多头注意力，对应参考实现的 `PPFormulaNetAttention`：q、k、v、out 四个线性层，
//! 按头切开后做缩放点积。K、V 由调用方算好并保存（自注意力逐步追加、交叉注意力只算一次），
//! 这里只负责用查询去注意。

use pagesift_infer::{Linear, Tensor, Weights};

pub(super) struct Attention {
    q: Linear,
    pub(super) k: Linear,
    pub(super) v: Linear,
    out: Linear,
    heads: usize,
}

impl Attention {
    /// 读 `{p}.q_proj`、`{p}.k_proj`、`{p}.v_proj`、`{p}.out_proj`。
    pub(super) fn load(w: &Weights, p: &str, heads: usize) -> Result<Attention, String> {
        Ok(Attention {
            q: Linear::load(w, &format!("{p}.q_proj"))?,
            k: Linear::load(w, &format!("{p}.k_proj"))?,
            v: Linear::load(w, &format!("{p}.v_proj"))?,
            out: Linear::load(w, &format!("{p}.out_proj"))?,
            heads,
        })
    }

    /// `x` 是 `[T, D]` 的查询输入；`keys`、`values` 是投影好的 `[S, D]`（按行连续存放）；
    /// 第 `i` 行查询只看前 `visible(i)` 行（因果掩码）。结果 `[T, D]`。
    pub(super) fn attend(
        &self,
        x: &Tensor,
        keys: &[f32],
        values: &[f32],
        visible: impl Fn(usize) -> usize,
    ) -> Tensor {
        let q = self.q.forward(x);
        let (t, d) = q.tc();
        let hd = d / self.heads;
        let mut out = vec![0f32; t * d];
        attend_rows(&q.data, d, self.heads, keys, values, visible, &mut out);
        // 只对输出投影，缩放已在 attend_rows 里：scaling = head_dim^−0.5
        debug_assert_eq!(hd * self.heads, d);
        self.out.forward(&Tensor::new(vec![t, d], out))
    }
}

/// 逐行、逐头的缩放点积注意力：
/// 分数 = (q·k)·head_dim^−0.5，softmax 后对 v 加权求和（`eager_attention_forward`）。
fn attend_rows(
    q: &[f32],
    d: usize,
    heads: usize,
    keys: &[f32],
    values: &[f32],
    visible: impl Fn(usize) -> usize,
    out: &mut [f32],
) {
    let hd = d / heads;
    // [协议] `PPFormulaNetAttention.scaling = head_dim ** -0.5`
    let scale = (hd as f32).powf(-0.5);
    let mut scores = Vec::new();
    for (i, (qr, or)) in q.chunks_exact(d).zip(out.chunks_exact_mut(d)).enumerate() {
        let n = visible(i);
        for h in 0..heads {
            let at = h * hd;
            let qh = &qr[at..at + hd];
            scores.clear();
            scores.extend(keys[..n * d].chunks_exact(d).map(|k| {
                let dot: f32 = qh.iter().zip(&k[at..at + hd]).map(|(a, b)| a * b).sum();
                dot * scale
            }));
            // softmax：减去最大值再取指数，与 `ops::softmax_rows` 相同
            let m = scores.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let mut sum = 0f32;
            for s in &mut scores {
                *s = (*s - m).exp();
                sum += *s;
            }
            let oh = &mut or[at..at + hd];
            for (p, v) in scores.iter().zip(values[..n * d].chunks_exact(d)) {
                let p = p / sum;
                for (o, v) in oh.iter_mut().zip(&v[at..at + hd]) {
                    *o += p * v;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手算：单头、head_dim = 2，scale = 1/√2，分数 [1/√2, 0]。
    #[test]
    fn one_head_by_hand() {
        let mut out = vec![0f32; 2];
        attend_rows(
            &[1.0, 0.0],
            2,
            1,
            &[1.0, 0.0, 0.0, 1.0],
            &[1.0, 2.0, 3.0, 4.0],
            |_| 2,
            &mut out,
        );
        let e = (std::f32::consts::FRAC_1_SQRT_2).exp();
        let (p0, p1) = (e / (e + 1.0), 1.0 / (e + 1.0));
        assert!((out[0] - (p0 + 3.0 * p1)).abs() < 1e-6);
        assert!((out[1] - (2.0 * p0 + 4.0 * p1)).abs() < 1e-6);
    }

    /// 两个头各看自己那一段：头 0 选第 0 行、头 1 选第 1 行时，输出的两段分别取自这两行。
    #[test]
    fn heads_split_columns() {
        let big = 100.0;
        let mut out = vec![0f32; 2];
        attend_rows(
            &[big, big],
            2,
            2,
            &[1.0, 0.0, 0.0, 1.0],
            &[5.0, 6.0, 7.0, 8.0],
            |_| 2,
            &mut out,
        );
        assert!((out[0] - 5.0).abs() < 1e-4);
        assert!((out[1] - 8.0).abs() < 1e-4);
    }

    /// 掩码：只看第 0 行时结果就是第 0 行的值。
    #[test]
    fn mask_limits_keys() {
        let mut out = vec![0f32; 4];
        attend_rows(
            &[0.0, 1.0, 0.0, 1.0],
            2,
            1,
            &[1.0, 0.0, 0.0, 1.0],
            &[1.0, 2.0, 3.0, 4.0],
            |i| i + 1,
            &mut out,
        );
        assert_eq!(&out[..2], &[1.0, 2.0]);
        assert!(out[2] > 1.0 && out[2] < 3.0);
    }
}
