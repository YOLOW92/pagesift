//! 文字识别（PP-OCRv6 small rec）。结构照 transformers 的 `modeling_pp_ocrv6_small_rec.py`
//! 与 `image_processing_pp_ocrv6_small_rec.py`（Apache-2.0，PaddlePaddle 与 HuggingFace）：
//!
//! 行图 → 缩放到高 48（双线性）、BGR、归一化到 [−1, 1]、不足 320 宽右边补 0 →
//! LCNetV4 骨干取第 4 段 `[384, 3, W/4]` → avg_pool2d(3, 2) 得 `[384, 1, W/8]` →
//! EncoderWithSVTR（两路 1×1 卷积、1×7 深度卷积残差、2 层 Transformer、LayerNorm、与第一路相加）→
//! 全连接到字符表 → softmax → CTC 贪心解码。
//!
//! 卷积后的批归一化在载入时并进卷积。

use std::path::Path;

use pagesift_infer::{
    Activation, BN_EPS, BatchNorm, Conv2d, LayerNorm, Linear, Tensor, Weights, ops,
};

use crate::Image;
use crate::lcnet::{Backbone, Config as BackboneConfig};

/// [协议] `PPOCRV6SmallRecImageProcessor.size["height"]`：输入高 48
const TARGET_H: usize = 48;
/// [协议] `size["width"]` 与 `pad_size["width"]`：最小宽 320，不足时右边补 0
const MIN_W: usize = 320;
/// [协议] 预处理缺省 `max_image_width = 3200`（preprocessor_config.json 可覆盖）
const DEFAULT_MAX_W: usize = 3200;
/// [协议] `PPOCRV6SmallRecConfig.layer_norm_eps` 缺省 1e-6
const LN_EPS: f32 = 1e-6;
/// [协议] `PPOCRV6SmallRecConfig.num_attention_heads` 缺省 8
const DEFAULT_HEADS: usize = 8;

/// 卷积 + 批归一化（已并入）+ 激活，对应参考实现的 `PPOCRV6SmallRecConvLayer`。
struct ConvAct {
    conv: Conv2d,
    act: Activation,
}

impl ConvAct {
    /// 读 `{p}.convolution`、`{p}.normalization`；补边 (kh/2, kw/2)，深度卷积时 groups = 输出通道。
    fn load(w: &Weights, p: &str, depthwise: bool, act: Activation) -> Result<ConvAct, String> {
        let shape = w.get(&format!("{p}.convolution.weight"))?.shape.clone();
        let groups = if depthwise { shape[0] } else { 1 };
        let mut conv = Conv2d::load(
            w,
            &format!("{p}.convolution"),
            (1, 1),
            (shape[2] / 2, shape[3] / 2),
            (1, 1),
            groups,
        )?;
        conv.fold(&BatchNorm::load(w, &format!("{p}.normalization"), BN_EPS)?);
        Ok(ConvAct { conv, act })
    }

    fn forward(&self, x: &Tensor) -> Tensor {
        let mut y = self.conv.forward(x);
        self.act.apply_all(&mut y);
        y
    }
}

/// 多头自注意力：`qkv` 一个线性层出 3·C 维，按 (3, heads, head_dim) 拆。
struct Attention {
    qkv: Linear,
    proj: Linear,
    heads: usize,
}

impl Attention {
    fn forward(&self, x: &Tensor) -> Tensor {
        let (t, c) = x.tc();
        let hd = c / self.heads;
        let qkv = self.qkv.forward(x); // [T, 3C]，列 = s·C + h·hd + d
        // [协议] scaling = head_dim^−0.5（`PPOCRV6SmallRecAttention.scale`）
        let scale = (hd as f32).powf(-0.5);
        let mut out = vec![0f32; t * c];
        // 取第 s 份（0 = q、1 = k、2 = v）第 h 个头，成 [T, hd]
        let take = |s: usize, h: usize| {
            let mut d = Vec::with_capacity(t * hd);
            for row in qkv.data.chunks(3 * c) {
                let at = s * c + h * hd;
                d.extend_from_slice(&row[at..at + hd]);
            }
            Tensor::new(vec![t, hd], d)
        };
        for h in 0..self.heads {
            let q = take(0, h);
            let kt = ops::transpose(&take(1, h));
            let v = take(2, h);
            let mut a = ops::matmul(&q, &kt); // [T, T]
            for s in &mut a.data {
                *s *= scale;
            }
            ops::softmax_rows(&mut a);
            let o = ops::matmul(&a, &v); // [T, hd]
            // 各头拼回 [T, C]：列 = h·hd + d
            for (i, row) in o.data.chunks(hd).enumerate() {
                out[i * c + h * hd..i * c + (h + 1) * hd].copy_from_slice(row);
            }
        }
        self.proj.forward(&Tensor::new(vec![t, c], out))
    }
}

