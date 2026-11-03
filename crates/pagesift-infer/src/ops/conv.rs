//! 卷积与转置卷积的快速实现，按形状分派：
//! - 每组只有一个输入通道（深度卷积）：直接循环，内层对一整行输出做连续的乘加；
//! - 1×1、步长 1、无补边：输出 `[O, H·W]` 就是权重 `[O, C]` 乘输入 `[C, H·W]`；
//! - 其余：按输出像素分段做 im2col，每段一次矩阵乘，段与段多线程。
//!
//! 输出先填偏置再累加乘积，与按定义的求和只差浮点舍入。

use super::gemm::{self, Init, Scratch};
use super::kernel::{self, NR};
use crate::Tensor;
use crate::par::{self, SyncPtr};

/// 一次卷积的几何参数（都按 (行, 列)）。
#[derive(Clone, Copy)]
pub(super) struct Geom {
    pub stride: (usize, usize),
    pub pad: (usize, usize),
    pub dilation: (usize, usize),
    pub k: (usize, usize),
    /// 输入的高、宽
    pub input: (usize, usize),
    /// 输出的高、宽
    pub output: (usize, usize),
}

/// 一行里对某个核列 `kx`，满足 0 ≤ ox·s + off < w 的输出列区间 `[lo, hi)`（off = kx·dilation − pad）。
fn valid_cols(ow: usize, s: usize, off: isize, w: usize) -> (usize, usize) {
    let lo = if off >= 0 {
        0
    } else {
        ((-off) as usize).div_ceil(s)
    };
    let last = w as isize - 1 - off;
    let hi = if last < 0 {
        0
    } else {
        (last as usize / s + 1).min(ow)
    };
    (lo.min(hi), hi)
}

/// 输出 `[o, oh·ow]`，每个通道先填偏置（转置卷积用；卷积由各线程自己填）。
fn biased_output(o: usize, plane: usize, bias: Option<&[f32]>) -> Vec<f32> {
    match bias {
        Some(b) => {
            assert!(b.len() >= o, "偏置比输出通道数短");
            let mut out = Vec::with_capacity(o * plane);
            for &v in b {
                out.resize(out.len() + plane, v);
            }
            out
        }
        None => vec![0.0; o * plane],
    }
}

/// 卷积入口。形状已由调用方检查；`og` 是每组输出通道数。
pub(super) fn conv2d(
    x: &Tensor,
    w: &Tensor,
    bias: Option<&[f32]>,
    g: Geom,
    groups: usize,
) -> Tensor {
    let o = w.shape[0];
    let cg = w.shape[1];
    let (oh, ow) = g.output;
    if let Some(b) = bias {
        assert!(b.len() >= o, "偏置比输出通道数短");
    }
    let mut out = Vec::with_capacity(o * oh * ow);
    let dst = SyncPtr::new(out.as_mut_ptr());
    // SAFETY：三条路径都把 o·oh·ow 个元素全部写过一遍（各自先填偏置）才 set_len
    unsafe {
        if cg == 1 {
            depthwise(x, w, bias, g, groups, dst);
        } else if g.k == (1, 1) && g.stride == (1, 1) && g.pad == (0, 0) {
            pointwise(x, w, bias, groups, dst);
        } else {
            im2col_gemm(x, w, bias, g, groups, dst);
        }
        out.set_len(o * oh * ow);
    }
    Tensor::new(vec![o, oh, ow], out)
}

/// 每组一个输入通道：输出通道 oc 只看输入通道 oc / og。按输出通道多线程。
///
/// # Safety
/// `out` 指向 o·oh·ow 个可写元素，调用期间别处不访问。
unsafe fn depthwise(
    x: &Tensor,
    w: &Tensor,
    bias: Option<&[f32]>,
    g: Geom,
    groups: usize,
    out: SyncPtr,
) {
    let o = w.shape[0];
    let og = o / groups;
    let (h, wd) = g.input;
    let (oh, ow) = g.output;
    let (kh, kw) = g.k;
    let plane = oh * ow;
    let threads = par::threads_for(o * plane * kh * kw, o);
    par::for_each(
        o,
        threads,
        || (),
        |_, oc| {
            let xc = &x.data[(oc / og) * h * wd..(oc / og + 1) * h * wd];
            let wk = &w.data[oc * kh * kw..(oc + 1) * kh * kw];
            let v = bias.map_or(0.0, |b| b[oc]);
            // SAFETY：通道 oc 的平面只属于这一项；先逐个写入偏置，再当作已初始化的切片用
            let dst = unsafe {
                let p = out.get().add(oc * plane);
                for i in 0..plane {
                    p.add(i).write(v);
                }
                std::slice::from_raw_parts_mut(p, plane)
            };
            depthwise_plane(xc, wk, g, dst);
        },
    );
}

