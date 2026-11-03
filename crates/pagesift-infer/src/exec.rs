//! 执行器：模型的前向写成对 [`Exec`] 泛型，同一份代码既能在 CPU 上算，也能在显存里连续算、拼批算。
//!
//! 为什么不只靠 [`crate::ops`] 分流（见 [`crate::backend`]）：`Tensor.data` 是公开的 `Vec<f32>`，
//! 模型可以随时直接读写，所以经过 `ops` 的每个结果都必须回到内存，每层来回拷一次；而且 `[C, H, W]` 的
//! 接口也表达不了“一批”。[`Exec::T`] 是不透明的句柄，张量可以留在显存，批次维度也藏在句柄里，
//! 模型代码看到的仍是单个样本的形状。
//!
//! [`Cpu`] 单态化之后就是原来对 `ops` 的直接调用，没有额外开销。

use crate::{Activation, Conv2d, ConvTranspose2d, LayerNorm, Linear, Tensor, ops};

/// 模型前向要用的全部算子，语义同 [`crate::ops`] 和 `nn` 里的同名层。
/// 形状都指单个样本：图像特征 `[C, H, W]`，序列 `[T, C]`。
pub trait Exec {
    /// 张量句柄。
    type T;

    /// 单个样本的形状。
    fn shape(&self, x: &Self::T) -> Vec<usize>;
    fn conv(&self, l: &Conv2d, x: &Self::T) -> Self::T;
    fn conv_transpose(&self, l: &ConvTranspose2d, x: &Self::T) -> Self::T;
    fn act(&self, a: Activation, x: &mut Self::T);
    fn add(&self, a: &Self::T, b: &Self::T) -> Self::T;
    fn add_assign(&self, a: &mut Self::T, b: &Self::T);
    fn pad_right_bottom(&self, x: &Self::T, right: usize, bottom: usize) -> Self::T;
    fn max_pool2d(
        &self,
        x: &Self::T,
        k: (usize, usize),
        stride: (usize, usize),
        ceil_mode: bool,
    ) -> Self::T;
    fn avg_pool2d(&self, x: &Self::T, k: (usize, usize)) -> Self::T;
    /// 每个通道的全局平均，结果 `[C, 1, 1]`。
    fn global_avg(&self, x: &Self::T) -> Self::T;
    /// `s` 是 `[C, 1, 1]`，逐通道乘到 `x` 上。
    fn scale_channels(&self, x: &mut Self::T, s: &Self::T);
    /// 颈部 SE 的门：`v ← 1 + clamp(0.2·v + 0.5, 0, 1)`。
    fn se_gate(&self, s: &mut Self::T);
    fn concat(&self, xs: &[&Self::T]) -> Self::T;
    /// 最近邻插值到 `oh × ow`。
    fn upsample(&self, x: &Self::T, oh: usize, ow: usize) -> Self::T;
    fn chw_to_seq(&self, x: &Self::T) -> Self::T;
    fn linear(&self, l: &Linear, x: &Self::T) -> Self::T;
    fn layer_norm(&self, l: &LayerNorm, x: &Self::T) -> Self::T;
    /// 多头自注意力的中段：输入线性层出来的 `[T, 3C]`（列 = s·C + h·hd + d，s 为 0 = q、1 = k、2 = v），
    /// 返回拼回的 `[T, C]`，分数按 `head_dim^−0.5` 缩放。
    fn attention(&self, qkv: &Self::T, heads: usize) -> Self::T;
    /// CTC 摘要：每个样本一个列表，每步一个（最大类下标，softmax 的最大概率）。
    fn ctc_digest(&self, logits: &Self::T) -> Vec<Vec<(u32, f32)>>;
}

/// [协议] 注意力分数的缩放 `head_dim^−0.5`（Transformer 的 scaled dot-product）。
pub fn attention_scale(head_dim: usize) -> f32 {
    (head_dim as f32).powf(-0.5)
}

/// CPU 执行器。
#[derive(Debug, Clone, Copy, Default)]
pub struct Cpu;

impl Exec for Cpu {
    type T = Tensor;

