//! [`BatchExec`] 的各个算子：语义同 `pagesift_infer::exec::Cpu`，批次维藏在 `self.n` 里。

use std::rc::Rc;

use pagesift_infer::exec::{Exec, attention_scale};
use pagesift_infer::{Activation, Conv2d, ConvTranspose2d, LayerNorm, Linear};

use crate::buffer::Args;
use crate::exec::{BatchExec, DevTensor};
use crate::ops::ConvShape;

/// 图像特征的 `[C, H, W]`。
fn chw(x: &DevTensor<'_, '_>) -> (usize, usize, usize) {
    // [协议] 图像特征的约定形状是三维
    assert_eq!(x.shape.len(), 3, "需要 [C, H, W]，实际 {:?}", x.shape);
    (x.shape[0], x.shape[1], x.shape[2])
}

/// 序列的 `[T, C]`。
fn tc(x: &DevTensor<'_, '_>) -> (usize, usize) {
    assert_eq!(x.shape.len(), 2, "需要 [T, C]，实际 {:?}", x.shape);
    (x.shape[0], x.shape[1])
}

/// 对各样本的有效宽度套同一个尺寸公式。
fn map_widths(w: &[u32], f: impl Fn(usize) -> usize) -> Rc<Vec<u32>> {
    Rc::new(w.iter().map(|&v| f(v as usize) as u32).collect())
}

impl<'c, 'g> BatchExec<'c, 'g> {
    fn elems(&self, shape: &[usize]) -> usize {
        self.n * shape.iter().product::<usize>()
    }

    fn new_tensor(&self, shape: Vec<usize>, widths: Rc<Vec<u32>>) -> DevTensor<'c, 'g> {
        let len = self.elems(&shape);
        self.alloc(len, shape, widths)
    }
}

impl<'c, 'g> BatchExec<'c, 'g> {
    /// 隐式卷积：见 [`ConvShape::implicit`]。权重按 `[kh, kw, o, c]` 重排后存放，每个核位置是连续的
    /// `[o, c]`；输出先写到行距 `nw` 的临时缓冲，最后裁掉每行多余的列。每个通道算
    /// `(oh − 1)·nw + ow` 列：最后一行只算有效的 `ow` 列，窗口才不会读出补边后的平面。
    fn conv_implicit(
        &self,
        l: &Conv2d,
        x: &DevTensor<'c, 'g>,
        s: &ConvShape,
        out: &DevTensor<'c, 'g>,
        bias: Option<u64>,
    ) {
        let (oh, ow) = s.out_hw();
        let (nh, nw) = (s.h + 2 * s.pad.0, s.w + 2 * s.pad.1);
        let none = || Rc::new(Vec::new());
        let k = &self.ctx.gpu.k;
        let padded = self.new_tensor(vec![s.c, nh, nw], none());
        let total = self.elems(&padded.shape);
        let args = Args::default()
            .raw_ptr(x.ptr())
            .raw_ptr(padded.ptr())
            .u32(self.n * s.c)
            .u32(s.h)
            .u32(s.w)
            .u32(s.pad.0)
            .u32(s.pad.1)
            .u32(nh)
            .u32(nw);
        self.d1(k.pad_all, total, args);
        let cols = (oh - 1) * nw + ow;
        let wide = self.new_tensor(vec![s.o, cols], none());
        let wt = self.weight_kernel_first(l);
        let per_k = s.o * s.c; // 一个核位置的权重个数
        let ldb = nh * nw;
        for (i, (ky, kx)) in (0..s.kh)
            .flat_map(|ky| (0..s.kw).map(move |kx| (ky, kx)))
            .enumerate()
        {
            if !self.live() {
                break;
            }
            let (a, b) = (
                wt + bytes_of(i * per_k),
                padded.ptr() + bytes_of(ky * nw + kx),
            );
            self.check(self.ctx.gpu.sgemm_ext(
                a,
                b,
                wide.ptr(),
                (s.o, cols, s.c),
                ldb,
                i > 0,
                self.n,
                (0, s.c * ldb, s.o * cols),
            ));
        }
        let total = self.elems(&out.shape);
        let args = Args::default()
            .raw_ptr(wide.ptr())
            .raw_ptr(out.ptr())
            .u32(cols)
            .u32(nw)
            .u32(oh)
            .u32(ow)
            .u32(total);
        self.d1(k.crop_cols, total, args);
        if let Some(b) = bias {
            let args = Args::default()
                .raw_ptr(out.ptr())
                .raw_ptr(b)
                .u32(oh * ow)
                .u32(s.o)
                .u32(total);
            self.d1(k.add_bias, total, args);
        }
    }
}

