//! 版面模型共用的层：卷积 + 批归一化、多层感知机、前馈层、多头自注意力、二维正弦位置编码、
//! 双线性缩放。语义照 transformers 的 `modeling_pp_doclayout_v3.py`（Apache-2.0）里对应的模块。

use pagesift_infer::{
    Activation, BN_EPS, BatchNorm, Conv2d, LayerNorm, Linear, Tensor, Weights, ops,
};

use crate::lcnet::ConvBn;

/// 读卷积 `conv` 和其后的批归一化 `norm`，把 BN 并进卷积。`depthwise` 时按输出通道分组。
pub(crate) fn conv_bn(
    w: &Weights,
    conv: &str,
    norm: &str,
    stride: usize,
    pad: usize,
    depthwise: bool,
    act: Activation,
) -> Result<ConvBn, String> {
    let mut c = Conv2d::load(w, conv, (stride, stride), (pad, pad), (1, 1), 1)?;
    if depthwise {
        c.groups = c.weight.shape[0];
    }
    c.fold(&BatchNorm::load(w, norm, BN_EPS)?);
    Ok(ConvBn { conv: c, act })
}

/// `{p}.convolution` + `{p}.normalization`（HGNetV2ConvLayer、PPDocLayoutV3ConvLayer），补边 (k − 1) / 2。
pub(crate) fn conv_layer(
    w: &Weights,
    p: &str,
    stride: usize,
    depthwise: bool,
    act: Activation,
) -> Result<ConvBn, String> {
    let k = w.get(&format!("{p}.convolution.weight"))?.shape[2];
    conv_bn(
        w,
        &format!("{p}.convolution"),
        &format!("{p}.normalization"),
        stride,
        (k - 1) / 2,
        depthwise,
        act,
    )
}

/// `{p}.conv` + `{p}.norm`（PPDocLayoutV3ConvNormLayer），补边 (k − 1) / 2。
pub(crate) fn conv_norm(
    w: &Weights,
    p: &str,
    stride: usize,
    act: Activation,
) -> Result<ConvBn, String> {
    let k = w.get(&format!("{p}.conv.weight"))?.shape[2];
    conv_bn(
        w,
        &format!("{p}.conv"),
        &format!("{p}.norm"),
        stride,
        (k - 1) / 2,
        false,
        act,
    )
}

/// 下标从 0 起连续存在的权重有几个（ModuleList 的长度）。
pub(crate) fn count(w: &Weights, name: impl Fn(usize) -> String) -> usize {
    (0..).take_while(|&i| w.opt(&name(i)).is_some()).count()
}

/// 多层感知机（`PPDocLayoutV3MLPPredictionHead`）：层间 ReLU，最后一层不加激活。
#[derive(Debug, Clone)]
pub(crate) struct Mlp {
    layers: Vec<Linear>,
}

impl Mlp {
    pub(crate) fn load(w: &Weights, p: &str) -> Result<Mlp, String> {
        let n = count(w, |i| format!("{p}.layers.{i}.weight"));
        if n == 0 {
            return Err(format!("权重里没有 {p}.layers"));
        }
        let layers = (0..n)
            .map(|i| Linear::load(w, &format!("{p}.layers.{i}")))
            .collect::<Result<_, _>>()?;
        Ok(Mlp { layers })
    }

    pub(crate) fn forward(&self, x: &Tensor) -> Tensor {
        let mut h = self.layers[0].forward(x);
        for l in &self.layers[1..] {
            Activation::Relu.apply_all(&mut h);
            h = l.forward(&h);
        }
        h
    }
}

/// 前馈层（`PPDocLayoutV3MLP`）：`{p}.fc1` → 激活 → `{p}.fc2`。
#[derive(Debug, Clone)]
pub(crate) struct Ffn {
    fc1: Linear,
    fc2: Linear,
    act: Activation,
}