/// Transformer 块（前置 LayerNorm）：x + attn(ln1(x))，再 x + mlp(ln2(x))。
struct Block {
    ln1: LayerNorm,
    attn: Attention,
    ln2: LayerNorm,
    fc1: Linear,
    fc2: Linear,
    act: Activation,
}

impl Block {
    fn load(w: &Weights, p: &str, heads: usize, act: Activation) -> Result<Block, String> {
        Ok(Block {
            ln1: LayerNorm::load(w, &format!("{p}.layer_norm1"), LN_EPS)?,
            attn: Attention {
                qkv: Linear::load(w, &format!("{p}.self_attn.qkv"))?,
                proj: Linear::load(w, &format!("{p}.self_attn.projection"))?,
                heads,
            },
            ln2: LayerNorm::load(w, &format!("{p}.layer_norm2"), LN_EPS)?,
            fc1: Linear::load(w, &format!("{p}.mlp.fc1"))?,
            fc2: Linear::load(w, &format!("{p}.mlp.fc2"))?,
            act,
        })
    }

    fn forward(&self, x: Tensor) -> Tensor {
        let x = ops::add(&x, &self.attn.forward(&self.ln1.forward(&x)));
        let mut h = self.fc1.forward(&self.ln2.forward(&x));
        self.act.apply_all(&mut h);
        ops::add(&x, &self.fc2.forward(&h))
    }
}

/// 载入好的识别模型。
pub struct Rec {
    backbone: Backbone,
    /// conv_block 0（skip_conv）、1（conv_reduce）、2（local_conv，深度 1×7）
    skip: ConvAct,
    reduce: ConvAct,
    local: ConvAct,
    blocks: Vec<Block>,
    norm: LayerNorm,
    head: Linear,
    /// 字符表，下标 0 是 CTC 的 blank
    chars: Vec<String>,
    max_w: usize,
}

impl Rec {
    /// `dir` 是模型目录：model.safetensors、config.json、preprocessor_config.json（含字符表）。
    pub fn load(dir: &Path) -> Result<Rec, String> {
        let cfg = crate::read_json(dir, "config.json")?;
        let pre = crate::read_json(dir, "preprocessor_config.json")?;
        let w = crate::read_weights(dir)?;
        // [协议] 权重名前缀：`PPOCRV6SmallRecForTextRecognition` 的 model.backbone 与 head.encoder
        let backbone = Backbone::load(
            &w,
            "model.backbone.encoder",
            &BackboneConfig::from_json(&cfg)?,
        )?;
        // [协议] `hidden_act` 缺省 "silu"，颈部卷积与 MLP 共用
        let act = match cfg["hidden_act"].as_str().unwrap_or("silu") {
            "silu" | "swish" => Activation::Silu,
            "gelu" => Activation::Gelu,
            "relu" => Activation::Relu,
            a => return Err(format!("不支持的 hidden_act {a}")),
        };
        let heads = cfg["num_attention_heads"]
            .as_u64()
            .map_or(DEFAULT_HEADS, |v| v as usize);
        let p = "head.encoder";
        let mut blocks = Vec::new();
        while w
            .opt(&format!(
                "{p}.svtr_block.{}.layer_norm1.weight",
                blocks.len()
            ))
            .is_some()
        {
            let i = blocks.len();
            blocks.push(Block::load(&w, &format!("{p}.svtr_block.{i}"), heads, act)?);
        }
        if blocks.is_empty() {
            return Err("权重里没有 svtr_block".into());
        }
        let hidden = w.get(&format!("{p}.norm.weight"))?.shape[0];
        if hidden % heads != 0 {
            return Err(format!("hidden_size {hidden} 不能被头数 {heads} 整除"));
        }
        let chars: Vec<String> = pre["character_list"]
            .as_array()
            .ok_or("preprocessor_config.json 缺 character_list")?
            .iter()
            .map(|c| c.as_str().map(str::to_owned).ok_or("字符表里有非字符串项"))
            .collect::<Result<_, _>>()?;
        let head = Linear::load(&w, "head.head")?;
        if head.weight_t.shape[1] != chars.len() {
            return Err(format!(
                "输出层 {} 类与字符表 {} 项不符",
                head.weight_t.shape[1],
                chars.len()
            ));
        }
        Ok(Rec {
            backbone,
            skip: ConvAct::load(&w, &format!("{p}.conv_block.0"), false, act)?,
            reduce: ConvAct::load(&w, &format!("{p}.conv_block.1"), false, act)?,
            local: ConvAct::load(&w, &format!("{p}.conv_block.2"), true, act)?,
            blocks,
            norm: LayerNorm::load(&w, &format!("{p}.norm"), LN_EPS)?,
            head,
            chars,
            max_w: pre["max_image_width"]
                .as_u64()
                .map_or(DEFAULT_MAX_W, |v| v as usize),
        })
    }

