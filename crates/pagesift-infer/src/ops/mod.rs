//! 无参数的算子（语义同 `torch.nn.functional` 的同名函数）。卷积和矩阵乘按形状换了快算法
//! （见 [`conv`]、[`gemm`] 模块），与按定义的结果只差浮点舍入；按定义写的原版留在测试的
//! `reference` 模块里作对照。

mod conv;
mod gemm;
mod kernel;
#[cfg(test)]
mod reference;
#[cfg(test)]
mod tests;

use crate::{Tensor, backend, par};

/// 二维卷积（`F.conv2d`）。`x` 是 `[C, H, W]`，`w` 是 `[O, C/groups, kh, kw]`；
/// `pad` 是上下、左右各补的 0，`stride`、`dilation` 按 (行, 列)。
#[allow(clippy::too_many_arguments)]
pub fn conv2d(
    x: &Tensor,
    w: &Tensor,
    bias: Option<&[f32]>,
    stride: (usize, usize),
    pad: (usize, usize),
    dilation: (usize, usize),
    groups: usize,
) -> Tensor {
    let (c, h, wd) = x.chw();
    let (o, cg, kh, kw) = (w.shape[0], w.shape[1], w.shape[2], w.shape[3]);
    assert_eq!(c, cg * groups, "卷积输入通道 {c} 与权重 {cg}×{groups} 不符");
    assert_eq!(o % groups, 0, "输出通道 {o} 不能被 groups {groups} 整除");
    let oh = (h + 2 * pad.0 - dilation.0 * (kh - 1) - 1) / stride.0 + 1;
    let ow = (wd + 2 * pad.1 - dilation.1 * (kw - 1) - 1) / stride.1 + 1;
    if let Some(data) =
        backend::current().and_then(|b| b.conv2d(x, w, bias, stride, pad, dilation, groups))
    {
        return Tensor::new(vec![o, oh, ow], data);
    }
    let g = conv::Geom {
        stride,
        pad,
        dilation,
        k: (kh, kw),
        input: (h, wd),
        output: (oh, ow),
    };
    conv::conv2d(x, w, bias, g, groups)
}

/// 二维转置卷积（`F.conv_transpose2d`，groups = 1，无输出补边）。`w` 是 `[C, O, kh, kw]`。
pub fn conv_transpose2d(
    x: &Tensor,
    w: &Tensor,
    bias: Option<&[f32]>,
    stride: (usize, usize),
    pad: (usize, usize),
) -> Tensor {
    let (c, h, wd) = x.chw();
    let (wc, kh, kw) = (w.shape[0], w.shape[2], w.shape[3]);
    assert_eq!(c, wc, "转置卷积输入通道不符");
    let oh = (h - 1) * stride.0 + kh - 2 * pad.0;
    let ow = (wd - 1) * stride.1 + kw - 2 * pad.1;
    conv::conv_transpose2d(x, w, bias, stride, pad, (oh, ow))
}

/// 在右边补 `right` 列、下边补 `bottom` 行 0（`F.pad(x, (0, right, 0, bottom))`）。
pub fn pad_right_bottom(x: &Tensor, right: usize, bottom: usize) -> Tensor {
    let (c, h, w) = x.chw();
    let (nh, nw) = (h + bottom, w + right);
    let mut out = vec![0f32; c * nh * nw];
    for ch in 0..c {
        for y in 0..h {
            let src = &x.data[(ch * h + y) * w..(ch * h + y + 1) * w];
            out[(ch * nh + y) * nw..(ch * nh + y) * nw + w].copy_from_slice(src);
        }
    }
    Tensor::new(vec![c, nh, nw], out)
}

