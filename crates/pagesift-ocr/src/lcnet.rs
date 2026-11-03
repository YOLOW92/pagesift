//! PP-LCNetV4 骨干（检测、识别共用）。结构照 transformers 的 `modeling_pp_lcnet_v4.py`
//! （Apache-2.0，PaddlePaddle 与 HuggingFace）：大 stem + 4 段深度可分离卷积块。
//! 批归一化在载入时并进卷积。

use pagesift_infer::exec::{Cpu, Exec};
use pagesift_infer::{Activation, BN_EPS, BatchNorm, Conv2d, Tensor, Weights};

/// 每个块的配置：(核大小, 输入通道, 输出通道, 步长 (行, 列), 是否用 SE)，对应 config.json 的 `block_configs`。
pub type BlockConfig = (usize, usize, usize, (usize, usize), bool);

/// 骨干的配置（config.json 里的 `backbone_config`）。
#[derive(Debug, Clone)]
pub struct Config {
    pub stem_channels: [usize; 3],
    pub blocks: Vec<Vec<BlockConfig>>,
}

impl Config {
    /// 从 config.json 的 `backbone_config` 读。只支持 `stem_type = "large"`、缩放 1.0、
    /// 不用可学习仿射块（PP-OCRv6 的检测、识别都是这样）。
    pub fn from_json(v: &serde_json::Value) -> Result<Config, String> {
        let b = &v["backbone_config"];
        if b["stem_type"].as_str().unwrap_or("large") != "large" {
            return Err("只支持 large stem".into());
        }
        if b["use_learnable_affine_block"].as_bool().unwrap_or(false) {
            return Err("不支持可学习仿射块".into());
        }
        let int = |x: &serde_json::Value| x.as_u64().map(|v| v as usize).ok_or("配置里应为整数");
        let sc = b["stem_channels"].as_array().ok_or("缺 stem_channels")?;
        let stem_channels = [int(&sc[0])?, int(&sc[1])?, int(&sc[2])?];
        let mut blocks = Vec::new();
        for stage in b["block_configs"].as_array().ok_or("缺 block_configs")? {
            let mut s = Vec::new();
            for blk in stage.as_array().ok_or("block_configs 格式不对")? {
                let a = blk.as_array().ok_or("block_configs 格式不对")?;
                // 步长可以是一个数，也可以是 [行, 列]
                let stride = match a[3].as_array() {
                    Some(p) => (int(&p[0])?, int(&p[1])?),
                    None => (int(&a[3])?, int(&a[3])?),
                };
                s.push((
                    int(&a[0])?,
                    int(&a[1])?,
                    int(&a[2])?,
                    stride,
                    a[4].as_bool().unwrap_or(false),
                ));
            }
            blocks.push(s);
        }
        Ok(Config {
            stem_channels,
            blocks,
        })
    }
}

/// 卷积 + 批归一化（已并入）+ 激活。
#[derive(Debug, Clone)]
pub struct ConvBn {
    pub conv: Conv2d,
    pub act: Activation,
}

impl ConvBn {
    /// 读 `{p}.convolution` 和 `{p}.normalization`；补边按 PyTorch 的 `(k − 1) / 2`。
    pub fn load(
        w: &Weights,
        p: &str,
        stride: (usize, usize),
        groups: usize,
        act: Activation,
    ) -> Result<ConvBn, String> {
        let k = w.get(&format!("{p}.convolution.weight"))?.shape[2];
        let pad = (k - 1) / 2;
        let mut conv = Conv2d::load(
            w,
            &format!("{p}.convolution"),
            stride,
            (pad, pad),
            (1, 1),
            groups,
        )?;
        conv.fold(&BatchNorm::load(w, &format!("{p}.normalization"), BN_EPS)?);
        Ok(ConvBn { conv, act })
    }

    pub fn forward(&self, x: &Tensor) -> Tensor {
        self.forward_on(&Cpu, x)
    }

    pub fn forward_on<E: Exec>(&self, e: &E, x: &E::T) -> E::T {
        let mut y = e.conv(&self.conv, x);
        e.act(self.act, &mut y);
        y
    }
}

/// SE 注意力：全局平均 → 1×1 卷积 → ReLU → 1×1 卷积 → Hardsigmoid → 逐通道相乘。
#[derive(Debug, Clone)]
struct Se {
    c1: Conv2d,
    c2: Conv2d,
}

impl Se {
    fn load(w: &Weights, p: &str) -> Result<Se, String> {
        // [协议] ModuleList 里卷积和激活交替放，卷积在下标 0、2
        let c = |i: usize| {
            Conv2d::load(
                w,
                &format!("{p}.convolutions.{i}"),
                (1, 1),
                (0, 0),
                (1, 1),
                1,
            )
        };
        Ok(Se {
            c1: c(0)?,
            c2: c(2)?,
        })
    }

    fn forward_on<E: Exec>(&self, e: &E, x: &mut E::T) {
        let pooled = e.global_avg(x);
        let mut s = e.conv(&self.c1, &pooled);
        e.act(Activation::Relu, &mut s);
        let mut s = e.conv(&self.c2, &s);
        e.act(Activation::HardSigmoid, &mut s);
        e.scale_channels(x, &s);
    }
}