impl<'c, 'g> Exec for BatchExec<'c, 'g> {
    type T = DevTensor<'c, 'g>;

    fn shape(&self, x: &Self::T) -> Vec<usize> {
        x.shape.clone()
    }

    fn conv(&self, l: &Conv2d, x: &Self::T) -> Self::T {
        let (c, h, w) = chw(x);
        let s = ConvShape {
            c,
            h,
            w,
            o: l.weight.shape[0],
            kh: l.weight.shape[2],
            kw: l.weight.shape[3],
            stride: l.stride,
            pad: l.pad,
            dilation: l.dilation,
            groups: l.groups,
        };
        let (oh, ow) = s.out_hw();
        let widths = map_widths(&x.widths, |v| {
            (v + 2 * s.pad.1 - s.dilation.1 * (s.kw - 1) - 1) / s.stride.1 + 1
        });
        let out = self.new_tensor(vec![s.o, oh, ow], widths);
        let bias = l.bias.as_deref().map(|b| self.weight(b));
        if s.implicit() {
            self.conv_implicit(l, x, &s, &out, bias);
            self.mask(&out);
            return out;
        }
        let wt = self.weight(&l.weight.data);
        // im2col 的临时缓冲也要算进试算的峰值
        let col = s.needs_col().then(|| {
            let len = self.n * s.col_len();
            self.alloc(len, vec![len], Rc::default())
        });
        if self.live() {
            self.check(self.ctx.gpu.conv_raw(
                x.ptr(),
                wt,
                bias,
                &s,
                self.n,
                out.ptr(),
                col.as_ref().map(|t| t.ptr()),
            ));
        }
        drop(col);
        self.mask(&out);
        out
    }

    fn conv_transpose(&self, l: &ConvTranspose2d, x: &Self::T) -> Self::T {
        let (c, h, w) = chw(x);
        let (o, kh, kw) = (l.weight.shape[1], l.weight.shape[2], l.weight.shape[3]);
        let oh = (h - 1) * l.stride.0 + kh - 2 * l.pad.0;
        let ow = (w - 1) * l.stride.1 + kw - 2 * l.pad.1;
        let widths = map_widths(&x.widths, |v| (v - 1) * l.stride.1 + kw - 2 * l.pad.1);
        let out = self.new_tensor(vec![o, oh, ow], widths);
        let wt = self.weight(&l.weight.data);
        let bias = l.bias.as_deref().map_or(0, |b| self.weight(b));
        let total = self.elems(&out.shape);
        let args = Args::default()
            .raw_ptr(x.ptr())
            .raw_ptr(wt)
            .raw_ptr(bias)
            .raw_ptr(out.ptr())
            .u32(c)
            .u32(o)
            .u32(l.bias.is_some() as usize)
            .u32(h)
            .u32(w)
            .u32(kh)
            .u32(kw)
            .u32(l.stride.0)
            .u32(l.stride.1)
            .u32(l.pad.0)
            .u32(l.pad.1)
            .u32(oh)
            .u32(ow)
            .u32(total);
        self.d1(self.ctx.gpu.k.conv_transpose, total, args);
        self.mask(&out);
        out
    }

    fn act(&self, a: Activation, x: &mut Self::T) {
        let k = &self.ctx.gpu.k;
        let f = match a {
            Activation::Identity => return,
            Activation::Relu => k.relu,
            Activation::Silu => k.silu,
            Activation::Gelu => k.gelu,
            Activation::HardSigmoid => k.hardsigmoid,
            Activation::Sigmoid => k.sigmoid,
        };
        let total = self.elems(&x.shape);
        self.d1(f, total, Args::default().raw_ptr(x.ptr()).u32(total));
        // f(0) ≠ 0 的激活会把补边的列变成非零
        // [协议] 序列是二维、图像特征是三维，只有图像特征有“列”要清
        if a.apply(0.0) != 0.0 && x.shape.len() == 3 {
            self.mask(x);
        }
    }