/// 单通道卷积：对每个输出行、每个核位置，把一段输入行乘权重加到输出行上。
/// 累加次序与按定义的循环相同（先偏置，再按 ky、kx）。
fn depthwise_plane(xc: &[f32], wk: &[f32], g: Geom, dst: &mut [f32]) {
    let (h, wd) = g.input;
    let (oh, ow) = g.output;
    let (kh, kw) = g.k;
    let (sy, sx) = g.stride;
    let offs: Vec<(isize, usize, usize)> = (0..kw)
        .map(|kx| {
            let off = (kx * g.dilation.1) as isize - g.pad.1 as isize;
            let (lo, hi) = valid_cols(ow, sx, off, wd);
            (off, lo, hi)
        })
        .collect();
    for (oy, orow) in dst.chunks_exact_mut(ow).enumerate().take(oh) {
        for ky in 0..kh {
            let iy = (oy * sy + ky * g.dilation.0) as isize - g.pad.0 as isize;
            if iy < 0 || iy >= h as isize {
                continue;
            }
            let irow = &xc[iy as usize * wd..(iy as usize + 1) * wd];
            for (kx, &(off, lo, hi)) in offs.iter().enumerate() {
                let wv = wk[ky * kw + kx];
                if lo >= hi {
                    continue;
                }
                if sx == 1 {
                    let s = (lo as isize + off) as usize;
                    for (o, &v) in orow[lo..hi].iter_mut().zip(&irow[s..s + hi - lo]) {
                        *o += wv * v;
                    }
                } else {
                    for (ox, o) in orow.iter_mut().enumerate().take(hi).skip(lo) {
                        *o += wv * irow[((ox * sx) as isize + off) as usize];
                    }
                }
            }
        }
    }
}

/// 1×1、步长 1、无补边：每组一次矩阵乘 `[og, cg] · [cg, H·W]`。
///
/// # Safety
/// `out` 指向 o·H·W 个可写元素，调用期间别处不访问。
unsafe fn pointwise(x: &Tensor, w: &Tensor, bias: Option<&[f32]>, groups: usize, out: SyncPtr) {
    let o = w.shape[0];
    let cg = w.shape[1];
    let og = o / groups;
    let hw = x.data.len() / x.shape[0];
    for gi in 0..groups {
        // SAFETY：第 gi 组的输出行 [gi·og, (gi+1)·og) 在 out 范围内，组与组不重叠
        unsafe {
            gemm::gemm(
                kernel::best(),
                (og, hw, cg),
                &w.data[gi * og * cg..(gi + 1) * og * cg],
                cg,
                &x.data[gi * cg * hw..(gi + 1) * cg * hw],
                hw,
                out.get().add(gi * og * hw),
                hw,
                Init::Fill(bias.map(|b| &b[gi * og..])),
            )
        };
    }
}

/// [策略] im2col 每段最多的输出像素：段的列矩阵 K×段长，K ≤ 1152（128 通道 3×3）时
/// 不超过 1152×256×4 B ≈ 1.2 MB，在 L3 里；同时是 NR 的倍数，矩阵乘的微面板整齐。
const MAX_SEG: usize = 256;
/// [策略] 为凑够线程数缩短段时的下限（两个微面板宽）。
const MIN_SEG: usize = 2 * NR;
/// [策略] 每线程平均分到的段数，动态领取时尾部更均衡。
const SEGS_PER_THREAD: usize = 2;

/// 通用卷积：输出像素切成段，每段先把对应的输入取成列矩阵 `[cg·kh·kw, 段长]`，
/// 再乘本组权重 `[og, cg·kh·kw]` 加到输出上。不同 (组, 段) 多线程。
///
/// # Safety
/// `out` 指向 o·oh·ow 个可写元素，调用期间别处不访问。
unsafe fn im2col_gemm(
    x: &Tensor,
    w: &Tensor,
    bias: Option<&[f32]>,
    g: Geom,
    groups: usize,
    out: SyncPtr,
) {
    let o = w.shape[0];
    let cg = w.shape[1];
    let og = o / groups;
    let (kh, kw) = g.k;
    let kdim = cg * kh * kw;
    let (oh, ow) = g.output;
    let plane = oh * ow;
    let work = o * plane * kdim;
    let threads = par::threads_for(work, usize::MAX);
    let want = threads * SEGS_PER_THREAD;
    let mut seg = MAX_SEG.min(plane);
    while groups * plane.div_ceil(seg) < want && seg > MIN_SEG {
        seg = (seg / 2).next_multiple_of(NR).max(MIN_SEG);
    }
    let nseg = plane.div_ceil(seg);
    let threads = threads.min(groups * nseg);
    par::for_each(groups * nseg, threads, Scratch::take, |s, item| {
        let (gi, si) = (item / nseg, item % nseg);
        let p0 = si * seg;
        let n = seg.min(plane - p0);
        let Scratch { packs, cols } = &mut **s;
        im2col(x, gi * cg, cg, g, p0, n, cols);
        // SAFETY：本组输出通道 [gi·og, (gi+1)·og) 的像素 [p0, p0+n) 只属于这一项；
        // 其最远处 (og−1)·plane + n 不超过 out 的长度
        unsafe {
            gemm::gemm_serial(
                (og, n, kdim),
                &w.data[gi * og * kdim..(gi + 1) * og * kdim],
                kdim,
                cols,
                n,
                out.get().add(gi * og * plane + p0),
                plane,
                Init::Fill(bias.map(|b| &b[gi * og..])),
                packs,
            )
        };
    });
}