/// 最大池化（`F.max_pool2d`，无补边）。`ceil_mode` 为真时输出尺寸向上取整，越界的窗口只取界内。
pub fn max_pool2d(
    x: &Tensor,
    k: (usize, usize),
    stride: (usize, usize),
    ceil_mode: bool,
) -> Tensor {
    let (c, h, w) = x.chw();
    let size = |n: usize, k: usize, s: usize| {
        if ceil_mode {
            let o = (n - k).div_ceil(s) + 1;
            // PyTorch：最后一个窗口必须从界内开始
            if (o - 1) * s >= n { o - 1 } else { o }
        } else {
            (n - k) / s + 1
        }
    };
    let (oh, ow) = (size(h, k.0, stride.0), size(w, k.1, stride.1));
    let mut out = vec![f32::NEG_INFINITY; c * oh * ow];
    let threads = par::threads_for(out.len() * k.0 * k.1, c);
    // 按通道多线程；每个输出行对窗口里的每个 (ky, kx) 扫一遍输入行，步长为 1 时是连续切片
    par::for_chunks_mut(&mut out, oh * ow, threads, |ch, plane| {
        let xc = &x.data[ch * h * w..(ch + 1) * h * w];
        for (oy, orow) in plane.chunks_exact_mut(ow).enumerate() {
            let y0 = oy * stride.0;
            for iy in y0..(y0 + k.0).min(h) {
                let irow = &xc[iy * w..(iy + 1) * w];
                for kx in 0..k.1.min(w) {
                    // 满足 ox·s + kx < w 的输出列数
                    let n = ((w - 1 - kx) / stride.1 + 1).min(ow);
                    if stride.1 == 1 {
                        for (o, &v) in orow[..n].iter_mut().zip(&irow[kx..kx + n]) {
                            *o = o.max(v);
                        }
                    } else {
                        for (ox, o) in orow[..n].iter_mut().enumerate() {
                            *o = o.max(irow[ox * stride.1 + kx]);
                        }
                    }
                }
            }
        }
    });
    Tensor::new(vec![c, oh, ow], out)
}

/// 平均池化（`F.avg_pool2d(x, k)`：步长等于核大小、无补边、向下取整）。
pub fn avg_pool2d(x: &Tensor, k: (usize, usize)) -> Tensor {
    let (c, h, w) = x.chw();
    let (oh, ow) = (h / k.0, w / k.1);
    let n = (k.0 * k.1) as f32;
    let mut out = Vec::with_capacity(c * oh * ow);
    // 逐通道、逐输出行，下标按行递推，免去每个元素的除法
    for ch in 0..c {
        for oy in 0..oh {
            for ox in 0..ow {
                let mut s = 0f32;
                for ky in 0..k.0 {
                    let at = (ch * h + oy * k.0 + ky) * w + ox * k.1;
                    for &v in &x.data[at..at + k.1] {
                        s += v;
                    }
                }
                out.push(s / n);
            }
        }
    }
    Tensor::new(vec![c, oh, ow], out)
}

/// 每个通道的全局平均（`AdaptiveAvgPool2d(1)`），结果 `[C]`。
pub fn global_avg_pool(x: &Tensor) -> Vec<f32> {
    let (c, h, w) = x.chw();
    let n = (h * w) as f32;
    (0..c)
        .map(|ch| x.data[ch * h * w..(ch + 1) * h * w].iter().sum::<f32>() / n)
        .collect()
}

/// 每个通道乘一个系数（SE 注意力的重标定）。
pub fn scale_channels(x: &mut Tensor, s: &[f32]) {
    let (c, h, w) = x.chw();
    assert_eq!(c, s.len());
    for (ch, &k) in s.iter().enumerate() {
        for v in &mut x.data[ch * h * w..(ch + 1) * h * w] {
            *v *= k;
        }
    }
}

/// 按通道拼接（`torch.cat(dim=0)`，各张量的 H、W 相同）。
pub fn concat_channels(xs: &[&Tensor]) -> Tensor {
    let (_, h, w) = xs[0].chw();
    let mut data = Vec::with_capacity(xs.iter().map(|x| x.data.len()).sum());
    let mut c = 0;
    for x in xs {
        let (xc, xh, xw) = x.chw();
        assert_eq!((xh, xw), (h, w), "拼接的张量尺寸不同");
        data.extend_from_slice(&x.data);
        c += xc;
    }
    Tensor::new(vec![c, h, w], data)
}