/// 深度可分离卷积块。
#[derive(Debug, Clone)]
struct Block {
    token: Conv2d,
    se: Option<Se>,
    ch1: ConvBn,
    ch2: ConvBn,
    residual: bool,
}

impl Block {
    fn load(w: &Weights, p: &str, cfg: &BlockConfig) -> Result<Block, String> {
        let &(k, cin, cout, stride, use_se) = cfg;
        let same = cin == cout && stride == (1, 1);
        // 步长 1、通道不变时 token_conv 是带偏置的普通深度卷积（重参数化后的形式）；
        // 否则是卷积 + 批归一化、不加激活
        let token = if same {
            Conv2d::load(
                w,
                &format!("{p}.token_conv"),
                stride,
                (k / 2, k / 2),
                (1, 1),
                cin,
            )?
        } else {
            ConvBn::load(
                w,
                &format!("{p}.token_conv"),
                stride,
                cin,
                Activation::Identity,
            )?
            .conv
        };
        let se = if use_se {
            Some(Se::load(w, &format!("{p}.token_squeeze_excitation"))?)
        } else {
            None
        };
        Ok(Block {
            token,
            se,
            ch1: ConvBn::load(
                w,
                &format!("{p}.channel_conv1"),
                (1, 1),
                1,
                Activation::Gelu,
            )?,
            ch2: ConvBn::load(
                w,
                &format!("{p}.channel_conv2"),
                (1, 1),
                1,
                Activation::Identity,
            )?,
            residual: same,
        })
    }

    fn forward_on<E: Exec>(&self, e: &E, x: &E::T) -> E::T {
        let mut t = e.conv(&self.token, x);
        if let Some(se) = &self.se {
            se.forward_on(e, &mut t);
        }
        let y = self.ch2.forward_on(e, &self.ch1.forward_on(e, &t));
        if self.residual { e.add(&t, &y) } else { y }
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
    stages: Vec<Vec<Block>>,
}

impl Backbone {
    /// `p` 是权重名前缀（PP-OCRv6 里是 `model.backbone.encoder`）。
    pub fn load(w: &Weights, p: &str, cfg: &Config) -> Result<Backbone, String> {
        // [协议] stem 的步长 (2, 1, 1, 2, 1)、激活 ReLU（PPLCNetV4Config 的缺省）
        let s = |name: &str, stride: usize| {
            ConvBn::load(
                w,
                &format!("{p}.convolution.{name}"),
                (stride, stride),
                1,
                Activation::Relu,
            )
        };
        let mut stages = Vec::new();
        for (i, stage) in cfg.blocks.iter().enumerate() {
            let mut v = Vec::new();
            for (j, b) in stage.iter().enumerate() {
                v.push(Block::load(w, &format!("{p}.blocks.{i}.blocks.{j}"), b)?);
            }
            stages.push(v);
        }
        Ok(Backbone {
            stem1: s("stem1", 2)?,
            stem2a: s("stem2a", 1)?,
            stem2b: s("stem2b", 1)?,
            stem3: s("stem3", 2)?,
            stem4: s("stem4", 1)?,
            stages,
        })
    }

    /// 输入 `[3, H, W]`（已归一化），返回 4 段各自的输出（stage1–stage4）。
    pub fn forward(&self, x: &Tensor) -> Vec<Tensor> {
        self.forward_on(&Cpu, x)
    }

    /// 同 [`Backbone::forward`]，在执行器 `ex` 上算。
    pub fn forward_on<E: Exec>(&self, ex: &E, x: &E::T) -> Vec<E::T> {
        let stem = self.stem_on(ex, x);
        let mut outs: Vec<E::T> = Vec::with_capacity(self.stages.len());
        for stage in &self.stages {
            let h = self.stage_on(ex, stage, outs.last().unwrap_or(&stem));
            outs.push(h);
        }
        outs
    }

    /// 只要最后一段的输出（识别用）。中间特征算完就释放，显存峰值低得多。
    pub fn forward_last_on<E: Exec>(&self, ex: &E, x: &E::T) -> E::T {
        let mut h = self.stem_on(ex, x);
        for stage in &self.stages {
            h = self.stage_on(ex, stage, &h);
        }
        h
    }

    fn stem_on<E: Exec>(&self, ex: &E, x: &E::T) -> E::T {
        let e = ex.pad_right_bottom(&self.stem1.forward_on(ex, x), 1, 1);
        let a = ex.pad_right_bottom(&self.stem2a.forward_on(ex, &e), 1, 1);
        let a = self.stem2b.forward_on(ex, &a);
        let pooled = ex.max_pool2d(&e, (2, 2), (1, 1), true);
        let cat = ex.concat(&[&pooled, &a]);
        self.stem4.forward_on(ex, &self.stem3.forward_on(ex, &cat))
    }

    fn stage_on<E: Exec>(&self, ex: &E, stage: &[Block], x: &E::T) -> E::T {
        let mut h = stage[0].forward_on(ex, x);
        for b in &stage[1..] {
            h = b.forward_on(ex, &h);
        }
        h
    }
}
