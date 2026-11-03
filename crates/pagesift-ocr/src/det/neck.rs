//! 颈部 RepLKFPN 与 DB 头。结构照 transformers 的 `modeling_pp_ocrv6_small_det.py`
//! （Apache-2.0，PaddlePaddle 与 HuggingFace）：
//! - 颈部：每段 1×1 卷积 + SE 残差统一到 96 通道 → 自顶向下最近邻上采样相加 →
//!   每段大核深度卷积 + 逐点卷积（到 24 通道）+ SE 残差 → 都上采样到步长 4 → 按 p5、p4、p3、p2 拼接；
//! - 头：3×3 卷积 + BN + ReLU → 转置卷积 ×2 + BN + ReLU → 转置卷积 ×2 → sigmoid，得到原尺寸概率图。

use pagesift_infer::{
    Activation, BN_EPS, BatchNorm, Conv2d, ConvTranspose2d, Tensor, Weights, ops,
};

/// 颈部和头的配置（config.json 顶层）。
#[derive(Debug, Clone)]
pub struct NeckConfig {
    pub in_channels: Vec<usize>,
    pub out_channels: usize,
    pub kernel_size: usize,
    pub head_kernels: [usize; 3],
}

impl NeckConfig {
    pub fn from_json(v: &serde_json::Value) -> Result<NeckConfig, String> {
        let ints = |key: &str| -> Result<Vec<usize>, String> {
            v[key]
                .as_array()
                .ok_or(format!("缺 {key}"))?
                .iter()
                .map(|x| {
                    x.as_u64()
                        .map(|n| n as usize)
                        .ok_or(format!("{key} 应为整数"))
                })
                .collect()
        };
        if v["interpolate_mode"].as_str().unwrap_or("nearest") != "nearest" {
            return Err("颈部只支持最近邻上采样".into());
        }
        let in_channels = ints("layer_list_out_channels")?;
        if in_channels.len() != 4 {
            return Err("layer_list_out_channels 应有 4 段".into());
        }
        let hk = ints("kernel_list")?;
        if hk.len() != 3 {
            return Err("kernel_list 应有 3 个数".into());
        }
        Ok(NeckConfig {
            in_channels,
            // [协议] neck_out_channels 缺省 96、dilated_kernel_size 缺省 7（PPOCRV6SmallDetConfig）
            out_channels: v["neck_out_channels"].as_u64().unwrap_or(96) as usize,
            kernel_size: v["dilated_kernel_size"].as_u64().unwrap_or(7) as usize,
            head_kernels: [hk[0], hk[1], hk[2]],
        })
    }
}

/// 颈部的 SE：全局平均 → 1×1 → ReLU → 1×1 → clamp(0.2x + 0.5, 0, 1)。
/// 两处都以 `h + se(h)` 的形式出现，等价于逐通道乘 `1 + s`，这里直接原地乘。
#[derive(Debug, Clone)]
struct SeResidual {
    c1: Conv2d,
    c2: Conv2d,
}

impl SeResidual {
    fn load(w: &Weights, p: &str) -> Result<SeResidual, String> {
        let c = |n: &str| Conv2d::load(w, &format!("{p}.{n}"), (1, 1), (0, 0), (1, 1), 1);
        Ok(SeResidual {
            c1: c("conv1")?,
            c2: c("conv2")?,
        })
    }

    fn apply(&self, x: &mut Tensor) {
        let c = x.shape[0];
        let pooled = Tensor::new(vec![c, 1, 1], ops::global_avg_pool(x));
        let mut s = self.c1.forward(&pooled);
        Activation::Relu.apply_all(&mut s);
        let s = self.c2.forward(&s);
        // [协议] 颈部 SE 用 Paddle 式硬 sigmoid：clamp(0.2·x + 0.5, 0, 1)（不是 PyTorch 的 x/6 + 1/2）
        let k: Vec<f32> = s
            .data
            .iter()
            .map(|v| 1.0 + (0.2 * v + 0.5).clamp(0.0, 1.0))
            .collect();
        ops::scale_channels(x, &k);
    }
}

/// `insert_conv`：1×1 卷积（无偏置）+ SE 残差。
#[derive(Debug, Clone)]
struct InsertConv {
    conv: Conv2d,
    se: SeResidual,
}

impl InsertConv {
    fn forward(&self, x: &Tensor) -> Tensor {
        let mut h = self.conv.forward(x);
        self.se.apply(&mut h);
        h
    }
}

/// `input_conv`：大核深度卷积（带偏置）→ 逐点卷积到 1/4 通道 → SE 残差。
#[derive(Debug, Clone)]
struct InputConv {
    depthwise: Conv2d,
    pointwise: Conv2d,
    se: SeResidual,
}

impl InputConv {
    fn forward(&self, x: &Tensor) -> Tensor {
        let mut h = self.pointwise.forward(&self.depthwise.forward(x));
        self.se.apply(&mut h);
        h
    }
}

/// 颈部。
#[derive(Debug, Clone)]
pub struct Neck {
    insert: Vec<InsertConv>,
    input: Vec<InputConv>,
}

