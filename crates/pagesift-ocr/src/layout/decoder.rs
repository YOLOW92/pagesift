//! 解码器（`PPDocLayoutV3Decoder`）：6 层（自注意力 + 可变形交叉注意力 + 前馈），每层之后用框头迭代
//! 修正参考框。类别头、框头与编码器的 `enc_score_head`、`enc_bbox_head` 共用权重（`_tied_weights_keys`，
//! 权重文件里没有另存）。阅读顺序头（`decoder_order_head` + 全局指针）只算最后一层。逐层掩码输出不算。

use pagesift_infer::{LayerNorm, Linear, Tensor, Weights, ops};

use super::config::Config;
use super::deform::{DeformAttn, Level};
use super::layers::{Ffn, Mlp, SelfAttention, add_norm, conv_bn, count};
use super::query::{Bbox, QuerySelect, inverse_sigmoid, sigmoid};
use crate::lcnet::ConvBn;

/// 解码器的一层（`PPDocLayoutV3DecoderLayer`，后归一化）。
#[derive(Debug, Clone)]
struct Layer {
    attn: SelfAttention,
    norm1: LayerNorm,
    cross: DeformAttn,
    norm2: LayerNorm,
    ffn: Ffn,
    norm3: LayerNorm,
}

impl Layer {
    fn load(w: &Weights, p: &str, cfg: &Config) -> Result<Layer, String> {
        let eps = cfg.layer_norm_eps;
        Ok(Layer {
            attn: SelfAttention::load(w, &format!("{p}.self_attn"), cfg.decoder_heads)?,
            norm1: LayerNorm::load(w, &format!("{p}.self_attn_layer_norm"), eps)?,
            cross: DeformAttn::load(
                w,
                &format!("{p}.encoder_attn"),
                cfg.decoder_heads,
                cfg.decoder_points,
            )?,
            norm2: LayerNorm::load(w, &format!("{p}.encoder_attn_layer_norm"), eps)?,
            ffn: Ffn::load(w, p, cfg.decoder_act)?,
            norm3: LayerNorm::load(w, &format!("{p}.final_layer_norm"), eps)?,
        })
    }

    fn forward(
        &self,
        x: &Tensor,
        pos: &Tensor,
        src: &Tensor,
        levels: &[Level],
        refs: &[Bbox],
    ) -> Tensor {
        let h = add_norm(x, &self.attn.forward(x, pos), &self.norm1);
        let cross = self.cross.forward(&ops::add(&h, pos), src, levels, refs);
        let h = add_norm(&h, &cross, &self.norm2);
        add_norm(&h, &self.ffn.forward(&h), &self.norm3)
    }
}

/// 解码器的原始输出。
#[derive(Debug, Clone)]
pub struct Raw {
    /// 类别 logit `[Q, K]`。
    pub logits: Tensor,
    /// 框 (cx, cy, w, h)，相对网络输入 0–1。
    pub boxes: Vec<Bbox>,
    /// 阅读顺序 logit `[Q, Q]`，下三角（含对角线）无意义。
    pub order: Tensor,
}

/// 整个解码端：输入投影、查询选取、解码层和各个头。
#[derive(Debug, Clone)]
pub struct Decoder {
    input_proj: Vec<ConvBn>,
    select: QuerySelect,
    layers: Vec<Layer>,
    query_pos: Mlp,
    bbox: Mlp,
    class: Linear,
    norm: LayerNorm,
    order_head: Linear,
    pointer: Linear,
    pointer_dim: usize,
}

