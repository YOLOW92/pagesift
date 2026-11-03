//! HGNetV2 骨干。结构照 transformers 的 `modeling_hgnet_v2.py`（Apache-2.0）：stem + 4 段 HG 块，
//! 块里若干卷积的输出连同输入按通道拼起来，再过两个 1×1 卷积聚合。批归一化在载入时并进卷积。
//! 配置（段数、块数、每块层数、核大小、是否轻量块）都从权重推出来，不写死 L 档的表。

use pagesift_infer::{Activation, Tensor, Weights, ops};

use super::layers::{conv_layer, count};
use crate::lcnet::ConvBn;

/// HG 块里的一层。
#[derive(Debug, Clone)]
enum Layer {
    /// `HGNetV2ConvLayer`。
    Plain(ConvBn),
    /// `HGNetV2ConvLayerLight`：1×1（无激活）+ 深度卷积（ReLU）。
    Light(ConvBn, ConvBn),
}

impl Layer {
    fn load(w: &Weights, p: &str) -> Result<Layer, String> {
        if w.opt(&format!("{p}.conv1.convolution.weight")).is_some() {
            Ok(Layer::Light(
                conv_layer(w, &format!("{p}.conv1"), 1, false, Activation::Identity)?,
                conv_layer(w, &format!("{p}.conv2"), 1, true, Activation::Relu)?,
            ))
        } else {
            Ok(Layer::Plain(conv_layer(w, p, 1, false, Activation::Relu)?))
        }
    }

    fn forward(&self, x: &Tensor) -> Tensor {
        match self {
            Layer::Plain(c) => c.forward(x),
            Layer::Light(a, b) => b.forward(&a.forward(x)),
        }
    }
}

/// `HGNetV2BasicLayer`。
#[derive(Debug, Clone)]
struct Block {
    layers: Vec<Layer>,
    squeeze: ConvBn,
    excite: ConvBn,
    residual: bool,
}

impl Block {
    fn load(w: &Weights, p: &str, residual: bool) -> Result<Block, String> {
        // 轻量块的权重在 layers.i.conv1 下，普通块在 layers.i.convolution 下
        let n = count(w, |i| format!("{p}.layers.{i}.convolution.weight")).max(count(w, |i| {
            format!("{p}.layers.{i}.conv1.convolution.weight")
        }));
        if n == 0 {
            return Err(format!("{p} has no convolution layers"));
        }
        let layers = (0..n)
            .map(|i| Layer::load(w, &format!("{p}.layers.{i}")))
            .collect::<Result<_, _>>()?;
        Ok(Block {
            layers,
            squeeze: conv_layer(w, &format!("{p}.aggregation.0"), 1, false, Activation::Relu)?,
            excite: conv_layer(w, &format!("{p}.aggregation.1"), 1, false, Activation::Relu)?,
            residual,
        })
    }

    fn forward(&self, x: &Tensor) -> Tensor {
        let mut outs = Vec::with_capacity(self.layers.len());
        for l in &self.layers {
            let y = l.forward(outs.last().unwrap_or(x));
            outs.push(y);
        }
        let mut parts: Vec<&Tensor> = Vec::with_capacity(outs.len() + 1);
        parts.push(x);
        parts.extend(outs.iter());
        let cat = ops::concat_channels(&parts);
        drop(outs);
        let y = self.excite.forward(&self.squeeze.forward(&cat));
        if self.residual { ops::add(&y, x) } else { y }
    }
}

/// `HGNetV2Stage`。
#[derive(Debug, Clone)]
struct Stage {
    down: Option<ConvBn>,
    blocks: Vec<Block>,
}

/// [协议] `HGNetV2Config` 的 `stage_downsample_strides` 缺省值
const STAGE_STRIDE: usize = 2;

impl Stage {
    fn load(w: &Weights, p: &str) -> Result<Stage, String> {
        let dp = format!("{p}.downsample");
        let down = if w.opt(&format!("{dp}.convolution.weight")).is_some() {
            Some(conv_layer(
                w,
                &dp,
                STAGE_STRIDE,
                true,
                Activation::Identity,
            )?)
        } else {
            None
        };
        let n = count(w, |i| {
            format!("{p}.blocks.{i}.aggregation.0.convolution.weight")
        });
        // `residual = (i != 0)`：每段第一个块改变通道，不做残差
        let blocks = (0..n)
            .map(|i| Block::load(w, &format!("{p}.blocks.{i}"), i != 0))
            .collect::<Result<_, _>>()?;
        Ok(Stage { down, blocks })
    }

    fn forward(&self, x: Tensor) -> Tensor {
        let mut h = match &self.down {
            Some(d) => d.forward(&x),
            None => x,
        };
        for b in &self.blocks {
            h = b.forward(&h);
        }
        h
    }
}

/// 整个骨干。
#[derive(Debug, Clone)]
pub struct Backbone {
    stem1: ConvBn,
    stem2a: ConvBn,
    stem2b: ConvBn,
    stem3: ConvBn,
    stem4: ConvBn,
    stages: Vec<Stage>,
}

impl Backbone {
    /// `p` 是权重名前缀（`model.backbone.model`）。
    pub fn load(w: &Weights, p: &str) -> Result<Backbone, String> {
        // [协议] `HGNetV2Config` 的 `stem_strides` 缺省 (2, 1, 1, 2, 1)、激活 ReLU
        let s = |name: &str, stride: usize| {
            conv_layer(
                w,
                &format!("{p}.embedder.{name}"),
                stride,
                false,
                Activation::Relu,
            )
        };
        let n = count(w, |i| {
            format!("{p}.encoder.stages.{i}.blocks.0.aggregation.0.convolution.weight")
        });
        if n == 0 {
            return Err(format!("{p} has no stages"));
        }
        let stages = (0..n)
            .map(|i| Stage::load(w, &format!("{p}.encoder.stages.{i}")))
            .collect::<Result<_, _>>()?;
        Ok(Backbone {
            stem1: s("stem1", 2)?,
            stem2a: s("stem2a", 1)?,
            stem2b: s("stem2b", 1)?,
            stem3: s("stem3", 2)?,
            stem4: s("stem4", 1)?,
            stages,
        })
    }

    /// 输入 `[3, H, W]`（已归一化），返回各段的输出（stage1 到最后一段）。
    pub fn forward(&self, x: &Tensor) -> Vec<Tensor> {
        let e = ops::pad_right_bottom(&self.stem1.forward(x), 1, 1);
        let a = ops::pad_right_bottom(&self.stem2a.forward(&e), 1, 1);
        let a = self.stem2b.forward(&a);
        let pooled = ops::max_pool2d(&e, (2, 2), (1, 1), true);
        let cat = ops::concat_channels(&[&pooled, &a]);
        let mut h = self.stem4.forward(&self.stem3.forward(&cat));
        let mut outs = Vec::with_capacity(self.stages.len());
        for st in &self.stages {
            h = st.forward(h);
            outs.push(h.clone());
        }
        outs
    }
}
