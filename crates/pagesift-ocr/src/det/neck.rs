//! 颈部 RepLKFPN 与 DB 头。结构照 transformers 的 `modeling_pp_ocrv6_small_det.py`
//! （Apache-2.0，PaddlePaddle 与 HuggingFace）：
//! - 颈部：每段 1×1 卷积 + SE 残差统一到 96 通道 → 自顶向下最近邻上采样相加 →
//!   每段大核深度卷积 + 逐点卷积（到 24 通道）+ SE 残差 → 都上采样到步长 4 → 按 p5、p4、p3、p2 拼接；
//! - 头：3×3 卷积 + BN + ReLU → 转置卷积 ×2 + BN + ReLU → 转置卷积 ×2 → sigmoid，得到原尺寸概率图。

use pagesift_infer::exec::{Cpu, Exec};
use pagesift_infer::{Activation, BN_EPS, BatchNorm, Conv2d, ConvTranspose2d, Tensor, Weights};

/// 颈部和头的配置（config.json 顶层）。
#[derive(Debug, Clone)]
pub struct NeckConfig {
    pub in_channels: Vec<usize>,
    pub out_channels: usize,
    pub kernel_size: usize,
}

/// 读配置里的 `kernel_list`（头的三个核大小）。
// [协议] kernel_list 固定三项：conv_down、conv_up、conv_final
pub fn head_kernels(v: &serde_json::Value) -> Result<[usize; 3], String> {
    let a = v["kernel_list"].as_array().ok_or("missing kernel_list")?;
    let k: Vec<usize> = a
        .iter()
        .map(|x| {
            x.as_u64()
                .map(|n| n as usize)
                .ok_or("kernel_list must be integers")
        })
        .collect::<Result<_, _>>()?;
    k.try_into()
        .map_err(|_| "kernel_list must have 3 numbers".into())
}

impl NeckConfig {
    pub fn from_json(v: &serde_json::Value) -> Result<NeckConfig, String> {
        let ints = |key: &str| -> Result<Vec<usize>, String> {
            v[key]
                .as_array()
                .ok_or(format!("missing {key}"))?
                .iter()
                .map(|x| {
                    x.as_u64()
                        .map(|n| n as usize)
                        .ok_or(format!("{key} must be an integer"))
                })
                .collect()
        };
        if v["interpolate_mode"].as_str().unwrap_or("nearest") != "nearest" {
            return Err("the neck only supports nearest-neighbor upsampling".into());
        }
        let in_channels = ints("layer_list_out_channels")?;
        if in_channels.len() != 4 {
            return Err("layer_list_out_channels must have 4 stages".into());
        }
        Ok(NeckConfig {
            in_channels,
            // [协议] neck_out_channels 缺省 96、dilated_kernel_size 缺省 7（PPOCRV6SmallDetConfig）
            out_channels: v["neck_out_channels"].as_u64().unwrap_or(96) as usize,
            kernel_size: v["dilated_kernel_size"].as_u64().unwrap_or(7) as usize,
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

    fn apply_on<E: Exec>(&self, e: &E, x: &mut E::T) {
        let pooled = e.global_avg(x);
        let mut s = e.conv(&self.c1, &pooled);
        e.act(Activation::Relu, &mut s);
        let mut s = e.conv(&self.c2, &s);
        e.se_gate(&mut s);
        e.scale_channels(x, &s);
    }
}

/// `insert_conv`：1×1 卷积（无偏置）+ SE 残差。
#[derive(Debug, Clone)]
struct InsertConv {
    conv: Conv2d,
    se: SeResidual,
}

impl InsertConv {
    fn forward_on<E: Exec>(&self, e: &E, x: &E::T) -> E::T {
        let mut h = e.conv(&self.conv, x);
        self.se.apply_on(e, &mut h);
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
    fn forward_on<E: Exec>(&self, e: &E, x: &E::T) -> E::T {
        let mut h = e.conv(&self.pointwise, &e.conv(&self.depthwise, x));
        self.se.apply_on(e, &mut h);
        h
    }
}

/// 颈部。
#[derive(Debug, Clone)]
pub struct Neck {
    insert: Vec<InsertConv>,
    input: Vec<InputConv>,
}

/// 最近邻放大整数倍（`F.interpolate(scale_factor=s, mode="nearest")`），`s > 1`。
pub(super) fn upsample_on<E: Exec>(e: &E, x: &E::T, s: usize) -> E::T {
    let shape = e.shape(x);
    e.upsample(x, shape[1] * s, shape[2] * s)
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
        self.forward_on(&Cpu, feats)
    }

    /// 同 [`Neck::forward`]，在执行器上算。
    pub fn forward_on<E: Exec>(&self, e: &E, feats: &[E::T]) -> Result<E::T, String> {
        if feats.len() != self.insert.len() {
            return Err(format!(
                "the backbone gave {} feature stages, the neck needs {}",
                feats.len(),
                self.insert.len()
            ));
        }
        let mut fused: Vec<E::T> = self
            .insert
            .iter()
            .zip(feats)
            .map(|(m, f)| m.forward_on(e, f))
            .collect();
        // 自顶向下：p4 += up(p5)，p3 += up(p4)，p2 += up(p3)
        for i in (0..fused.len() - 1).rev() {
            let up = upsample_on(e, &fused[i + 1], 2);
            if e.shape(&up) != e.shape(&fused[i]) {
                return Err(format!(
                    "neck upsampling size mismatch: {:?} and {:?}",
                    e.shape(&up),
                    e.shape(&fused[i])
                ));
            }
            e.add_assign(&mut fused[i], &up);
        }
        let processed: Vec<E::T> = self
            .input
            .iter()
            .zip(&fused)
            .enumerate()
            .map(|(i, (m, f))| {
                let h = m.forward_on(e, f);
                if i == 0 {
                    h
                } else {
                    upsample_on(e, &h, 1 << i)
                }
            })
            .collect();
        // [协议] 按 [p5, p4, p3, p2] 的顺序拼通道
        let refs: Vec<&E::T> = processed.iter().rev().collect();
        Ok(e.concat(&refs))
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
    /// `p` 是权重名前缀（`head`），`k` 是配置里的 `kernel_list`。
    // [协议] kernel_list 固定三项，见 head_kernels
    pub fn load(w: &Weights, p: &str, k: [usize; 3]) -> Result<Head, String> {
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
                return Err(format!(
                    "{name} has kernel size {got}, the config says {want}"
                ));
            }
        }
        Ok(Head { down, up, fin })
    }

    /// 返回 `[1, H, W]` 的概率图（已过 sigmoid）。
    pub fn forward(&self, x: &Tensor) -> Tensor {
        self.forward_on(&Cpu, x)
    }

    /// 同 [`Head::forward`]，在执行器上算。
    pub fn forward_on<E: Exec>(&self, e: &E, x: &E::T) -> E::T {
        let mut h = e.conv(&self.down, x);
        e.act(Activation::Relu, &mut h);
        let mut h = e.conv_transpose(&self.up, &h);
        e.act(Activation::Relu, &mut h);
        let mut h = e.conv_transpose(&self.fin, &h);
        e.act(Activation::Sigmoid, &mut h);
        h
    }
}