    /// 识别一行字的图（已裁出、拉正成水平的一行）。返回文字和置信度。
    pub fn recognize(&self, line: &Image) -> Result<(String, f32), String> {
        if line.width == 0 || line.height == 0 {
            return Err("行图为空".into());
        }
        if line.channels != 1 && line.channels != 3 {
            return Err(format!("不支持 {} 通道的图", line.channels));
        }
        let x = preprocess(line, self.max_w);
        let feats = self.backbone.forward(&x);
        let last = feats.last().ok_or("骨干没有输出")?;
        // [协议] `PPOCRV6SmallRecModel.forward`：F.avg_pool2d(stage4, (3, 2))
        let pooled = ops::avg_pool2d(last, (3, 2));
        let seq = self.encode(&pooled);
        let logits = self.head.forward(&seq);
        Ok(self.decode(&logits))
    }

    /// EncoderWithSVTR：`[C, 1, T]` → `[T, hidden]`。
    fn encode(&self, x: &Tensor) -> Tensor {
        let (_, h, w) = x.chw();
        let residual = ops::chw_to_seq(&self.skip.forward(x));
        let r = self.reduce.forward(x);
        let r = ops::add(&r, &self.local.forward(&r));
        let mut s = ops::chw_to_seq(&r);
        for b in &self.blocks {
            s = b.forward(s);
        }
        debug_assert_eq!(s.tc().0, h * w);
        // 高已是 1，序列顺序即从左到右
        ops::add(&self.norm.forward(&s), &residual)
    }

    /// softmax + CTC 贪心解码：每步取概率最大的类，先去相邻重复、再去 blank；
    /// 置信度是保留下来的各字最大概率的平均。
    fn decode(&self, logits: &Tensor) -> (String, f32) {
        let (_, n) = logits.tc();
        let mut text = String::new();
        let (mut sum, mut kept) = (0f32, 0usize);
        let mut prev = usize::MAX;
        for row in logits.data.chunks(n) {
            // softmax 的最大概率 = 1 / Σ exp(xᵢ − max)，取最大的类不用整行归一化
            let (mut arg, mut m) = (0usize, f32::NEG_INFINITY);
            for (i, &v) in row.iter().enumerate() {
                if v > m {
                    (arg, m) = (i, v);
                }
            }
            let z: f32 = row.iter().map(|&v| (v - m).exp()).sum();
            // [协议] 下标 0 是 blank（character_list[0] = "blank"）
            if arg != prev && arg != 0 {
                text.push_str(&self.chars[arg]);
                sum += 1.0 / z;
                kept += 1;
            }
            prev = arg;
        }
        // 没有字时参考实现对空张量求均值得 NaN，这里给 0
        let score = if kept == 0 { 0.0 } else { sum / kept as f32 };
        (text, score)
    }
}