    fn add(&self, a: &Self::T, b: &Self::T) -> Self::T {
        assert_eq!(a.shape, b.shape, "相加的形状不同");
        let out = self.new_tensor(a.shape.clone(), a.widths.clone());
        let total = self.elems(&a.shape);
        let args = Args::default()
            .raw_ptr(a.ptr())
            .raw_ptr(b.ptr())
            .raw_ptr(out.ptr())
            .u32(total);
        self.d1(self.ctx.gpu.k.add2, total, args);
        out
    }

    fn add_assign(&self, a: &mut Self::T, b: &Self::T) {
        assert_eq!(a.shape, b.shape, "相加的形状不同");
        let total = self.elems(&a.shape);
        let args = Args::default()
            .raw_ptr(a.ptr())
            .raw_ptr(b.ptr())
            .raw_ptr(a.ptr())
            .u32(total);
        self.d1(self.ctx.gpu.k.add2, total, args);
    }

    fn pad_right_bottom(&self, x: &Self::T, right: usize, bottom: usize) -> Self::T {
        let (c, h, w) = chw(x);
        let (nh, nw) = (h + bottom, w + right);
        let out = self.new_tensor(vec![c, nh, nw], map_widths(&x.widths, |v| v + right));
        let total = self.elems(&out.shape);
        let args = Args::default()
            .raw_ptr(x.ptr())
            .raw_ptr(out.ptr())
            .u32(self.n * c)
            .u32(h)
            .u32(w)
            .u32(nh)
            .u32(nw);
        self.d1(self.ctx.gpu.k.pad_rb, total, args);
        out
    }

    fn max_pool2d(
        &self,
        x: &Self::T,
        k: (usize, usize),
        stride: (usize, usize),
        ceil_mode: bool,
    ) -> Self::T {
        let (c, h, w) = chw(x);
        // 尺寸公式同 pagesift_infer::ops::max_pool2d
        let size = |n: usize, k: usize, s: usize| {
            if ceil_mode {
                let o = (n - k).div_ceil(s) + 1;
                if (o - 1) * s >= n { o - 1 } else { o }
            } else {
                (n - k) / s + 1
            }
        };
        let (oh, ow) = (size(h, k.0, stride.0), size(w, k.1, stride.1));
        let out = self.new_tensor(
            vec![c, oh, ow],
            map_widths(&x.widths, |v| size(v, k.1, stride.1)),
        );
        let total = self.elems(&out.shape);
        let args = Args::default()
            .raw_ptr(x.ptr())
            .raw_ptr(out.ptr())
            .u32(self.n * c)
            .u32(h)
            .u32(w)
            .u32(k.0)
            .u32(k.1)
            .u32(stride.0)
            .u32(stride.1)
            .u32(oh)
            .u32(ow);
        self.d1(self.ctx.gpu.k.pool_max, total, args);
        self.mask(&out);
        out
    }

    fn avg_pool2d(&self, x: &Self::T, k: (usize, usize)) -> Self::T {
        let (c, h, w) = chw(x);
        let (oh, ow) = (h / k.0, w / k.1);
        let out = self.new_tensor(vec![c, oh, ow], map_widths(&x.widths, |v| v / k.1));
        let total = self.elems(&out.shape);
        let args = Args::default()
            .raw_ptr(x.ptr())
            .raw_ptr(out.ptr())
            .u32(self.n * c)
            .u32(h)
            .u32(w)
            .u32(k.0)
            .u32(k.1)
            .u32(oh)
            .u32(ow);
        self.d1(self.ctx.gpu.k.pool_avg, total, args);
        self.mask(&out);
        out
    }

