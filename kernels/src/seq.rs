//! 序列部分的内核：LayerNorm、自注意力的拆头和掩码 softmax、CTC 摘要。
//! 张量是 `[N, T, C]` 行优先；T 是一批里最长的序列，每个样本的实际长度在 `lens` 里。

use core::f32::math::sqrt;

use crate::global_index;
use crate::math::expf;

/// `nn.LayerNorm`，作用在最后一维。每线程一行，累加顺序同 CPU 版。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn layer_norm(
    x: *const f32,
    out: *mut f32,
    wt: *const f32,
    bias: *const f32,
    eps: f32,
    c: u32,
    rows: u32,
) {
    let r = global_index();
    if r >= rows as u64 {
        return;
    }
    let at = r as usize * c as usize;
    let (row, dst) = (x.add(at), out.add(at));
    let (c, cf) = (c as usize, c as f32);
    let mut sum = 0f32;
    for i in 0..c {
        sum += *row.add(i);
    }
    let mean = sum / cf;
    let mut var = 0f32;
    for i in 0..c {
        let d = *row.add(i) - mean;
        var += d * d;
    }
    let inv = 1.0 / sqrt(var / cf + eps);
    for i in 0..c {
        *dst.add(i) = (*row.add(i) - mean) * inv * *wt.add(i) + *bias.add(i);
    }
}

/// 把线性层出来的 `[N, T, 3C]`（列 = s·C + h·hd + d，s 为 0 = q、1 = k、2 = v）拆成每头一个矩阵：
/// `q`、`v` 是 `[N·H, T, hd]`，`kt` 是 k 的转置 `[N·H, hd, T]`。每线程搬一个 (头, t, d) 的三个数。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn attn_split(
    qkv: *const f32,
    q: *mut f32,
    kt: *mut f32,
    v: *mut f32,
    t: u32,
    heads: u32,
    hd: u32,
    total: u32,
) {
    let i = global_index();
    if i >= total as u64 {
        return;
    }
    let i = i as u32;
    let per = t * hd;
    let (nh, r) = (i / per, i % per);
    let (tt, d) = (r / hd, r % hd);
    let (z, h) = (nh / heads, nh % heads);
    let c = heads * hd;
    let row = ((z * t + tt) as usize) * (3 * c) as usize;
    let col = (h * hd + d) as usize;
    let c = c as usize;
    *q.add(i as usize) = *qkv.add(row + col);
    *kt.add((nh * per + d * t + tt) as usize) = *qkv.add(row + c + col);
    *v.add(i as usize) = *qkv.add(row + 2 * c + col);
}

/// 注意力分数 `a` 是 `[N·H, T, T]`：先乘 `scale`，每行只在该样本的前 `lens[z]` 列上做 softmax，
/// 其余列（和超出长度的行）写 0，这样后面乘 v 时补边的 token 没有影响。每线程一行。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn attn_softmax(
    a: *mut f32,
    lens: *const u32,
    scale: f32,
    t: u32,
    heads: u32,
    rows: u32,
) {
    let r = global_index();
    if r >= rows as u64 {
        return;
    }
    let r = r as u32;
    let (nh, tt) = (r / t, r % t);
    let len = *lens.add((nh / heads) as usize);
    let row = a.add(r as usize * t as usize);
    let (t, len) = (t as usize, if tt < len { len as usize } else { 0 });
    let mut m = f32::NEG_INFINITY;
    for j in 0..len {
        let v = *row.add(j) * scale;
        *row.add(j) = v;
        m = m.max(v);
    }
    let mut s = 0f32;
    for j in 0..len {
        let e = expf(*row.add(j) - m);
        *row.add(j) = e;
        s += e;
    }
    for j in 0..t {
        *row.add(j) = if j < len { *row.add(j) / s } else { 0.0 };
    }
}

/// 把每头的 `o`（`[N·H, T, hd]`）拼回 `[N, T, C]`：列 = h·hd + d。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn attn_merge(
    o: *const f32,
    out: *mut f32,
    t: u32,
    heads: u32,
    hd: u32,
    total: u32,
) {
    let i = global_index();
    if i >= total as u64 {
        return;
    }
    let i = i as u32;
    let c = heads * hd;
    let per = t * c;
    let (z, r) = (i / per, i % per);
    let (tt, col) = (r / c, r % c);
    let (h, d) = (col / hd, col % hd);
    *out.add(i as usize) = *o.add((((z * heads + h) * t + tt) * hd + d) as usize);
}

/// CTC 摘要：每行（logits `[rows, n]`）写两个数：最大类的下标（u32 的位）和 1 / Σ exp(xᵢ − max)
/// （softmax 的最大概率），回传它们比回传整行少两到三个数量级。每线程一行，顺序同 CPU 版。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn ctc_digest(x: *const f32, out: *mut f32, n: u32, rows: u32) {
    let r = global_index();
    if r >= rows as u64 {
        return;
    }
    let r = r as usize;
    let n = n as usize;
    let row = x.add(r * n);
    let (mut arg, mut m) = (0u32, f32::NEG_INFINITY);
    for i in 0..n {
        let v = *row.add(i);
        if v > m {
            arg = i as u32;
            m = v;
        }
    }
    let mut z = 0f32;
    for i in 0..n {
        z += expf(*row.add(i) - m);
    }
    *out.add(2 * r) = f32::from_bits(arg);
    *out.add(2 * r + 1) = 1.0 / z;
}