/// 最近邻放大整数倍（`F.interpolate(scale_factor=s, mode="nearest")`）。
fn upsample(x: &Tensor, s: usize) -> Tensor {
    if s == 1 {
        return x.clone();
    }
    let (_, h, w) = x.chw();
    ops::interpolate_nearest(x, h * s, w * s)
}

impl Neck {
    /// `p` 是权重名前缀（PP-OCRv6 里是 `model.neck`）。
    pub fn load(w: &Weights, p: &str, cfg: &NeckConfig) -> Result<Neck, String> {
        let k = cfg.kernel_size;
        let c = cfg.out_channels;
        let mut insert = Vec::new();
        let mut input = Vec::new();
        for i in 0..cfg.in_channels.len() {
            let q = format!("{p}.insert_conv.{i}");
            insert.push(InsertConv {
                conv: Conv2d::load(w, &format!("{q}.in_conv"), (1, 1), (0, 0), (1, 1), 1)?,
                se: SeResidual::load(w, &format!("{q}.squeeze_excitation_block"))?,
            });
            let q = format!("{p}.input_conv.{i}");
            input.push(InputConv {
                // [协议] 深度卷积补边 kernel_size // 2、groups = 输入通道、不带空洞
                depthwise: Conv2d::load(
                    w,
                    &format!("{q}.depthwise_convolution"),
                    (1, 1),
                    (k / 2, k / 2),
                    (1, 1),
                    c,
                )?,
                pointwise: Conv2d::load(
                    w,
                    &format!("{q}.pointwise_convolution"),
                    (1, 1),
                    (0, 0),
                    (1, 1),
                    1,
                )?,
                se: SeResidual::load(w, &format!("{q}.squeeze_excitation_module"))?,
            });
        }
        Ok(Neck { insert, input })
    }

    /// 输入骨干的 4 段特征（步长 4、8、16、32），输出步长 4 的融合特征。
    pub fn forward(&self, feats: &[Tensor]) -> Result<Tensor, String> {
        if feats.len() != self.insert.len() {
            return Err(format!(
                "骨干给了 {} 段特征，颈部要 {} 段",
                feats.len(),
                self.insert.len()
            ));
        }
        let mut fused: Vec<Tensor> = self
            .insert
            .iter()
            .zip(feats)
            .map(|(m, f)| m.forward(f))
            .collect();
        // 自顶向下：p4 += up(p5)，p3 += up(p4)，p2 += up(p3)
        for i in (0..fused.len() - 1).rev() {
            let up = upsample(&fused[i + 1], 2);
            if up.shape != fused[i].shape {
                return Err(format!(
                    "颈部上采样尺寸不符：{:?} 与 {:?}",
                    up.shape, fused[i].shape
                ));
            }
            for (a, b) in fused[i].data.iter_mut().zip(&up.data) {
                *a += b;
            }
        }
        let processed: Vec<Tensor> = self
            .input
            .iter()
            .zip(&fused)
            .enumerate()
            .map(|(i, (m, f))| upsample(&m.forward(f), 1 << i))
            .collect();
        // [协议] 按 [p5, p4, p3, p2] 的顺序拼通道
        let refs: Vec<&Tensor> = processed.iter().rev().collect();
        Ok(ops::concat_channels(&refs))
    }
}

/// DB 头：输出与网络输入同尺寸的文字概率图。
#[derive(Debug, Clone)]
pub struct Head {
    down: Conv2d,
    up: ConvTranspose2d,
    fin: ConvTranspose2d,
}

impl Head {
    /// `p` 是权重名前缀（`head`）。
    pub fn load(w: &Weights, p: &str, cfg: &NeckConfig) -> Result<Head, String> {
        let k = cfg.head_kernels;
        // [协议] conv_down 补边 kernel_list[0] // 2、无偏置；两个转置卷积步长 2、无补边
        let mut down = Conv2d::load(
            w,
            &format!("{p}.conv_down.convolution"),
            (1, 1),
            (k[0] / 2, k[0] / 2),
            (1, 1),
            1,
        )?;
        down.fold(&BatchNorm::load(w, &format!("{p}.conv_down.norm"), BN_EPS)?);
        let mut up = ConvTranspose2d::load(w, &format!("{p}.conv_up.convolution"), (2, 2), (0, 0))?;
        up.fold(&BatchNorm::load(w, &format!("{p}.conv_up.norm"), BN_EPS)?);
        let fin = ConvTranspose2d::load(w, &format!("{p}.conv_final"), (2, 2), (0, 0))?;
        for (name, got, want) in [
            ("conv_up", up.weight.shape[2], k[1]),
            ("conv_final", fin.weight.shape[2], k[2]),
        ] {
            if got != want {
                return Err(format!("{name} 的核大小 {got} 与配置 {want} 不符"));
            }
        }
        Ok(Head { down, up, fin })
    }

    /// 返回 `[1, H, W]` 的概率图（已过 sigmoid）。
    pub fn forward(&self, x: &Tensor) -> Tensor {
        let mut h = self.down.forward(x);
        Activation::Relu.apply_all(&mut h);
        let mut h = self.up.forward(&h);
        Activation::Relu.apply_all(&mut h);
        let mut h = self.fin.forward(&h);
        Activation::Sigmoid.apply_all(&mut h);
        h
    }
}