impl Ffn {
    pub(crate) fn load(w: &Weights, p: &str, act: Activation) -> Result<Ffn, String> {
        Ok(Ffn {
            fc1: Linear::load(w, &format!("{p}.fc1"))?,
            fc2: Linear::load(w, &format!("{p}.fc2"))?,
            act,
        })
    }

    pub(crate) fn forward(&self, x: &Tensor) -> Tensor {
        let mut h = self.fc1.forward(x);
        self.act.apply_all(&mut h);
        self.fc2.forward(&h)
    }
}

/// 残差 + 层归一化（后归一化）：`norm(x + y)`。
pub(crate) fn add_norm(x: &Tensor, y: &Tensor, norm: &LayerNorm) -> Tensor {
    norm.forward(&ops::add(x, y))
}

/// 多头自注意力（`PPDocLayoutV3SelfAttention`）：位置编码只加在 Q、K 上，V 不加。
#[derive(Debug, Clone)]
pub(crate) struct SelfAttention {
    q: Linear,
    k: Linear,
    v: Linear,
    o: Linear,
    heads: usize,
}

impl SelfAttention {
    /// [协议] 发布权重里的名字是 q_proj、k_proj、v_proj、out_proj（transformers 载入时才改名 o_proj）
    pub(crate) fn load(w: &Weights, p: &str, heads: usize) -> Result<SelfAttention, String> {
        Ok(SelfAttention {
            q: Linear::load(w, &format!("{p}.q_proj"))?,
            k: Linear::load(w, &format!("{p}.k_proj"))?,
            v: Linear::load(w, &format!("{p}.v_proj"))?,
            o: Linear::load(w, &format!("{p}.out_proj"))?,
            heads,
        })
    }

    pub(crate) fn forward(&self, x: &Tensor, pos: &Tensor) -> Tensor {
        let qk = ops::add(x, pos);
        let out = attention(
            &self.q.forward(&qk),
            &self.k.forward(&qk),
            &self.v.forward(x),
            self.heads,
        );
        self.o.forward(&out)
    }
}

/// 取 `[T, C]` 的 `[at, at + n)` 列，结果 `[T, n]`。
fn columns(x: &Tensor, at: usize, n: usize) -> Tensor {
    let (t, c) = x.tc();
    let mut data = Vec::with_capacity(t * n);
    for row in x.data.chunks_exact(c) {
        data.extend_from_slice(&row[at..at + n]);
    }
    Tensor::new(vec![t, n], data)
}

/// 缩放点积注意力，分 `heads` 个头：softmax(Q·Kᵀ / √d)·V，各头结果按列拼回 `[T, C]`。
pub(crate) fn attention(q: &Tensor, k: &Tensor, v: &Tensor, heads: usize) -> Tensor {
    let (t, c) = q.tc();
    let d = c / heads;
    let scale = (d as f32).sqrt().recip();
    let mut out = vec![0f32; t * c];
    for h in 0..heads {
        let at = h * d;
        let kt = ops::transpose(&columns(k, at, d));
        let mut s = ops::matmul(&columns(q, at, d), &kt);
        for x in &mut s.data {
            *x *= scale;
        }
        ops::softmax_rows(&mut s);
        let o = ops::matmul(&s, &columns(v, at, d));
        for (dst, src) in out.chunks_exact_mut(c).zip(o.data.chunks_exact(d)) {
            dst[at..at + d].copy_from_slice(src);
        }
    }
    Tensor::new(vec![t, c], out)
}

/// 二维正弦位置编码（`build_2d_sinusoidal_position_embedding`）：每个位置 `[sin_h | cos_h | sin_w | cos_w]`，
/// 按行优先排，结果 `[H·W, dim]`。
pub(crate) fn sine_pos_2d(h: usize, w: usize, dim: usize, temperature: f64) -> Tensor {
    // [协议] 四段各占 dim 的四分之一
    let pd = dim / 4;
    let omega: Vec<f64> = (0..pd)
        .map(|i| temperature.powf(i as f64 / pd as f64).recip())
        .collect();
    let mut data = Vec::with_capacity(h * w * dim);
    for y in 0..h {
        for x in 0..w {
            for grid in [y as f64, x as f64] {
                data.extend(omega.iter().map(|o| (grid * o).sin() as f32));
                data.extend(omega.iter().map(|o| (grid * o).cos() as f32));
            }
        }
    }
    Tensor::new(vec![h * w, dim], data)
}

