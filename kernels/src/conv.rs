//! 卷积用的两个内核：im2col（之后接 SGEMM）和深度卷积的直接内核。
//! 语义同 `pagesift_infer::ops::conv2d`：`x` 是 `[C, H, W]`，权重 `[O, C/groups, kh, kw]`，
//! 输出 `[O, OH, OW]`；补边是 0，stride、pad、dilation 按 (行, 列)。
//!
//! 下标拆分（除法、取余）全用 u32：GPU 没有整数除法指令，64 位除法要展开成上百条指令，
//! 32 位的短得多（1070 上实测深度卷积 3×3、7×7 都快约 17%）。宿主保证元素总数 < 2³²。

use crate::global_index;

/// 卷积几何参数。宿主按内核签名的顺序逐个传 u32，内核里收拢成这个结构。
struct Geom {
    h: u32,
    w: u32,
    kh: u32,
    kw: u32,
    sh: u32,
    sw: u32,
    ph: u32,
    pw: u32,
    dh: u32,
    dw: u32,
    oh: u32,
    ow: u32,
}

impl Geom {
    /// 输出位置 (oy, ox) 对应核位置 (ky, kx) 在本通道里的输入下标；落在补边里返回 None。
    #[inline(always)]
    fn input_at(&self, oy: u32, ox: u32, ky: u32, kx: u32) -> Option<usize> {
        let iy = (oy * self.sh + ky * self.dh).wrapping_sub(self.ph);
        let ix = (ox * self.sw + kx * self.dw).wrapping_sub(self.pw);
        // 减成负数时 wrapping 成极大值，一次比较同时排除两侧越界
        if iy < self.h && ix < self.w {
            Some((iy * self.w + ix) as usize)
        } else {
            None
        }
    }

    #[inline(always)]
    fn plane(&self) -> usize {
        self.h as usize * self.w as usize
    }
}

/// 展开：`col[(c·kh + ky)·kw + kx, oy·OW + ox] = x[c, iy, ix]`（补边处为 0），
/// 共 `C·kh·kw · OH·OW` 个元素，每线程一个；相邻线程写相邻的输出列。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn im2col(
    x: *const f32,
    col: *mut f32,
    c: u32,
    h: u32,
    w: u32,
    kh: u32,
    kw: u32,
    sh: u32,
    sw: u32,
    ph: u32,
    pw: u32,
    dh: u32,
    dw: u32,
    oh: u32,
    ow: u32,
) {
    let g = Geom {
        h,
        w,
        kh,
        kw,
        sh,
        sw,
        ph,
        pw,
        dh,
        dw,
        oh,
        ow,
    };
    let ohw = g.oh * g.ow;
    let i = global_index();
    if i >= c as u64 * (g.kh * g.kw * ohw) as u64 {
        return;
    }
    let i = i as u32;
    let (r, pos) = (i / ohw, i % ohw);
    let (oy, ox) = (pos / g.ow, pos % g.ow);
    let kx = r % g.kw;
    let ky = (r / g.kw) % g.kh;
    let ch = r / (g.kw * g.kh);
    *col.add(i as usize) = match g.input_at(oy, ox, ky, kx) {
        Some(at) => *x.add(ch as usize * g.plane() + at),
        None => 0.0,
    };
}

/// 每组只有一个输入通道（`C/groups = 1`，深度卷积及其通道倍增形式）的直接卷积。
/// 每线程算一个输出元素：输出通道 `oc` 读输入通道 `oc / og`（`og = O/groups`），
/// 先放偏置（`has_bias` 为 0 时放 0），再按 ky、kx 的顺序累加，与 CPU 参考实现的顺序相同。
/// 批量时 `o` 是 N·O、`wrap` 是一个样本的输出通道数 O：权重和偏置按 `oc % wrap` 取，各样本共用。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn depthwise(
    x: *const f32,
    wt: *const f32,
    bias: *const f32,
    out: *mut f32,
    o: u32,
    og: u32,
    wrap: u32,
    has_bias: u32,
    h: u32,
    w: u32,
    kh: u32,
    kw: u32,
    sh: u32,
    sw: u32,
    ph: u32,
    pw: u32,
    dh: u32,
    dw: u32,
    oh: u32,
    ow: u32,
) {
    let g = Geom {
        h,
        w,
        kh,
        kw,
        sh,
        sw,
        ph,
        pw,
        dh,
        dw,
        oh,
        ow,
    };
    let ohw = g.oh * g.ow;
    let i = global_index();
    if i >= o as u64 * ohw as u64 {
        return;
    }
    let i = i as u32;
    let (oc, pos) = (i / ohw, i % ohw);
    let (oy, ox) = (pos / g.ow, pos % g.ow);
    let xc = x.add((oc / og) as usize * g.plane());
    let oc1 = oc % wrap;
    let wc = wt.add(oc1 as usize * (g.kh * g.kw) as usize);
    let mut s = if has_bias != 0 {
        *bias.add(oc1 as usize)
    } else {
        0.0
    };
    for ky in 0..g.kh {
        for kx in 0..g.kw {
            if let Some(at) = g.input_at(oy, ox, ky, kx) {
                s += *xc.add(at) * *wc.add((ky * g.kw + kx) as usize);
            }
        }
    }
    *out.add(i as usize) = s;
}

/// 转置卷积（`F.conv_transpose2d`，groups = 1，无输出补边）。权重 `[C, O, kh, kw]`，输出
/// `[N·O, OH, OW]`。每线程一个输出元素：沿每个核位置反推输入坐标，整除才有贡献。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn conv_transpose(
    x: *const f32,
    wt: *const f32,
    bias: *const f32,
    out: *mut f32,
    c: u32,
    o: u32,
    has_bias: u32,
    h: u32,
    w: u32,
    kh: u32,
    kw: u32,
    sh: u32,
    sw: u32,
    ph: u32,
    pw: u32,
    oh: u32,
    ow: u32,
    total: u32,
) {
    let plane = oh * ow;
    let i = global_index();
    if i >= total as u64 {
        return;
    }
    let i = i as u32;
    let (oct, pos) = (i / plane, i % plane);
    let (z, oc) = (oct / o, oct % o);
    let (oy, ox) = (pos / ow, pos % ow);
    let mut s = if has_bias != 0 {
        *bias.add(oc as usize)
    } else {
        0.0
    };
    for ky in 0..kh {
        let dy = (oy + ph).wrapping_sub(ky);
        if dy > oy + ph || dy % sh != 0 || dy / sh >= h {
            continue;
        }
        let iy = dy / sh;
        for kx in 0..kw {
            let dx = (ox + pw).wrapping_sub(kx);
            if dx > ox + pw || dx % sw != 0 || dx / sw >= w {
                continue;
            }
            let at = iy * w + dx / sw;
            for ic in 0..c {
                let v = *x.add((((z * c + ic) * h * w) + at) as usize);
                let k = *wt.add((((ic * o + oc) * kh + ky) * kw + kx) as usize);
                s += v * k;
            }
        }
    }
    *out.add(i as usize) = s;
}
