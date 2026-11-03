//! 混合编码器（`PPDocLayoutV3HybridEncoder`，RT-DETR 式）：骨干后三段各投影到同一通道 → 最高层做
//! 层内自注意力（AIFI）→ 自顶向下 FPN + 自底向上 PAN 融合；另外用融合结果和骨干第一段算掩码特征。
//! RepVGG 块的两路卷积在载入时并成一个 3×3，结果只差浮点舍入。

use pagesift_infer::{Activation, Conv2d, LayerNorm, Tensor, Weights, ops};

use super::config::Config;
use super::layers::{
    Ffn, SelfAttention, add_norm, conv_bn, conv_layer, conv_norm, count, resize_bilinear,
    sine_pos_2d,
};
use crate::lcnet::ConvBn;

/// AIFI 的一层 Transformer（`PPDocLayoutV3EncoderLayer`，后归一化）。
#[derive(Debug, Clone)]
struct AifiLayer {
    attn: SelfAttention,
    norm1: LayerNorm,
    ffn: Ffn,
    norm2: LayerNorm,
    pos_temperature: f64,
}

impl AifiLayer {
    fn load(w: &Weights, p: &str, cfg: &Config) -> Result<AifiLayer, String> {
        let eps = cfg.layer_norm_eps;
        Ok(AifiLayer {
            attn: SelfAttention::load(w, &format!("{p}.self_attn"), cfg.encoder_heads)?,
            norm1: LayerNorm::load(w, &format!("{p}.self_attn_layer_norm"), eps)?,
            ffn: Ffn::load(w, p, cfg.encoder_act)?,
            norm2: LayerNorm::load(w, &format!("{p}.final_layer_norm"), eps)?,
            pos_temperature: cfg.pos_temperature,
        })
    }

    fn forward(&self, x: &Tensor) -> Tensor {
        let (c, h, w) = x.chw();
        let seq = ops::chw_to_seq(x);
        let pos = sine_pos_2d(h, w, c, self.pos_temperature);
        let y = add_norm(&seq, &self.attn.forward(&seq, &pos), &self.norm1);
        let z = add_norm(&y, &self.ffn.forward(&y), &self.norm2);
        ops::seq_to_chw(&z, h, w)
    }
}

/// 把 1×1 卷积的权重并进 3×3 卷积的中心、偏置相加：`a(x) + b(x)` 变成一次卷积（RepVGG 重参数化）。
fn merge_rep(mut a: ConvBn, b: &ConvBn) -> Result<ConvBn, String> {
    let (o, c, kh, kw) = (
        a.conv.weight.shape[0],
        a.conv.weight.shape[1],
        a.conv.weight.shape[2],
        a.conv.weight.shape[3],
    );
    if b.conv.weight.shape != [o, c, 1, 1] {
        return Err("RepVGG 两路卷积的通道不同".into());
    }
    let center = (kh / 2) * kw + kw / 2;
    let per = kh * kw;
    for (i, v) in b.conv.weight.data.iter().enumerate() {
        a.conv.weight.data[i * per + center] += v;
    }
    if let (Some(x), Some(y)) = (&mut a.conv.bias, &b.conv.bias) {
        for (x, y) in x.iter_mut().zip(y) {
            *x += y;
        }
    }
    Ok(a)
}

/// `PPDocLayoutV3CSPRepLayer`：两个 1×1 分流，一路过若干 RepVGG 块，再相加。
#[derive(Debug, Clone)]
struct CspRep {
    conv1: ConvBn,
    conv2: ConvBn,
    blocks: Vec<ConvBn>,
}

impl CspRep {
    fn load(w: &Weights, p: &str, act: Activation) -> Result<CspRep, String> {
        let n = count(w, |i| format!("{p}.bottlenecks.{i}.conv1.conv.weight"));
        let mut blocks = Vec::with_capacity(n);
        for i in 0..n {
            let b = format!("{p}.bottlenecks.{i}");
            // RepVggBlock：两路都不带激活，相加后再激活
            let c3 = conv_norm(w, &format!("{b}.conv1"), 1, act)?;
            let c1 = conv_norm(w, &format!("{b}.conv2"), 1, Activation::Identity)?;
            blocks.push(merge_rep(c3, &c1)?);
        }
        Ok(CspRep {
            conv1: conv_norm(w, &format!("{p}.conv1"), 1, act)?,
            conv2: conv_norm(w, &format!("{p}.conv2"), 1, act)?,
            blocks,
        })
    }