    fn shape(&self, x: &Tensor) -> Vec<usize> {
        x.shape.clone()
    }

    fn conv(&self, l: &Conv2d, x: &Tensor) -> Tensor {
        l.forward(x)
    }

    fn conv_transpose(&self, l: &ConvTranspose2d, x: &Tensor) -> Tensor {
        l.forward(x)
    }

    fn act(&self, a: Activation, x: &mut Tensor) {
        a.apply_all(x);
    }

    fn add(&self, a: &Tensor, b: &Tensor) -> Tensor {
        ops::add(a, b)
    }

    fn add_assign(&self, a: &mut Tensor, b: &Tensor) {
        assert_eq!(a.shape, b.shape, "相加的形状不同");
        for (a, b) in a.data.iter_mut().zip(&b.data) {
            *a += b;
        }
    }

    fn pad_right_bottom(&self, x: &Tensor, right: usize, bottom: usize) -> Tensor {
        ops::pad_right_bottom(x, right, bottom)
    }

    fn max_pool2d(
        &self,
        x: &Tensor,
        k: (usize, usize),
        stride: (usize, usize),
        ceil_mode: bool,
    ) -> Tensor {
        ops::max_pool2d(x, k, stride, ceil_mode)
    }

    fn avg_pool2d(&self, x: &Tensor, k: (usize, usize)) -> Tensor {
        ops::avg_pool2d(x, k)
    }

    fn global_avg(&self, x: &Tensor) -> Tensor {
        Tensor::new(vec![x.shape[0], 1, 1], ops::global_avg_pool(x))
    }

    fn scale_channels(&self, x: &mut Tensor, s: &Tensor) {
        ops::scale_channels(x, &s.data);
    }

    fn se_gate(&self, s: &mut Tensor) {
        for v in &mut s.data {
            *v = se_gate(*v);
        }
    }

    fn concat(&self, xs: &[&Tensor]) -> Tensor {
        ops::concat_channels(xs)
    }

    fn upsample(&self, x: &Tensor, oh: usize, ow: usize) -> Tensor {
        ops::interpolate_nearest(x, oh, ow)
    }

    fn chw_to_seq(&self, x: &Tensor) -> Tensor {
        ops::chw_to_seq(x)
    }

    fn linear(&self, l: &Linear, x: &Tensor) -> Tensor {
        l.forward(x)
    }

    fn layer_norm(&self, l: &LayerNorm, x: &Tensor) -> Tensor {
        l.forward(x)
    }

    fn attention(&self, qkv: &Tensor, heads: usize) -> Tensor {
        let (t, c3) = qkv.tc();
        let c = c3 / 3; // [协议] q、k、v 三份
        let hd = c / heads;
        let scale = attention_scale(hd);
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
        for h in 0..heads {
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
        Tensor::new(vec![t, c], out)
    }

    fn ctc_digest(&self, logits: &Tensor) -> Vec<Vec<(u32, f32)>> {
        let (_, n) = logits.tc();
        vec![
            logits
                .data
                .chunks(n)
                .map(|row| {
                    let (mut arg, mut m) = (0u32, f32::NEG_INFINITY);
                    for (i, &v) in row.iter().enumerate() {
                        if v > m {
                            (arg, m) = (i as u32, v);
                        }
                    }
                    // softmax 的最大概率 = 1 / Σ exp(xᵢ − max)，取最大的类不用整行归一化
                    let z: f32 = row.iter().map(|&v| (v - m).exp()).sum();
                    (arg, 1.0 / z)
                })
                .collect(),
        ]
    }
}

/// 颈部 SE 的门，一个值。
#[inline]
pub fn se_gate(v: f32) -> f32 {
    // [协议] 颈部 SE 用 Paddle 式硬 sigmoid：clamp(0.2·x + 0.5, 0, 1)（不是 PyTorch 的 x/6 + 1/2）；
    // 以 h + se(h) 的形式出现，等价于逐通道乘 1 + s
    1.0 + (0.2 * v + 0.5).clamp(0.0, 1.0)
}
