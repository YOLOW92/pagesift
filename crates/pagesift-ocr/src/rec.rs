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

use pagesift_infer::exec::{Cpu, Exec};
use pagesift_infer::{Activation, BN_EPS, BatchNorm, Conv2d, LayerNorm, Linear, Tensor, Weights};

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

    fn forward_on<E: Exec>(&self, e: &E, x: &E::T) -> E::T {
        let mut y = e.conv(&self.conv, x);
        e.act(self.act, &mut y);
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
    fn forward_on<E: Exec>(&self, e: &E, x: &E::T) -> E::T {
        // [T, 3C]，列 = s·C + h·hd + d；分数的缩放见 Exec::attention
        let qkv = e.linear(&self.qkv, x);
        let o = e.attention(&qkv, self.heads);
        e.linear(&self.proj, &o)
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

    fn forward_on<E: Exec>(&self, e: &E, x: E::T) -> E::T {
        let x = e.add(&x, &self.attn.forward_on(e, &e.layer_norm(&self.ln1, &x)));
        let mut h = e.linear(&self.fc1, &e.layer_norm(&self.ln2, &x));
        e.act(self.act, &mut h);
        e.add(&x, &e.linear(&self.fc2, &h))
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
            a => return Err(format!("unsupported hidden_act {a}")),
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
            return Err("no svtr_block in the weights".into());
        }
        let hidden = w.get(&format!("{p}.norm.weight"))?.shape[0];
        if hidden % heads != 0 {
            return Err(format!(
                "hidden_size {hidden} is not divisible by {heads} heads"
            ));
        }
        let chars: Vec<String> = pre["character_list"]
            .as_array()
            .ok_or("preprocessor_config.json has no character_list")?
            .iter()
            .map(|c| {
                c.as_str()
                    .map(str::to_owned)
                    .ok_or("the character list has a non-string item")
            })
            .collect::<Result<_, _>>()?;
        let head = Linear::load(&w, "head.head")?;
        if head.weight_t.shape[1] != chars.len() {
            return Err(format!(
                "output layer has {} classes but the character list has {} items",
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
        let x = self.preprocess(line)?;
        let logits = self.logits_on(&Cpu, &x);
        let rows = Cpu.ctc_digest(&logits);
        Ok(self.decode(&rows[0]))
    }

    /// 网络输入的通道数。
    pub fn input_channels(&self) -> usize {
        3 // [协议] BGR 三通道（见 preprocess）
    }

    /// 网络输入的高。
    pub fn input_height(&self) -> usize {
        TARGET_H
    }

    /// 预处理一行图成网络输入 `[3, 48, W]`。
    pub fn preprocess(&self, line: &Image) -> Result<Tensor, String> {
        if line.width == 0 || line.height == 0 {
            return Err("empty line image".into());
        }
        if line.channels != 1 && line.channels != 3 {
            return Err(format!(
                "images with {} channels are not supported",
                line.channels
            ));
        }
        Ok(preprocess(line, self.max_w))
    }

    /// 网络部分：`[3, 48, W]` → `[T, 字符表]` 的 logits，在执行器上算。
    pub fn logits_on<E: Exec>(&self, e: &E, x: &E::T) -> E::T {
        let last = self.backbone.forward_last_on(e, x);
        // [协议] `PPOCRV6SmallRecModel.forward`：F.avg_pool2d(stage4, (3, 2))
        let pooled = e.avg_pool2d(&last, (3, 2));
        drop(last);
        let seq = self.encode_on(e, &pooled);
        e.linear(&self.head, &seq)
    }

    /// EncoderWithSVTR：`[C, 1, T]` → `[T, hidden]`。
    fn encode_on<E: Exec>(&self, e: &E, x: &E::T) -> E::T {
        let residual = e.chw_to_seq(&self.skip.forward_on(e, x));
        let r = self.reduce.forward_on(e, x);
        let r = e.add(&r, &self.local.forward_on(e, &r));
        let mut s = e.chw_to_seq(&r);
        drop(r);
        for b in &self.blocks {
            s = b.forward_on(e, s);
        }
        // 高已是 1，序列顺序即从左到右
        e.add(&e.layer_norm(&self.norm, &s), &residual)
    }

    /// CTC 贪心解码：输入每步的（最大类，其概率）。先去相邻重复、再去 blank；
    /// 置信度是保留下来的各字最大概率的平均。
    pub fn decode(&self, rows: &[(u32, f32)]) -> (String, f32) {
        let mut text = String::new();
        let (mut sum, mut kept) = (0f32, 0usize);
        let mut prev = u32::MAX;
        for &(arg, p) in rows {
            // [协议] 下标 0 是 blank（character_list[0] = "blank"）
            if arg != prev && arg != 0 {
                text.push_str(&self.chars[arg as usize]);
                sum += p;
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