/// 目标宽度，照 `PPOCRV6SmallRecImageProcessor.get_target_size`（单张图）：
/// 宽高比不小于 320/48 时取 ⌊48·w/h⌋（恰为整数时就是它），超过上限取上限（此时宽被压扁）；
/// 更窄的图取 ⌈48·w/h⌉，之后再补到 320。
fn target_width(w: usize, h: usize, max_w: usize) -> usize {
    let r = w as f64 / h as f64;
    let ratio = r.max(MIN_W as f64 / TARGET_H as f64);
    let tw = (TARGET_H as f64 * ratio) as usize;
    if tw > max_w {
        return max_w;
    }
    let exact = (TARGET_H as f64 * r).ceil() as usize;
    if tw >= exact { exact.max(1) } else { tw }
}

/// 预处理成 `[3, 48, W]`：双线性缩放（不抗锯齿，结果舍入回 8 位）→ BGR →
/// (v/255 − 0.5)/0.5 → 不足 320 宽右边补 0（补在归一化之后）。
fn preprocess(img: &Image, max_w: usize) -> Tensor {
    let tw = target_width(img.width, img.height, max_w);
    let (th, out_w) = (TARGET_H, tw.max(MIN_W));
    // [协议] `do_pad`：宽不足 pad_size 时 tvF.pad(fill=0) 补在右、下，作用在归一化之后的张量上；
    // 缓冲先全置 0，右边多出的列就是补边
    let mut data = vec![0f32; 3 * th * out_w];
    let xs = taps(img.width, tw);
    let ys = taps(img.height, th);
    for (oy, &(y0, y1, fy)) in ys.iter().enumerate() {
        for (ox, &(x0, x1, fx)) in xs.iter().enumerate() {
            for c in 0..3 {
                let p = |x: usize, y: usize| f32::from(img.at(x, y, c));
                let top = p(x0, y0) + (p(x1, y0) - p(x0, y0)) * fx;
                let bot = p(x0, y1) + (p(x1, y1) - p(x0, y1)) * fx;
                // [协议] torchvision 对 uint8 图插值后 round 再截到 0–255
                let v = (top + (bot - top) * fy).round_ties_even().clamp(0.0, 255.0);
                // [协议] RGB → BGR（`stacked_images[:, [2, 1, 0]]`）
                let oc = 2 - c;
                // [协议] rescale 1/255 后按 IMAGENET_STANDARD_MEAN/STD（0.5、0.5）归一化
                data[(oc * th + oy) * out_w + ox] = (v / 255.0 - 0.5) / 0.5;
            }
        }
    }
    Tensor::new(vec![3, th, out_w], data)
}

/// 一维双线性的取样点（PyTorch `align_corners=False`、按尺寸缩放）：
/// 源坐标 = (目标 + 0.5)·入/出 − 0.5，负数截到 0；返回 (左, 右, 右的权重)。
fn taps(n_in: usize, n_out: usize) -> Vec<(usize, usize, f32)> {
    let scale = n_in as f32 / n_out as f32;
    (0..n_out)
        .map(|o| {
            let src = ((o as f32 + 0.5) * scale - 0.5).max(0.0);
            let i0 = (src as usize).min(n_in - 1);
            let i1 = (i0 + 1).min(n_in - 1);
            (i0, i1, src - i0 as f32)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_width_follows_reference() {
        // 宽行：⌊48·w/h⌋
        assert_eq!(target_width(960, 48, 3200), 960);
        assert_eq!(target_width(1001, 30, 3200), 1601);
        // 窄行：⌈48·w/h⌉，之后补到 320
        assert_eq!(target_width(100, 30, 3200), 160);
        assert_eq!(target_width(101, 30, 3200), 162);
        // 超上限压到上限
        assert_eq!(target_width(10000, 30, 3200), 3200);
    }

    #[test]
    fn taps_identity_and_half() {
        let t = taps(4, 4);
        assert!(
            t.iter()
                .enumerate()
                .all(|(i, &(a, _, f))| a == i && f == 0.0)
        );
        let t = taps(4, 2);
        assert_eq!((t[0].0, t[0].1), (0, 1));
        assert!((t[0].2 - 0.5).abs() < 1e-6);
    }
}
