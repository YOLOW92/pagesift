//! 纯 Rust 的小型神经网络推理：张量、卷积与 Transformer 层、safetensors 权重读取。
//! 给 OCR（PP-OCRv6 检测与识别）用，只做前向、f32。接口上批大小是 1；拼批和 GPU 经 [`exec`] 与
//! [`backend`] 两个接缝接入，见各自的说明。
//!
//! 约定：图像特征是 `[C, H, W]`（PyTorch 的 NCHW 去掉 N），序列是 `[T, C]`。各算子的语义与
//! PyTorch 同名算子一致（`torch.nn.functional`），实现上可以换更快的算法，但结果只允许浮点舍入差。

pub mod backend;
pub mod exec;
mod fastmath;
mod nn;
pub mod ops;
mod par;
mod safetensors;

pub use nn::{Activation, BN_EPS, BatchNorm, Conv2d, ConvTranspose2d, LayerNorm, Linear, erf};

pub use safetensors::Weights;

/// 一个 f32 张量，按行优先连续存放。
#[derive(Debug, Clone, PartialEq)]
pub struct Tensor {
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

impl Tensor {
    pub fn new(shape: Vec<usize>, data: Vec<f32>) -> Tensor {
        assert_eq!(
            shape.iter().product::<usize>(),
            data.len(),
            "形状与数据长度不符"
        );
        Tensor { shape, data }
    }

    pub fn zeros(shape: Vec<usize>) -> Tensor {
        let n = shape.iter().product();
        Tensor {
            shape,
            data: vec![0.0; n],
        }
    }

    /// `[C, H, W]` 的三个维度。
    pub fn chw(&self) -> (usize, usize, usize) {
        assert_eq!(self.shape.len(), 3, "需要 [C, H, W]，实际 {:?}", self.shape);
        (self.shape[0], self.shape[1], self.shape[2])
    }

    /// `[T, C]` 的两个维度。
    pub fn tc(&self) -> (usize, usize) {
        assert_eq!(self.shape.len(), 2, "需要 [T, C]，实际 {:?}", self.shape);
        (self.shape[0], self.shape[1])
    }

    pub fn reshape(mut self, shape: Vec<usize>) -> Tensor {
        assert_eq!(
            shape.iter().product::<usize>(),
            self.data.len(),
            "reshape 元素数不符"
        );
        self.shape = shape;
        self
    }
}