    fn forward(&self, x: &Tensor) -> Tensor {
        let mut a = self.conv1.forward(x);
        for b in &self.blocks {
            a = b.forward(&a);
        }
        ops::add(&a, &self.conv2.forward(x))
    }
}

/// 掩码特征头（`PPDocLayoutV3MaskFeatFPN` 及其后的上采样、横向连接、输出卷积）。
#[derive(Debug, Clone)]
struct MaskHead {
    /// 每层特征一组卷积；除了步长最小的那层，每个卷积后面都上采样两倍。
    scale_heads: Vec<Vec<ConvBn>>,
    output_conv: ConvBn,
    lateral: ConvBn,
    base_conv: ConvBn,
    conv: Conv2d,
}

impl MaskHead {
    fn load(w: &Weights, p: &str, cfg: &Config) -> Result<MaskHead, String> {
        let act = Activation::Silu; // [协议] 这几层在源码里写死 activation="silu"
        let mut scale_heads = Vec::with_capacity(cfg.feat_strides.len());
        let base = cfg.feat_strides[0];
        for (i, &stride) in cfg.feat_strides.iter().enumerate() {
            // head_length = max(1, log2(步长 / 基础步长))；ModuleList 里卷积与 Upsample 交替，
            // 所以有上采样时卷积在偶数下标
            let n = (stride / base).ilog2().max(1) as usize;
            let step = if stride != base { 2 } else { 1 };
            let head = (0..n)
                .map(|k| {
                    conv_layer(
                        w,
                        &format!("{p}.mask_feature_head.scale_heads.{i}.layers.{}", k * step),
                        1,
                        false,
                        act,
                    )
                })
                .collect::<Result<_, _>>()?;
            scale_heads.push(head);
        }
        Ok(MaskHead {
            scale_heads,
            output_conv: conv_layer(
                w,
                &format!("{p}.mask_feature_head.output_conv"),
                1,
                false,
                act,
            )?,
            lateral: conv_layer(w, &format!("{p}.encoder_mask_lateral"), 1, false, act)?,
            base_conv: conv_layer(
                w,
                &format!("{p}.encoder_mask_output.base_conv"),
                1,
                false,
                act,
            )?,
            conv: Conv2d::load(
                w,
                &format!("{p}.encoder_mask_output.conv"),
                (1, 1),
                (0, 0),
                (1, 1),
                1,
            )?,
        })
    }

    /// `pan` 按步长从小到大；`x4` 是骨干第一段。结果 `[原型数, H/4, W/4]`。
    fn forward(&self, pan: &[Tensor], x4: &Tensor) -> Tensor {
        let run = |i: usize, x: &Tensor| {
            let mut h = x.clone();
            for c in &self.scale_heads[i] {
                h = c.forward(&h);
                if i != 0 {
                    let (_, ph, pw) = h.chw();
                    h = resize_bilinear(&h, ph * 2, pw * 2);
                }
            }
            h
        };
        let mut out = run(0, &pan[0]);
        let (_, oh, ow) = out.chw();
        for (i, p) in pan.iter().enumerate().skip(1) {
            out = ops::add(&out, &resize_bilinear(&run(i, p), oh, ow));
        }
        let out = self.output_conv.forward(&out);
        let up = resize_bilinear(&out, oh * 2, ow * 2);
        let fused = ops::add(&up, &self.lateral.forward(x4));
        self.conv.forward(&self.base_conv.forward(&fused))
    }
}

/// 整个混合编码器。
#[derive(Debug, Clone)]
pub struct Encoder {
    input_proj: Vec<ConvBn>,
    aifi: AifiLayer,
    aifi_level: usize,
    lateral: Vec<ConvBn>,
    fpn: Vec<CspRep>,
    down: Vec<ConvBn>,
    pan: Vec<CspRep>,
    mask: MaskHead,
}

/// 编码器的输出。
#[derive(Debug, Clone)]
pub struct EncoderOut {
    /// 融合后的各层特征，步长从小到大。
    pub feats: Vec<Tensor>,
    /// 掩码特征 `[原型数, H/4, W/4]`。
    pub mask_feat: Tensor,
}

/// [协议] `PPDocLayoutV3ConvNormLayer` 下采样卷积的步长
const PAN_STRIDE: usize = 2;