/// 一个输出坐标的双线性抽头：(左下标, 右下标, 右边的权重)。
fn bilinear_taps(input: usize, output: usize) -> Vec<(usize, usize, f32)> {
    let scale = input as f32 / output as f32;
    (0..output)
        .map(|o| {
            // align_corners=False：源坐标 = (目标 + ½)·比例 − ½，负数截到 0
            let src = ((o as f32 + 0.5) * scale - 0.5).max(0.0);
            let i0 = (src as usize).min(input - 1);
            (i0, (i0 + 1).min(input - 1), src - i0 as f32)
        })
        .collect()
}

/// 双线性缩放（`F.interpolate(mode="bilinear", align_corners=False)`，不抗锯齿）。
pub(crate) fn resize_bilinear(x: &Tensor, oh: usize, ow: usize) -> Tensor {
    let (c, h, w) = x.chw();
    if (h, w) == (oh, ow) {
        return x.clone();
    }
    let tx = bilinear_taps(w, ow);
    let ty = bilinear_taps(h, oh);
    let mut out = Vec::with_capacity(c * oh * ow);
    for plane in x.data.chunks_exact(h * w) {
        for &(y0, y1, ly) in &ty {
            let (r0, r1) = (&plane[y0 * w..(y0 + 1) * w], &plane[y1 * w..(y1 + 1) * w]);
            out.extend(tx.iter().map(|&(x0, x1, lx)| {
                let top = r0[x0] * (1.0 - lx) + r0[x1] * lx;
                let bottom = r1[x0] * (1.0 - lx) + r1[x1] * lx;
                top * (1.0 - ly) + bottom * ly
            }));
        }
    }
    Tensor::new(vec![c, oh, ow], out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 双线性放大两倍() {
        // 1×2 放大到 2×4：半像素对齐 → 0, 0.25, 0.75, 1，两行相同
        let x = Tensor::new(vec![1, 1, 2], vec![0.0, 1.0]);
        let y = resize_bilinear(&x, 2, 4);
        assert_eq!(y.shape, [1, 2, 4]);
        for (a, b) in y
            .data
            .iter()
            .zip([0.0, 0.25, 0.75, 1.0, 0.0, 0.25, 0.75, 1.0])
        {
            assert!((a - b).abs() < 1e-6, "{:?}", y.data);
        }
    }

    #[test]
    fn 位置编码的布局() {
        let p = sine_pos_2d(2, 3, 8, 10000.0);
        assert_eq!(p.shape, [6, 8]);
        // dim 8：每段 2 个频率，ω = 1, 1/100。第 (y=1, x=2) 个位置
        let row = &p.data[5 * 8..6 * 8];
        let want = [
            1f64.sin(),
            0.01f64.sin(),
            1f64.cos(),
            0.01f64.cos(),
            2f64.sin(),
            0.02f64.sin(),
            2f64.cos(),
            0.02f64.cos(),
        ];
        for (a, b) in row.iter().zip(want) {
            assert!((*a as f64 - b).abs() < 1e-6, "{row:?}");
        }
    }

    #[test]
    fn 注意力按头拼回() {
        // Q = K 时每行注意力集中在内积最大的位置；取足够大的差，结果约等于对应的 V
        let q = Tensor::new(vec![2, 4], vec![50., 0., 0., 50., 0., 50., 50., 0.]);
        let v = Tensor::new(vec![2, 4], vec![1., 2., 3., 4., 5., 6., 7., 8.]);
        let y = attention(&q, &q, &v, 2);
        for (a, b) in y.data.iter().zip(&v.data) {
            assert!((a - b).abs() < 1e-3, "{:?}", y.data);
        }
    }
}
