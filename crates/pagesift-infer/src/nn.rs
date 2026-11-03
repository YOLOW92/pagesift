//! 带权重的层（语义同 `torch.nn` 的同名模块），从 [`Weights`] 按前缀取参数。

use crate::{Tensor, Weights, backend, fastmath, ops, par};

/// 激活函数（语义同 transformers 的 `ACT2FN` 与 `torch.nn` 的同名模块）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    Identity,
    Relu,
    /// x · σ(x)
    Silu,
    /// 精确的 GELU：x · Φ(x) = x/2 · (1 + erf(x/√2))（`nn.GELU()` 的缺省）
    Gelu,
    /// `nn.Hardsigmoid`：relu6(x + 3) / 6
    HardSigmoid,
    Sigmoid,
}

impl Activation {
    #[inline]
    pub fn apply(self, x: f32) -> f32 {
        match self {
            Activation::Identity => x,
            Activation::Relu => x.max(0.0),
            Activation::Silu => fastmath::silu(x),
            Activation::Gelu => fastmath::gelu(x),
            // [协议] PyTorch Hardsigmoid 的定义：0（x ≤ −3）、1（x ≥ 3）、x/6 + 1/2
            Activation::HardSigmoid => ((x + 3.0) / 6.0).clamp(0.0, 1.0),
            Activation::Sigmoid => fastmath::sigmoid(x),
        }
    }

    /// 对每个元素做 [`apply`](Self::apply)。大张量切段多线程，结果与逐个调用逐位相同。
    pub fn apply_all(self, t: &mut Tensor) {
        if self == Activation::Identity {
            return;
        }
        if backend::current().is_some_and(|b| b.activate(&mut t.data, self)) {
            return;
        }
        // [推导] 每个元素折合的乘加次数（实测）：含 exp 的（GELU 里的 erf 也是）SSE2 下约 2.5 ns 一个，
        // 单核矩阵乘约 0.02 ns 一次乘加，折合约 100 次；ReLU 这类只是一次比较，算 1 次
        let cost = match self {
            Activation::Silu | Activation::Gelu | Activation::Sigmoid => 100,
            _ => 1,
        };
        let n = t.data.len();
        let threads = par::threads_for(n * cost, usize::MAX);
        par::for_chunks_mut(&mut t.data, n.div_ceil(threads), threads, |_, s| {
            self.apply_slice(s)
        });
    }

    /// 对一段切片逐个做 [`apply`](Self::apply)。分支放在循环外，每个循环体都能自动向量化。
    fn apply_slice(self, s: &mut [f32]) {
        // 泛型函数按每个 f 单独实例化，f 一定内联进循环
        fn each(s: &mut [f32], f: impl Fn(f32) -> f32) {
            for v in s {
                *v = f(*v);
            }
        }
        match self {
            Activation::Identity => {}
            Activation::Relu => each(s, |x| Activation::Relu.apply(x)),
            Activation::Silu => each(s, fastmath::silu),
            Activation::Gelu => each(s, fastmath::gelu),
            Activation::HardSigmoid => each(s, |x| Activation::HardSigmoid.apply(x)),
            Activation::Sigmoid => each(s, fastmath::sigmoid),
        }
    }
}

/// 误差函数。Abramowitz & Stegun《Handbook of Mathematical Functions》式 7.1.26，
/// 绝对误差不超过 1.5 × 10⁻⁷，低于 f32 的分辨率。
pub fn erf(x: f32) -> f32 {
    // [推导] 式 7.1.26 的系数 p、a1–a5
    const P: f64 = 0.327_591_1;
    const A: [f64; 5] = [
        0.254_829_592,
        -0.284_496_736,
        1.421_413_741,
        -1.453_152_027,
        1.061_405_429,
    ];
    let s = x.signum() as f64;
    let x = (x as f64).abs();
    let t = 1.0 / (1.0 + P * x);
    let poly = t * (A[0] + t * (A[1] + t * (A[2] + t * (A[3] + t * A[4]))));
    (s * (1.0 - poly * (-x * x).exp())) as f32
}

/// 二维卷积层（`nn.Conv2d`）。
#[derive(Debug, Clone)]
pub struct Conv2d {
    pub weight: Tensor,
    pub bias: Option<Vec<f32>>,
    pub stride: (usize, usize),
    pub pad: (usize, usize),
    pub dilation: (usize, usize),
    pub groups: usize,
}

impl Conv2d {
    /// 读 `{prefix}.weight` 和（有的话）`{prefix}.bias`。
    pub fn load(
        w: &Weights,
        prefix: &str,
        stride: (usize, usize),
        pad: (usize, usize),
        dilation: (usize, usize),
        groups: usize,
    ) -> Result<Conv2d, String> {
        Ok(Conv2d {
            weight: w.get(&format!("{prefix}.weight"))?.clone(),
            bias: w.opt(&format!("{prefix}.bias")).map(|t| t.data.clone()),
            stride,
            pad,
            dilation,
            groups,
        })
    }

    pub fn forward(&self, x: &Tensor) -> Tensor {
        ops::conv2d(
            x,
            &self.weight,
            self.bias.as_deref(),
            self.stride,
            self.pad,
            self.dilation,
            self.groups,
        )
    }

    /// 把其后的批归一化并进卷积的权重和偏置（推理时 BN 是逐通道的仿射变换）。
    pub fn fold(&mut self, bn: &BatchNorm) {
        let o = self.weight.shape[0];
        let per = self.weight.data.len() / o;
        let mut bias = self.bias.take().unwrap_or_else(|| vec![0.0; o]);
        for (oc, (w, b)) in self.weight.data.chunks_mut(per).zip(&mut bias).enumerate() {
            for v in w {
                *v *= bn.scale[oc];
            }
            *b = *b * bn.scale[oc] + bn.shift[oc];
        }
        self.bias = Some(bias);
    }
}