/// 逐元素相加，形状必须相同。
pub fn add(a: &Tensor, b: &Tensor) -> Tensor {
    assert_eq!(a.shape, b.shape, "相加的形状不同");
    let data = a.data.iter().zip(&b.data).map(|(x, y)| x + y).collect();
    Tensor::new(a.shape.clone(), data)
}

/// 最近邻插值到 `oh × ow`（`F.interpolate(mode="nearest")`：源坐标 = ⌊目标坐标 · 输入/输出⌋）。
pub fn interpolate_nearest(x: &Tensor, oh: usize, ow: usize) -> Tensor {
    let (c, h, w) = x.chw();
    // 每个输出列的源列只算一次
    let src_x: Vec<usize> = (0..ow)
        .map(|ox| ((ox as f64 * w as f64 / ow as f64) as usize).min(w - 1))
        .collect();
    let mut out = Vec::with_capacity(c * oh * ow);
    for ch in 0..c {
        for oy in 0..oh {
            let iy = ((oy as f64 * h as f64 / oh as f64) as usize).min(h - 1);
            let row = &x.data[(ch * h + iy) * w..(ch * h + iy + 1) * w];
            out.extend(src_x.iter().map(|&ix| row[ix]));
        }
    }
    Tensor::new(vec![c, oh, ow], out)
}

/// 矩阵乘：`a` 是 `[M, K]`，`b` 是 `[K, N]`，结果 `[M, N]`。
pub fn matmul(a: &Tensor, b: &Tensor) -> Tensor {
    matmul_with(a, b, false)
}

/// 同 [`matmul`]，`b_is_weight` 告诉后端 `b` 是长期不变的权重（`Linear` 用），值得缓存在显存。
pub(crate) fn matmul_with(a: &Tensor, b: &Tensor, b_is_weight: bool) -> Tensor {
    let (m, k) = a.tc();
    let (kb, n) = b.tc();
    assert_eq!(k, kb, "矩阵乘的内维不同");
    if let Some(data) = backend::current().and_then(|d| d.matmul(a, b, b_is_weight)) {
        return Tensor::new(vec![m, n], data);
    }
    Tensor::new(
        vec![m, n],
        gemm::gemm_new((m, n, k), &a.data, k, &b.data, n, None),
    )
}

/// [推导] 转置的分块边长：源块、目标块各 32×32×4 B = 4 KB，合起来在 L1d（32 KB）里，
/// 按列读源时不至于每个元素一次缓存缺失。
const TRANSPOSE_BLOCK: usize = 32;

/// 转置 `[M, N]` → `[N, M]`。
pub fn transpose(a: &Tensor) -> Tensor {
    let (m, n) = a.tc();
    let mut out = vec![0f32; m * n];
    for i0 in (0..m).step_by(TRANSPOSE_BLOCK) {
        for j0 in (0..n).step_by(TRANSPOSE_BLOCK) {
            for i in i0..(i0 + TRANSPOSE_BLOCK).min(m) {
                for j in j0..(j0 + TRANSPOSE_BLOCK).min(n) {
                    out[j * m + i] = a.data[i * n + j];
                }
            }
        }
    }
    Tensor::new(vec![n, m], out)
}

/// 按行 softmax（`[T, C]` 的最后一维）。
pub fn softmax_rows(x: &mut Tensor) {
    let (_, c) = x.tc();
    for row in x.data.chunks_mut(c) {
        let m = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let mut s = 0f32;
        for v in row.iter_mut() {
            *v = (*v - m).exp();
            s += *v;
        }
        for v in row.iter_mut() {
            *v /= s;
        }
    }
}

/// `[C, H, W]` 展平成序列 `[H·W, C]`（`flatten(2).transpose(1, 2)`）。
pub fn chw_to_seq(x: &Tensor) -> Tensor {
    let (c, h, w) = x.chw();
    transpose(&Tensor::new(vec![c, h * w], x.data.clone()))
}

/// 序列 `[H·W, C]` 还原成 `[C, H, W]`。
pub fn seq_to_chw(x: &Tensor, h: usize, w: usize) -> Tensor {
    let (t, c) = x.tc();
    assert_eq!(t, h * w);
    let d = transpose(x);
    Tensor::new(vec![c, h, w], d.data)
}
