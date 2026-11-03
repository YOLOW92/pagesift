//! 按定义直接写的朴素实现（改快之前的原版），只在测试里作对照基准。

use crate::Tensor;

pub(super) fn conv2d(
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
    let og = o / groups;
    let oh = (h + 2 * pad.0 - dilation.0 * (kh - 1) - 1) / stride.0 + 1;
    let ow = (wd + 2 * pad.1 - dilation.1 * (kw - 1) - 1) / stride.1 + 1;
    let mut out = vec![0f32; o * oh * ow];
    for oc in 0..o {
        let g = oc / og;
        let b = bias.map_or(0.0, |b| b[oc]);
        for oy in 0..oh {
            for ox in 0..ow {
                let mut s = b;
                for ic in 0..cg {
                    let xc = g * cg + ic;
                    for ky in 0..kh {
                        let iy = (oy * stride.0 + ky * dilation.0) as isize - pad.0 as isize;
                        if iy < 0 || iy >= h as isize {
                            continue;
                        }
                        for kx in 0..kw {
                            let ix = (ox * stride.1 + kx * dilation.1) as isize - pad.1 as isize;
                            if ix < 0 || ix >= wd as isize {
                                continue;
                            }
                            let xv = x.data[(xc * h + iy as usize) * wd + ix as usize];
                            let wv = w.data[((oc * cg + ic) * kh + ky) * kw + kx];
                            s += xv * wv;
                        }
                    }
                }
                out[(oc * oh + oy) * ow + ox] = s;
            }
        }
    }
    Tensor::new(vec![o, oh, ow], out)
}

pub(super) fn conv_transpose2d(
    x: &Tensor,
    w: &Tensor,
    bias: Option<&[f32]>,
    stride: (usize, usize),
    pad: (usize, usize),
) -> Tensor {
    let (c, h, wd) = x.chw();
    let (wc, o, kh, kw) = (w.shape[0], w.shape[1], w.shape[2], w.shape[3]);
    assert_eq!(c, wc, "转置卷积输入通道不符");
    let oh = (h - 1) * stride.0 + kh - 2 * pad.0;
    let ow = (wd - 1) * stride.1 + kw - 2 * pad.1;
    let mut out = vec![0f32; o * oh * ow];
    for oc in 0..o {
        let b = bias.map_or(0.0, |b| b[oc]);
        out[oc * oh * ow..(oc + 1) * oh * ow].fill(b);
    }
    for ic in 0..c {
        for iy in 0..h {
            for ix in 0..wd {
                let xv = x.data[(ic * h + iy) * wd + ix];
                for oc in 0..o {
                    for ky in 0..kh {
                        let oy = (iy * stride.0 + ky) as isize - pad.0 as isize;
                        if oy < 0 || oy >= oh as isize {
                            continue;
                        }
                        for kx in 0..kw {
                            let ox = (ix * stride.1 + kx) as isize - pad.1 as isize;
                            if ox < 0 || ox >= ow as isize {
                                continue;
                            }
                            let wv = w.data[((ic * o + oc) * kh + ky) * kw + kx];
                            out[(oc * oh + oy as usize) * ow + ox as usize] += xv * wv;
                        }
                    }
                }
            }
        }
    }
    Tensor::new(vec![o, oh, ow], out)
}

pub(super) fn max_pool2d(
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
    for ch in 0..c {
        for oy in 0..oh {
            for ox in 0..ow {
                let mut m = f32::NEG_INFINITY;
                for ky in 0..k.0 {
                    let iy = oy * stride.0 + ky;
                    if iy >= h {
                        continue;
                    }
                    for kx in 0..k.1 {
                        let ix = ox * stride.1 + kx;
                        if ix < w {
                            m = m.max(x.data[(ch * h + iy) * w + ix]);
                        }
                    }
                }
                out[(ch * oh + oy) * ow + ox] = m;
            }
        }
    }
    Tensor::new(vec![c, oh, ow], out)
}

pub(super) fn avg_pool2d(x: &Tensor, k: (usize, usize)) -> Tensor {
    let (c, h, w) = x.chw();
    let (oh, ow) = (h / k.0, w / k.1);
    let n = (k.0 * k.1) as f32;
    let mut out = vec![0f32; c * oh * ow];
    for ch in 0..c {
        for oy in 0..oh {
            for ox in 0..ow {
                let mut s = 0f32;
                for ky in 0..k.0 {
                    for kx in 0..k.1 {
                        s += x.data[(ch * h + oy * k.0 + ky) * w + ox * k.1 + kx];
                    }
                }
                out[(ch * oh + oy) * ow + ox] = s / n;
            }
        }
    }
    Tensor::new(vec![c, oh, ow], out)
}

pub(super) fn interpolate_nearest(x: &Tensor, oh: usize, ow: usize) -> Tensor {
    let (c, h, w) = x.chw();
    let mut out = vec![0f32; c * oh * ow];
    for ch in 0..c {
        for oy in 0..oh {
            let iy = ((oy as f64 * h as f64 / oh as f64) as usize).min(h - 1);
            for ox in 0..ow {
                let ix = ((ox as f64 * w as f64 / ow as f64) as usize).min(w - 1);
                out[(ch * oh + oy) * ow + ox] = x.data[(ch * h + iy) * w + ix];
            }
        }
    }
    Tensor::new(vec![c, oh, ow], out)
}

pub(super) fn matmul(a: &Tensor, b: &Tensor) -> Tensor {
    let (m, k) = a.tc();
    let (kb, n) = b.tc();
    assert_eq!(k, kb, "矩阵乘的内维不同");
    let mut out = vec![0f32; m * n];
    for i in 0..m {
        for p in 0..k {
            let av = a.data[i * k + p];
            let row = &b.data[p * n..(p + 1) * n];
            for (o, &bv) in out[i * n..(i + 1) * n].iter_mut().zip(row) {
                *o += av * bv;
            }
        }
    }
    Tensor::new(vec![m, n], out)
}

pub(super) fn transpose(a: &Tensor) -> Tensor {
    let (m, n) = a.tc();
    let mut out = vec![0f32; m * n];
    for i in 0..m {
        for j in 0..n {
            out[j * m + i] = a.data[i * n + j];
        }
    }
    Tensor::new(vec![n, m], out)
}
