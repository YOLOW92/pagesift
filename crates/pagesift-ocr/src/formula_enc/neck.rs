//! ViT 之后的两段：
//!
//! - 颈部（`PPFormulaNetVisionNeck`，SAM 的 neck）：1×1 卷积 768→256（无偏置）→ 通道 LayerNorm →
//!   3×3 卷积 256→256（补 1，无偏置）→ 通道 LayerNorm；输出 `[256, 48, 48]`。
//! - 投影（`PPFormulaNetMultiModalProjector`）：3×3 步长 2 卷积 256→512 → 3×3 步长 2 卷积 512→1024
//!   （都补 1、无偏置、中间无激活）→ 展平成 `[144, 1024]` → Linear 1024→1024 → Linear 1024→512
//!   （中间无激活）。
//!
//! 通道 LayerNorm（`data_format="channels_first"`）就是对每个位置的 C 个通道做 LayerNorm，
//! 在 `[H·W, C]` 的序列上直接用普通 LayerNorm。

use pagesift_infer::{Conv2d, LayerNorm, Linear, Tensor, Weights, ops};

/// [协议] `PPFormulaNetLayerNorm` 的缺省 eps（颈部没有另给）
const NECK_EPS: f32 = 1e-6;

/// 颈部。
pub(crate) struct Neck {
    /// 1×1 卷积当作无偏置的全连接
    conv1: Linear,
    ln1: LayerNorm,
    conv2: Conv2d,
    ln2: LayerNorm,
}

impl Neck {
    pub(crate) fn load(w: &Weights, p: &str) -> Result<Neck, String> {
        let k = w.get(&format!("{p}.conv1.weight"))?;
        // [协议] 卷积权重是 [O, C, kh, kw] 四维
        if k.shape.len() != 4 || k.shape[2] != 1 || k.shape[3] != 1 {
            return Err(format!("{p}.conv1 不是 1×1 卷积：{:?}", k.shape));
        }
        let flat = Tensor::new(vec![k.shape[0], k.shape[1]], k.data.clone());
        Ok(Neck {
            conv1: Linear {
                weight_t: ops::transpose(&flat),
                bias: None,
            },
            ln1: LayerNorm::load(w, &format!("{p}.layer_norm1"), NECK_EPS)?,
            // [协议] kernel 3、padding 1、stride 1
            conv2: Conv2d::load(w, &format!("{p}.conv2"), (1, 1), (1, 1), (1, 1), 1)?,
            ln2: LayerNorm::load(w, &format!("{p}.layer_norm2"), NECK_EPS)?,
        })
    }

    /// `[grid², 768]` → `[256, grid, grid]`。
    pub(crate) fn forward(&self, x: &Tensor, grid: usize) -> Tensor {
        let s = self.ln1.forward(&self.conv1.forward(x));
        let y = self.conv2.forward(&ops::seq_to_chw(&s, grid, grid));
        let s = self.ln2.forward(&ops::chw_to_seq(&y));
        ops::seq_to_chw(&s, grid, grid)
    }
}

/// 投影到解码器的维度。
pub(crate) struct Projector {
    conv1: Conv2d,
    conv2: Conv2d,
    lin1: Linear,
    lin2: Linear,
}

impl Projector {
    pub(crate) fn load(w: &Weights, p: &str) -> Result<Projector, String> {
        // [协议] 两个卷积都是 kernel 3、stride 2、padding 1
        let conv = |n: &str| Conv2d::load(w, &format!("{p}.{n}"), (2, 2), (1, 1), (1, 1), 1);
        Ok(Projector {
            conv1: conv("conv1")?,
            conv2: conv("conv2")?,
            lin1: Linear::load(w, &format!("{p}.linear_1"))?,
            lin2: Linear::load(w, &format!("{p}.linear_2"))?,
        })
    }

    /// `[256, H, W]` → `[(H/4)·(W/4), 512]`（行优先展平）。
    pub(crate) fn forward(&self, x: &Tensor) -> Tensor {
        let y = self.conv2.forward(&self.conv1.forward(x));
        let s = ops::chw_to_seq(&y);
        self.lin2.forward(&self.lin1.forward(&s))
    }
}