    fn global_avg(&self, x: &Self::T) -> Self::T {
        let (c, h, w) = chw(x);
        let out = self.new_tensor(vec![c, 1, 1], Rc::new(vec![1; self.n]));
        let total = self.n * c;
        let args = Args::default()
            .raw_ptr(x.ptr())
            .raw_ptr(out.ptr())
            .raw_ptr(self.widths_ptr(&x.widths))
            .u32(c)
            .u32(h)
            .u32(w)
            .u32(total);
        self.d1(self.ctx.gpu.k.global_avg, total, args);
        out
    }

    fn scale_channels(&self, x: &mut Self::T, s: &Self::T) {
        let (_, h, w) = chw(x);
        let total = self.elems(&x.shape);
        let args = Args::default()
            .raw_ptr(x.ptr())
            .raw_ptr(s.ptr())
            .u32(h * w)
            .u32(total);
        self.d1(self.ctx.gpu.k.scale_ch, total, args);
    }

    fn se_gate(&self, s: &mut Self::T) {
        let total = self.elems(&s.shape);
        let args = Args::default().raw_ptr(s.ptr()).u32(total);
        self.d1(self.ctx.gpu.k.se_gate, total, args);
    }

    fn concat(&self, xs: &[&Self::T]) -> Self::T {
        let (_, h, w) = chw(xs[0]);
        let c: usize = xs.iter().map(|x| chw(x).0).sum();
        let out = self.new_tensor(vec![c, h, w], xs[0].widths.clone());
        let stride = c * h * w;
        let mut at = 0usize;
        for x in xs {
            let (xc, xh, xw) = chw(x);
            assert_eq!((xh, xw), (h, w), "拼接的张量尺寸不同");
            let len = xc * h * w;
            let args = Args::default()
                .raw_ptr(x.ptr())
                .raw_ptr(out.ptr() + bytes_of(at))
                .u32(len)
                .u32(stride)
                .u32(self.n * len);
            self.d1(self.ctx.gpu.k.copy_part, self.n * len, args);
            at += len;
        }
        out
    }

    fn upsample(&self, x: &Self::T, oh: usize, ow: usize) -> Self::T {
        let (c, h, w) = chw(x);
        let out = self.new_tensor(
            vec![c, oh, ow],
            map_widths(&x.widths, |v| (v * ow).div_ceil(w)),
        );
        let total = self.elems(&out.shape);
        let args = Args::default()
            .raw_ptr(x.ptr())
            .raw_ptr(out.ptr())
            .u32(self.n * c)
            .u32(h)
            .u32(w)
            .u32(oh)
            .u32(ow);
        self.d1(self.ctx.gpu.k.interp_nearest, total, args);
        self.mask(&out);
        out
    }

    fn chw_to_seq(&self, x: &Self::T) -> Self::T {
        let (c, h, w) = chw(x);
        assert!(
            h == 1 || x.widths.iter().all(|&v| v as usize == w),
            "高不为 1 时各样本要一样宽，序列长度才说得清"
        );
        let hw = h * w;
        let out = self.new_tensor(vec![hw, c], map_widths(&x.widths, |v| v * h));
        let total = self.elems(&out.shape);
        let args = Args::default()
            .raw_ptr(x.ptr())
            .raw_ptr(out.ptr())
            .u32(c)
            .u32(hw)
            .u32(total);
        self.d1(self.ctx.gpu.k.chw_to_seq, total, args);
        out
    }

    fn linear(&self, l: &Linear, x: &Self::T) -> Self::T {
        let (t, k) = tc(x);
        let (kb, o) = (l.weight_t.shape[0], l.weight_t.shape[1]);
        assert_eq!(k, kb, "矩阵乘的内维不同");
        let out = self.new_tensor(vec![t, o], x.widths.clone());
        let wt = self.weight(&l.weight_t.data);
        let bias = l.bias.as_deref().map(|b| self.weight(b));
        if self.live() {
            self.check(self.ctx.gpu.sgemm_raw(
                x.ptr(),
                wt,
                out.ptr(),
                (t, o, k),
                self.n,
                (t * k, 0, t * o),
            ));
        }
        if let Some(b) = bias {
            // add_bias 按 (i / hw) % wrap 取偏置：hw = 1、wrap = o 就是按列加
            let total = self.elems(&out.shape);
            let args = Args::default()
                .raw_ptr(out.ptr())
                .raw_ptr(b)
                .u32(1)
                .u32(o)
                .u32(total);
            self.d1(self.ctx.gpu.k.add_bias, total, args);
        }
        out
    }

