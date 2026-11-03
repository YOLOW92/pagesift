//! 显存里连续执行时用的形状类内核：补边、池化、拼接、上采样、遮罩等。语义与
//! `pagesift_infer::ops` 的同名算子一致。
//!
//! 批量约定：`[N, C, H, W]` 按行优先连续存放，把它看成 `[N·C, H, W]` 时内核不用知道 N；
//! 要区分样本的内核（遮罩、全局平均）另外收 `widths`：每个样本的有效列数（u32，设备地址）。
//! 每线程算一个输出元素，下标拆分全用 u32（见 `conv.rs` 开头）。

use crate::global_index;

/// 把每个样本有效宽度以外的列清零：`x` 是 `[N, ..., W]`，每个样本 `per_sample` 个元素。
/// 这样下一层卷积读到的和单张推理时的补边 0 完全一样。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn zero_cols(
    x: *mut f32,
    widths: *const u32,
    per_sample: u32,
    w: u32,
    n: u32,
) {
    let i = global_index();
    if i >= n as u64 {
        return;
    }
    let i = i as u32;
    if i % w >= *widths.add((i / per_sample) as usize) {
        *x.add(i as usize) = 0.0;
    }
}

/// `F.pad(x, (0, nw − w, 0, nh − h))`：`x` 是 `[c, h, w]`，`out` 是 `[c, nh, nw]`。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn pad_rb(
    x: *const f32,
    out: *mut f32,
    c: u32,
    h: u32,
    w: u32,
    nh: u32,
    nw: u32,
) {
    let plane = nh * nw;
    let i = global_index();
    if i >= c as u64 * plane as u64 {
        return;
    }
    let i = i as u32;
    let (ch, pos) = (i / plane, i % plane);
    let (y, col) = (pos / nw, pos % nw);
    *out.add(i as usize) = if y < h && col < w {
        *x.add(((ch * h + y) * w + col) as usize)
    } else {
        0.0
    };
}

/// 最大池化（无补边）：窗口越界的部分不算，同 `F.max_pool2d(ceil_mode=True)` 的界内取值。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn pool_max(
    x: *const f32,
    out: *mut f32,
    c: u32,
    h: u32,
    w: u32,
    kh: u32,
    kw: u32,
    sh: u32,
    sw: u32,
    oh: u32,
    ow: u32,
) {
    let plane = oh * ow;
    let i = global_index();
    if i >= c as u64 * plane as u64 {
        return;
    }
    let i = i as u32;
    let (ch, pos) = (i / plane, i % plane);
    let (oy, ox) = (pos / ow, pos % ow);
    let xc = x.add((ch * h * w) as usize);
    let mut m = f32::NEG_INFINITY;
    for ky in 0..kh {
        let iy = oy * sh + ky;
        if iy >= h {
            break;
        }
        for kx in 0..kw {
            let ix = ox * sw + kx;
            if ix >= w {
                break;
            }
            m = m.max(*xc.add((iy * w + ix) as usize));
        }
    }
    *out.add(i as usize) = m;
}

/// 平均池化（步长等于核大小、无补边、向下取整），累加顺序同 CPU 版。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn pool_avg(
    x: *const f32,
    out: *mut f32,
    c: u32,
    h: u32,
    w: u32,
    kh: u32,
    kw: u32,
    oh: u32,
    ow: u32,
) {
    let plane = oh * ow;
    let i = global_index();
    if i >= c as u64 * plane as u64 {
        return;
    }
    let i = i as u32;
    let (ch, pos) = (i / plane, i % plane);
    let (oy, ox) = (pos / ow, pos % ow);
    let mut s = 0f32;
    for ky in 0..kh {
        let row = (ch * h + oy * kh + ky) * w + ox * kw;
        for kx in 0..kw {
            s += *x.add((row + kx) as usize);
        }
    }
    *out.add(i as usize) = s / (kh * kw) as f32;
}

/// 每个通道的全局平均，只算各样本的有效列。`x` 是 `[N, C, h, w]`，`out` 是 `[N, C]`。
/// 每线程一个输出通道，整张平面的累加都在这个线程里，顺序同 CPU 版。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn global_avg(
    x: *const f32,
    out: *mut f32,
    widths: *const u32,
    c: u32,
    h: u32,
    w: u32,
    total: u32,
) {
    let i = global_index();
    if i >= total as u64 {
        return;
    }
    let i = i as u32;
    let valid = *widths.add((i / c) as usize);
    let plane = x.add(i as usize * (h * w) as usize);
    let mut s = 0f32;
    for y in 0..h {
        for col in 0..valid {
            s += *plane.add((y * w + col) as usize);
        }
    }
    *out.add(i as usize) = s / (h * valid) as f32;
}