/// 转置卷积层（`nn.ConvTranspose2d`，groups = 1）。
#[derive(Debug, Clone)]
pub struct ConvTranspose2d {
    pub weight: Tensor,
    pub bias: Option<Vec<f32>>,
    pub stride: (usize, usize),
    pub pad: (usize, usize),
}

impl ConvTranspose2d {
    pub fn load(
        w: &Weights,
        prefix: &str,
        stride: (usize, usize),
        pad: (usize, usize),
    ) -> Result<Self, String> {
        Ok(ConvTranspose2d {
            weight: w.get(&format!("{prefix}.weight"))?.clone(),
            bias: w.opt(&format!("{prefix}.bias")).map(|t| t.data.clone()),
            stride,
            pad,
        })
    }

    pub fn forward(&self, x: &Tensor) -> Tensor {
        ops::conv_transpose2d(x, &self.weight, self.bias.as_deref(), self.stride, self.pad)
    }

    /// 并进其后的批归一化（转置卷积的输出通道是权重的第 1 维）。
    pub fn fold(&mut self, bn: &BatchNorm) {
        let (c, o) = (self.weight.shape[0], self.weight.shape[1]);
        let per = self.weight.data.len() / (c * o);
        for ic in 0..c {
            for oc in 0..o {
                let at = (ic * o + oc) * per;
                for v in &mut self.weight.data[at..at + per] {
                    *v *= bn.scale[oc];
                }
            }
        }
        let mut bias = self.bias.take().unwrap_or_else(|| vec![0.0; o]);
        for (b, (s, t)) in bias.iter_mut().zip(bn.scale.iter().zip(&bn.shift)) {
            *b = *b * s + t;
        }
        self.bias = Some(bias);
    }
}

/// 推理时的批归一化（`nn.BatchNorm2d` 的 eval 模式），化成逐通道 y = scale·x + shift。
#[derive(Debug, Clone)]
pub struct BatchNorm {
    pub scale: Vec<f32>,
    pub shift: Vec<f32>,
}

/// [协议] `nn.BatchNorm2d` 的缺省 eps
pub const BN_EPS: f32 = 1e-5;

impl BatchNorm {
    /// 读 `{prefix}.weight/bias/running_mean/running_var`。
    pub fn load(w: &Weights, prefix: &str, eps: f32) -> Result<BatchNorm, String> {
        let g = &w.get(&format!("{prefix}.weight"))?.data;
        let b = &w.get(&format!("{prefix}.bias"))?.data;
        let m = &w.get(&format!("{prefix}.running_mean"))?.data;
        let v = &w.get(&format!("{prefix}.running_var"))?.data;
        let scale: Vec<f32> = g.iter().zip(v).map(|(g, v)| g / (v + eps).sqrt()).collect();
        let shift = b
            .iter()
            .zip(m)
            .zip(&scale)
            .map(|((b, m), s)| b - m * s)
            .collect();
        Ok(BatchNorm { scale, shift })
    }

    pub fn forward(&self, x: &mut Tensor) {
        let (c, h, w) = x.chw();
        for ch in 0..c {
            for v in &mut x.data[ch * h * w..(ch + 1) * h * w] {
                *v = *v * self.scale[ch] + self.shift[ch];
            }
        }
    }
}

/// 全连接层（`nn.Linear`），作用在 `[T, in]` 上。
#[derive(Debug, Clone)]
pub struct Linear {
    /// `[in, out]`：载入时把 PyTorch 的 `[out, in]` 转置好，前向就是一次矩阵乘。
    pub weight_t: Tensor,
    pub bias: Option<Vec<f32>>,
}

impl Linear {
    pub fn load(w: &Weights, prefix: &str) -> Result<Linear, String> {
        Ok(Linear {
            weight_t: ops::transpose(w.get(&format!("{prefix}.weight"))?),
            bias: w.opt(&format!("{prefix}.bias")).map(|t| t.data.clone()),
        })
    }

    pub fn forward(&self, x: &Tensor) -> Tensor {
        let mut y = ops::matmul_with(x, &self.weight_t, true);
        if let Some(b) = &self.bias {
            for row in y.data.chunks_mut(b.len()) {
                for (v, b) in row.iter_mut().zip(b) {
                    *v += b;
                }
            }
        }
        y
    }
}

/// 层归一化（`nn.LayerNorm`），作用在 `[T, C]` 的最后一维。
#[derive(Debug, Clone)]
pub struct LayerNorm {
    pub weight: Vec<f32>,
    pub bias: Vec<f32>,
    pub eps: f32,
}

impl LayerNorm {
    pub fn load(w: &Weights, prefix: &str, eps: f32) -> Result<LayerNorm, String> {
        Ok(LayerNorm {
            weight: w.get(&format!("{prefix}.weight"))?.data.clone(),
            bias: w.get(&format!("{prefix}.bias"))?.data.clone(),
            eps,
        })
    }

    pub fn forward(&self, x: &Tensor) -> Tensor {
        let (_, c) = x.tc();
        let mut out = x.clone();
        for row in out.data.chunks_mut(c) {
            let mean = row.iter().sum::<f32>() / c as f32;
            let var = row.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / c as f32;
            let inv = 1.0 / (var + self.eps).sqrt();
            for (i, v) in row.iter_mut().enumerate() {
                *v = (*v - mean) * inv * self.weight[i] + self.bias[i];
            }
        }
        out
    }
}