impl Encoder {
    pub fn load(w: &Weights, cfg: &Config) -> Result<Encoder, String> {
        let levels = cfg.feat_strides.len();
        let act = cfg.act;
        let p = "model.encoder";
        let input_proj = (0..levels)
            .map(|i| {
                conv_bn(
                    w,
                    &format!("model.encoder_input_proj.{i}.0"),
                    &format!("model.encoder_input_proj.{i}.1"),
                    1,
                    0,
                    false,
                    Activation::Identity,
                )
            })
            .collect::<Result<_, _>>()?;
        let many = |name: &str, stride: usize| -> Result<Vec<ConvBn>, String> {
            (0..levels - 1)
                .map(|i| conv_norm(w, &format!("{p}.{name}.{i}"), stride, act))
                .collect()
        };
        let csp = |name: &str| -> Result<Vec<CspRep>, String> {
            (0..levels - 1)
                .map(|i| CspRep::load(w, &format!("{p}.{name}.{i}"), act))
                .collect()
        };
        Ok(Encoder {
            input_proj,
            // [协议] 发布权重里 AIFI 叫 encoder.0.layers.0（transformers 载入时才改名 aifi）
            aifi: AifiLayer::load(w, &format!("{p}.encoder.0.layers.0"), cfg)?,
            aifi_level: cfg.aifi_level,
            lateral: many("lateral_convs", 1)?,
            fpn: csp("fpn_blocks")?,
            down: many("downsample_convs", PAN_STRIDE)?,
            pan: csp("pan_blocks")?,
            mask: MaskHead::load(w, p, cfg)?,
        })
    }

    /// `backbone` 是骨干各段的输出：第一段给掩码特征，其余进 FPN。
    pub fn forward(&self, backbone: &[Tensor]) -> EncoderOut {
        let (x4, rest) = backbone.split_first().expect("骨干没有输出");
        let mut f: Vec<Tensor> = self
            .input_proj
            .iter()
            .zip(rest)
            .map(|(c, x)| c.forward(x))
            .collect();
        f[self.aifi_level] = self.aifi.forward(&f[self.aifi_level]);

        // 自顶向下：上一层经横向卷积后上采样，与下一层拼接
        let stages = self.lateral.len();
        let mut fpn = vec![f[stages].clone()];
        for (idx, (lat, block)) in self.lateral.iter().zip(&self.fpn).enumerate() {
            let top = lat.forward(fpn.last().expect("fpn 不空"));
            let (_, h, w) = top.chw();
            let up = ops::interpolate_nearest(&top, h * 2, w * 2);
            *fpn.last_mut().expect("fpn 不空") = top;
            let cat = ops::concat_channels(&[&up, &f[stages - idx - 1]]);
            fpn.push(block.forward(&cat));
        }
        fpn.reverse();

        // 自底向上：下一层经下采样后与上一层拼接
        let mut pan = vec![fpn[0].clone()];
        for (idx, (down, block)) in self.down.iter().zip(&self.pan).enumerate() {
            let d = down.forward(pan.last().expect("pan 不空"));
            let cat = ops::concat_channels(&[&d, &fpn[idx + 1]]);
            pan.push(block.forward(&cat));
        }
        let mask_feat = self.mask.forward(&pan, x4);
        EncoderOut {
            feats: pan,
            mask_feat,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repvgg_合并等于两路相加() {
        // 3×3 + 1×1 并成一个 3×3 后，前向结果应等于两路分别卷积再相加
        let wa: Vec<f32> = (0..2 * 2 * 9).map(|i| (i as f32 * 0.37).sin()).collect();
        let wb: Vec<f32> = (0..2 * 2).map(|i| (i as f32 * 0.91).cos()).collect();
        let conv = |shape: Vec<usize>, data: Vec<f32>, pad: usize, bias: Vec<f32>| ConvBn {
            conv: Conv2d {
                weight: Tensor::new(shape, data),
                bias: Some(bias),
                stride: (1, 1),
                pad: (pad, pad),
                dilation: (1, 1),
                groups: 1,
            },
            act: Activation::Identity,
        };
        let a = conv(vec![2, 2, 3, 3], wa, 1, vec![0.1, -0.2]);
        let b = conv(vec![2, 2, 1, 1], wb, 0, vec![0.3, 0.4]);
        let x = Tensor::new(
            vec![2, 4, 5],
            (0..40).map(|i| (i as f32 * 0.13).sin()).collect(),
        );
        let want = ops::add(&a.forward(&x), &b.forward(&x));
        let got = merge_rep(a, &b).unwrap().forward(&x);
        for (p, q) in got.data.iter().zip(&want.data) {
            assert!((p - q).abs() < 1e-5, "{p} {q}");
        }
    }
}