/// `x[c, i] *= s[c]`：`x` 是 `[C, hw]`（C 含批次），共 `n = C·hw` 个元素。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn scale_ch(x: *mut f32, s: *const f32, hw: u32, n: u32) {
    let i = global_index();
    if i < n as u64 {
        let i = i as u32;
        *x.add(i as usize) *= *s.add((i / hw) as usize);
    }
}

/// `out = a + b`，`out` 可以就是 `a`。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn add2(a: *const f32, b: *const f32, out: *mut f32, n: u32) {
    let i = global_index();
    if i < n as u64 {
        let i = i as usize;
        *out.add(i) = *a.add(i) + *b.add(i);
    }
}

/// 拼通道用：对每个样本 z，把 `src` 第 z 段（长 `len`）拷到 `dst + z·dstride`。
/// `src + z·len` 就是第 z 段，所以只要 `dstride`。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn copy_part(
    src: *const f32,
    dst: *mut f32,
    len: u32,
    dstride: u32,
    n: u32,
) {
    let i = global_index();
    if i < n as u64 {
        let i = i as u32;
        let (z, j) = (i / len, i % len);
        *dst.add((z * dstride + j) as usize) = *src.add(i as usize);
    }
}

/// 最近邻插值（`F.interpolate(mode="nearest")`：源坐标 = ⌊目标坐标 · 输入/输出⌋）。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn interp_nearest(
    x: *const f32,
    out: *mut f32,
    c: u32,
    h: u32,
    w: u32,
    oh: u32,
    ow: u32,
) {
    let plane = oh * ow;
    let i = global_index();
    if i >= c as u64 * plane as u64 {
        return;
    }
    let i = i as u32;
    let (ch, pos) = (i / plane, i % plane);
    let (oy, ox) = (pos / ow, pos % ow);
    let iy = oy * h / oh;
    let ix = ox * w / ow;
    *out.add(i as usize) = *x.add(((ch * h + iy) * w + ix) as usize);
}

/// `[N, C, hw]` → `[N, hw, C]`（`flatten(2).transpose(1, 2)`）。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn chw_to_seq(
    x: *const f32,
    out: *mut f32,
    c: u32,
    hw: u32,
    n: u32,
) {
    let i = global_index();
    if i >= n as u64 {
        return;
    }
    let i = i as u32;
    let per = c * hw;
    let (z, r) = (i / per, i % per);
    let (t, cc) = (r / c, r % c);
    *out.add(i as usize) = *x.add(((z * c + cc) * hw + t) as usize);
}

/// 四周补 0：`x` 是 `[c, h, w]`，`out` 是 `[c, nh, nw]`，原图放在 (ph, pw) 处。
/// 隐式卷积用：窗口在补边后的输入上按指针偏移取。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn pad_all(
    x: *const f32,
    out: *mut f32,
    c: u32,
    h: u32,
    w: u32,
    ph: u32,
    pw: u32,
    nh: u32,
    nw: u32,
) {
    let plane = nh * nw;
    let i = global_index();
    if i >= c as u64 * plane as u64 {
        return;
    }
    let i = i as u32;
    let (ch, pos) = (i / plane, i % plane);
    let (y, col) = (pos / nw, pos % nw);
    // 减成负数时 wrapping 成极大值，一次比较同时排除两侧
    let (iy, ix) = (y.wrapping_sub(ph), col.wrapping_sub(pw));
    *out.add(i as usize) = if iy < h && ix < w {
        *x.add(((ch * h + iy) * w + ix) as usize)
    } else {
        0.0
    };
}

/// 隐式卷积的收尾：`src` 每个通道 `cols` 个元素、行距 `nw`；`dst` 是紧排的 `[C, oh, ow]`，共 `n` 个元素。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn crop_cols(
    src: *const f32,
    dst: *mut f32,
    cols: u32,
    nw: u32,
    oh: u32,
    ow: u32,
    n: u32,
) {
    let i = global_index();
    if i < n as u64 {
        let i = i as u32;
        let (r, col) = (i / ow, i % ow);
        let (ch, y) = (r / oh, r % oh);
        *dst.add(i as usize) = *src.add((ch * cols + y * nw + col) as usize);
    }
}