    fn layer_norm(&self, l: &LayerNorm, x: &Self::T) -> Self::T {
        let (t, c) = tc(x);
        let out = self.new_tensor(vec![t, c], x.widths.clone());
        let (wt, bias) = (self.weight(&l.weight), self.weight(&l.bias));
        let rows = self.n * t;
        let args = Args::default()
            .raw_ptr(x.ptr())
            .raw_ptr(out.ptr())
            .raw_ptr(wt)
            .raw_ptr(bias)
            .f32(l.eps)
            .u32(c)
            .u32(rows);
        self.d1(self.ctx.gpu.k.layer_norm, rows, args);
        out
    }

    fn attention(&self, qkv: &Self::T, heads: usize) -> Self::T {
        let (t, c3) = tc(qkv);
        let c = c3 / 3; // [协议] q、k、v 三份
        let hd = c / heads;
        let nh = self.n * heads;
        let k = &self.ctx.gpu.k;
        let none = || Rc::new(Vec::new());
        let q = self.new_tensor(vec![heads, t, hd], none());
        let kt = self.new_tensor(vec![heads, hd, t], none());
        let v = self.new_tensor(vec![heads, t, hd], none());
        let total = nh * t * hd;
        let args = Args::default()
            .raw_ptr(qkv.ptr())
            .raw_ptr(q.ptr())
            .raw_ptr(kt.ptr())
            .raw_ptr(v.ptr())
            .u32(t)
            .u32(heads)
            .u32(hd)
            .u32(total);
        self.d1(k.attn_split, total, args);
        let a = self.new_tensor(vec![heads, t, t], none());
        if self.live() {
            self.check(self.ctx.gpu.sgemm_raw(
                q.ptr(),
                kt.ptr(),
                a.ptr(),
                (t, t, hd),
                nh,
                (t * hd, hd * t, t * t),
            ));
        }
        drop((q, kt));
        let rows = nh * t;
        let args = Args::default()
            .raw_ptr(a.ptr())
            .raw_ptr(self.widths_ptr(&qkv.widths))
            .f32(attention_scale(hd))
            .u32(t)
            .u32(heads)
            .u32(rows);
        self.d1(k.attn_softmax, rows, args);
        let o = self.new_tensor(vec![heads, t, hd], none());
        if self.live() {
            self.check(self.ctx.gpu.sgemm_raw(
                a.ptr(),
                v.ptr(),
                o.ptr(),
                (t, hd, t),
                nh,
                (t * t, t * hd, t * hd),
            ));
        }
        drop((a, v));
        let out = self.new_tensor(vec![t, c], qkv.widths.clone());
        let total = self.elems(&out.shape);
        let args = Args::default()
            .raw_ptr(o.ptr())
            .raw_ptr(out.ptr())
            .u32(t)
            .u32(heads)
            .u32(hd)
            .u32(total);
        self.d1(k.attn_merge, total, args);
        out
    }

    fn ctc_digest(&self, logits: &Self::T) -> Vec<Vec<(u32, f32)>> {
        let (t, n) = tc(logits);
        let rows = self.n * t;
        let out = self.new_tensor(vec![t, 2], logits.widths.clone());
        let args = Args::default()
            .raw_ptr(logits.ptr())
            .raw_ptr(out.ptr())
            .u32(n)
            .u32(rows);
        self.d1(self.ctx.gpu.k.ctc_digest, rows, args);
        if !self.live() {
            return vec![Vec::new(); self.n];
        }
        let host = match self.download(&out) {
            Ok(h) => h,
            Err(e) => {
                self.fail(e);
                return vec![Vec::new(); self.n];
            }
        };
        logits
            .widths
            .iter()
            .enumerate()
            .map(|(z, &len)| {
                host[z * t * 2..(z * t + len as usize) * 2]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|p| (p[0].to_bits(), p[1]))
                    .collect()
            })
            .collect()
    }
}

/// `n` 个 f32 的字节数，用于设备地址的偏移。
fn bytes_of(n: usize) -> u64 {
    crate::pool::bytes(n)
}