/// 把输入通道 `[c0, c0+cg)` 对输出像素 `[p0, p0+n)` 取成列矩阵：第 (ic·kh + ky)·kw + kx 行
/// 是各像素在该核位置读到的输入，越界读 0。
fn im2col(x: &Tensor, c0: usize, cg: usize, g: Geom, p0: usize, n: usize, cols: &mut Vec<f32>) {
    let (h, wd) = g.input;
    let ow = g.output.1;
    let (kh, kw) = g.k;
    let (sy, sx) = g.stride;
    cols.clear();
    cols.resize(cg * kh * kw * n, 0.0);
    let (oy0, oy1) = (p0 / ow, (p0 + n - 1) / ow);
    for ic in 0..cg {
        let xc = &x.data[(c0 + ic) * h * wd..(c0 + ic + 1) * h * wd];
        for ky in 0..kh {
            for kx in 0..kw {
                let r = (ic * kh + ky) * kw + kx;
                let dst = &mut cols[r * n..(r + 1) * n];
                let off = (kx * g.dilation.1) as isize - g.pad.1 as isize;
                let (lo, hi) = valid_cols(ow, sx, off, wd);
                for oy in oy0..=oy1 {
                    let iy = (oy * sy + ky * g.dilation.0) as isize - g.pad.0 as isize;
                    if iy < 0 || iy >= h as isize {
                        continue;
                    }
                    let irow = &xc[iy as usize * wd..(iy as usize + 1) * wd];
                    // 本行落在段内的输出列，再与有效列取交
                    let a = (p0.max(oy * ow) - oy * ow).max(lo);
                    let b = ((p0 + n).min((oy + 1) * ow) - oy * ow).min(hi);
                    if a >= b {
                        continue;
                    }
                    // 段内下标 = 像素号 − p0；a ≥ p0 − oy·ow，所以不会是负数
                    let d = &mut dst[oy * ow + a - p0..oy * ow + b - p0];
                    if sx == 1 {
                        let s = (a as isize + off) as usize;
                        d.copy_from_slice(&irow[s..s + b - a]);
                    } else {
                        for (i, v) in d.iter_mut().enumerate() {
                            *v = irow[(((a + i) * sx) as isize + off) as usize];
                        }
                    }
                }
            }
        }
    }
}

/// 转置卷积（groups = 1，无输出补边）：先算 `cols[(oc, ky, kx), (iy, ix)] = Σ_c w[c, oc, ky, kx]·x[c, iy, ix]`
/// （一次矩阵乘），再按输出通道多线程把各列加到对应输出位置。
pub(super) fn conv_transpose2d(
    x: &Tensor,
    w: &Tensor,
    bias: Option<&[f32]>,
    stride: (usize, usize),
    pad: (usize, usize),
    (oh, ow): (usize, usize),
) -> Tensor {
    let (c, h, wd) = x.chw();
    let (o, kh, kw) = (w.shape[1], w.shape[2], w.shape[3]);
    let rows = o * kh * kw;
    let hw = h * wd;
    // 权重看作 [C, O·kh·kw]，转置成 [O·kh·kw, C] 作矩阵乘的左边
    let wt = super::transpose(&Tensor::new(vec![c, rows], w.data.clone()));
    let cols = gemm::gemm_new((rows, hw, c), &wt.data, c, &x.data, hw, None);
    let mut out = biased_output(o, oh * ow, bias);
    let threads = par::threads_for(rows * hw, o);
    par::for_chunks_mut(&mut out, oh * ow, threads, |oc, dst| {
        for ky in 0..kh {
            for kx in 0..kw {
                let src = &cols[((oc * kh + ky) * kw + kx) * hw..][..hw];
                let off = kx as isize - pad.1 as isize;
                for iy in 0..h {
                    let oy = (iy * stride.0 + ky) as isize - pad.0 as isize;
                    if oy < 0 || oy >= oh as isize {
                        continue;
                    }
                    let orow = &mut dst[oy as usize * ow..(oy as usize + 1) * ow];
                    for (ix, &v) in src[iy * wd..(iy + 1) * wd].iter().enumerate() {
                        let ox = (ix * stride.1) as isize + off;
                        if ox >= 0 && ox < ow as isize {
                            orow[ox as usize] += v;
                        }
                    }
                }
            }
        }
    });
    Tensor::new(vec![o, oh, ow], out)
}