impl Decoder {
    pub fn load(w: &Weights, cfg: &Config) -> Result<Decoder, String> {
        let n = count(w, |i| {
            format!("model.decoder.layers.{i}.self_attn.q_proj.weight")
        });
        if n == 0 {
            return Err("权重里没有解码层".into());
        }
        let layers = (0..n)
            .map(|i| Layer::load(w, &format!("model.decoder.layers.{i}"), cfg))
            .collect::<Result<_, _>>()?;
        let input_proj = (0..cfg.feat_strides.len())
            .map(|i| {
                conv_bn(
                    w,
                    &format!("model.decoder_input_proj.{i}.0"),
                    &format!("model.decoder_input_proj.{i}.1"),
                    1,
                    0,
                    false,
                    pagesift_infer::Activation::Identity,
                )
            })
            .collect::<Result<_, _>>()?;
        Ok(Decoder {
            input_proj,
            select: QuerySelect::load(w, cfg.layer_norm_eps, cfg.num_queries)?,
            layers,
            query_pos: Mlp::load(w, "model.decoder.query_pos_head")?,
            // [协议] 这两个头与编码器共用权重（`_tied_weights_keys`）
            bbox: Mlp::load(w, "model.enc_bbox_head")?,
            class: Linear::load(w, "model.enc_score_head")?,
            norm: LayerNorm::load(w, "model.decoder_norm", cfg.layer_norm_eps)?,
            order_head: Linear::load(w, &format!("model.decoder_order_head.{}", n - 1))?,
            pointer: Linear::load(w, "model.decoder_global_pointer.dense")?,
            pointer_dim: cfg.pointer_dim,
        })
    }

    /// `feats` 是编码器各层输出（步长从小到大），`mask_feat` 是掩码特征。
    pub fn forward(&self, feats: &[Tensor], mask_feat: &Tensor) -> Raw {
        let mut levels = Vec::with_capacity(feats.len());
        let mut data = Vec::new();
        let mut channels = 0;
        let mut start = 0;
        for (proj, f) in self.input_proj.iter().zip(feats) {
            let x = proj.forward(f);
            let (c, h, w) = x.chw();
            levels.push(Level { h, w, start });
            start += h * w;
            channels = c;
            data.extend_from_slice(&ops::chw_to_seq(&x).data);
        }
        let src = Tensor::new(vec![start, channels], data);

        let (mut h, mut refs) = self.select.forward(&src, &levels, mask_feat);
        for l in &self.layers {
            let pos = self.query_pos.forward(&boxes_tensor(&refs));
            h = l.forward(&h, &pos, &src, &levels, &refs);
            let delta = self.bbox.forward(&h);
            for (r, d) in refs.iter_mut().zip(delta.data.as_chunks::<BOX_LEN>().0) {
                for (v, d) in r.iter_mut().zip(d) {
                    *v = sigmoid(d + inverse_sigmoid(*v));
                }
            }
        }
        let q = self.norm.forward(&h);
        let logits = self.class.forward(&q);
        let order = pointer_logits(
            &self.pointer.forward(&self.order_head.forward(&q)),
            self.pointer_dim,
        );
        Raw {
            logits,
            boxes: refs,
            order,
        }
    }
}

/// 一个框的分量数。
const BOX_LEN: usize = size_of::<Bbox>() / size_of::<f32>();

fn boxes_tensor(refs: &[Bbox]) -> Tensor {
    Tensor::new(vec![refs.len(), BOX_LEN], refs.concat())
}

/// [协议] 源码 `logits.masked_fill(tril, -1e4)`
const MASKED: f32 = -1e4;

/// 全局指针（`PPDocLayoutV3GlobalPointer`）：`dense` 的输出 `[Q, 2·d]` 前半是 q、后半是 k，
/// logit = qᵢ·kⱼ / √d；下三角（含对角线）填一个很小的数。
pub(crate) fn pointer_logits(dense: &Tensor, d: usize) -> Tensor {
    let (q, c) = dense.tc();
    assert_eq!(c, 2 * d, "全局指针的宽度不符");
    let mut qs = Vec::with_capacity(q * d);
    let mut ks = Vec::with_capacity(q * d);
    for row in dense.data.chunks_exact(c) {
        qs.extend_from_slice(&row[..d]);
        ks.extend_from_slice(&row[d..]);
    }
    let qs = Tensor::new(vec![q, d], qs);
    let ks = ops::transpose(&Tensor::new(vec![q, d], ks));
    let mut out = ops::matmul(&qs, &ks);
    let scale = (d as f32).sqrt().recip();
    for (i, row) in out.data.chunks_exact_mut(q).enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = if j <= i { MASKED } else { *v * scale };
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 全局指针() {
        // 2 个查询，d = 1（√d = 1）：q = [1, 3]，k = [2, 4]
        let dense = Tensor::new(vec![2, 2], vec![1.0, 2.0, 3.0, 4.0]);
        let o = pointer_logits(&dense, 1);
        assert_eq!(o.data, [MASKED, 4.0, MASKED, MASKED]);
    }
}
