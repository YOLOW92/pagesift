//! WebP 有损（VP8 关键帧，RFC 6386）：只为 OCR 解亮度平面。色度的系数要读（共用熵状态），不重建；
//! 环路滤波不做：帧内预测用的是滤波前的像素，不影响后面的解码，文字识别也不需要去块效应。

use super::tables::{
    AC_TABLE, BANDS, BMODES_PROBA, COEFFS_PROBA0, COEFFS_UPDATE, DC_TABLE, YMODES_INTRA4,
};

const BPS: usize = 32;
const DC_PRED: u8 = 0;
const TM_PRED: u8 = 1;
const V_PRED: u8 = 2;
const H_PRED: u8 = 3;
const B_PRED: u8 = 10;
const ZIGZAG: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];
const CAT3: &[u8] = &[173, 148, 140];
const CAT4: &[u8] = &[176, 155, 140, 135];
const CAT5: &[u8] = &[180, 157, 141, 134, 130];
const CAT6: &[u8] = &[254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129];

/// 布尔解码器（RFC 6386 §7）。读过末尾按 0 算。
struct Bool<'a> {
    data: &'a [u8],
    pos: usize,
    value: u64,
    range: u32,
    bits: i32,
}

impl<'a> Bool<'a> {
    fn new(data: &'a [u8]) -> Self {
        let mut b = Bool {
            data,
            pos: 0,
            value: 0,
            range: 255,
            bits: -8,
        };
        b.load();
        b
    }

    #[inline]
    fn load(&mut self) {
        while self.bits < 0 {
            let byte = self.data.get(self.pos).copied().unwrap_or(0);
            self.pos += 1;
            self.value = self.value << 8 | u64::from(byte);
            self.bits += 8;
        }
    }

    #[inline]
    fn bit(&mut self, prob: u8) -> bool {
        let split = 1 + (((self.range - 1) * u32::from(prob)) >> 8);
        let big = u64::from(split) << self.bits;
        let set = self.value >= big;
        if set {
            self.value -= big;
            self.range -= split;
        } else {
            self.range = split;
        }
        let shift = self.range.leading_zeros() as i32 - 24;
        self.range <<= shift;
        self.bits -= shift;
        if self.bits < 0 {
            self.load();
        }
        set
    }

    fn literal(&mut self, n: u32) -> u32 {
        (0..n).fold(0, |v, _| v << 1 | u32::from(self.bit(128)))
    }

    fn flag(&mut self) -> bool {
        self.bit(128)
    }

    fn signed(&mut self, n: u32) -> i32 {
        let v = self.literal(n) as i32;
        if self.flag() { -v } else { v }
    }

    fn optional(&mut self, n: u32) -> i32 {
        if self.flag() { self.signed(n) } else { 0 }
    }

    fn overrun(&self) -> bool {
        self.pos > self.data.len() + 16
    }
}

type Probs = [[[[u8; 11]; 3]; 8]; 4];

#[derive(Clone, Copy, Default)]
struct Quant {
    y1: [i32; 2],
    y2: [i32; 2],
    uv: [i32; 2],
}

fn clip_q(v: i32, max: i32) -> usize {
    v.clamp(0, max) as usize
}

#[inline]
fn clip8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

fn large_value(b: &mut Bool<'_>, p: &[u8; 11]) -> i32 {
    if !b.bit(p[3]) {
        if !b.bit(p[4]) {
            2
        } else {
            3 + i32::from(b.bit(p[5]))
        }
    } else if !b.bit(p[6]) {
        if !b.bit(p[7]) {
            5 + i32::from(b.bit(159))
        } else {
            let v = 7 + 2 * i32::from(b.bit(165));
            v + i32::from(b.bit(145))
        }
    } else {
        let bit1 = usize::from(b.bit(p[8]));
        let bit0 = usize::from(b.bit(p[9 + bit1]));
        let cat = 2 * bit1 + bit0;
        let table = [CAT3, CAT4, CAT5, CAT6][cat];
        let mut v = 0;
        for &t in table {
            v = v + v + i32::from(b.bit(t));
        }
        v + 3 + (8 << cat)
    }
}

/// 读一个块的系数，返回最后一个非零系数之后的位置。
fn coeffs(
    b: &mut Bool<'_>,
    probs: &[[[u8; 11]; 3]; 8],
    ctx: usize,
    dq: &[i32; 2],
    first: usize,
    out: &mut [i32],
) -> usize {
    let mut n = first;
    let mut p = &probs[BANDS[n]][ctx];
    while n < 16 {
        if !b.bit(p[0]) {
            return n;
        }
        while !b.bit(p[1]) {
            n += 1;
            if n == 16 {
                return 16;
            }
            p = &probs[BANDS[n]][0];
        }
        let next = &probs[BANDS[n + 1]];
        let v = if !b.bit(p[2]) {
            p = &next[1];
            1
        } else {
            let v = large_value(b, p);
            p = &next[2];
            v
        };
        let v = if b.bit(128) { -v } else { v };
        out[ZIGZAG[n]] = v * dq[usize::from(n > 0)];
        n += 1;
    }
    16
}

fn inverse_wht(input: &[i32; 16], out: &mut [i32]) {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let a0 = input[i] + input[12 + i];
        let a1 = input[4 + i] + input[8 + i];
        let a2 = input[4 + i] - input[8 + i];
        let a3 = input[i] - input[12 + i];
        t[i] = a0 + a1;
        t[8 + i] = a0 - a1;
        t[4 + i] = a3 + a2;
        t[12 + i] = a3 - a2;
    }
    for i in 0..4 {
        let row = i * 4;
        let dc = t[row] + 3;
        let a0 = dc + t[3 + row];
        let a1 = t[1 + row] + t[2 + row];
        let a2 = t[1 + row] - t[2 + row];
        let a3 = dc - t[3 + row];
        out[row * 16] = (a0 + a1) >> 3;
        out[(row + 1) * 16] = (a3 + a2) >> 3;
        out[(row + 2) * 16] = (a0 - a1) >> 3;
        out[(row + 3) * 16] = (a3 - a2) >> 3;
    }
}

#[inline]
fn mul1(a: i32) -> i32 {
    ((i64::from(a) * 20091) >> 16) as i32 + a
}

#[inline]
fn mul2(a: i32) -> i32 {
    ((i64::from(a) * 35468) >> 16) as i32
}

/// 反变换后加到 `dst`（工作区里的 4×4 块）。
fn add_idct(c: &[i32], dst: &mut [u8], at: usize) {
    if c[..16].iter().all(|&v| v == 0) {
        return;
    }
    let mut t = [0i32; 16];
    for i in 0..4 {
        let a = c[i] + c[8 + i];
        let b = c[i] - c[8 + i];
        let cc = mul2(c[4 + i]) - mul1(c[12 + i]);
        let d = mul1(c[4 + i]) + mul2(c[12 + i]);
        t[i * 4] = a + d;
        t[i * 4 + 1] = b + cc;
        t[i * 4 + 2] = b - cc;
        t[i * 4 + 3] = a - d;
    }
    for i in 0..4 {
        let dc = t[i] + 4;
        let a = dc + t[8 + i];
        let b = dc - t[8 + i];
        let cc = mul2(t[4 + i]) - mul1(t[12 + i]);
        let d = mul1(t[4 + i]) + mul2(t[12 + i]);
        let row = at + i * BPS;
        for (x, v) in [a + d, b + cc, b - cc, a - d].into_iter().enumerate() {
            dst[row + x] = clip8(i32::from(dst[row + x]) + (v >> 3));
        }
    }
}

#[inline]
fn avg3(a: u8, b: u8, c: u8) -> u8 {
    ((u32::from(a) + 2 * u32::from(b) + u32::from(c) + 2) >> 2) as u8
}

#[inline]
fn avg2(a: u8, b: u8) -> u8 {
    ((u32::from(a) + u32::from(b) + 1) >> 1) as u8
}

/// 4×4 帧内预测。`at` 是块左上角在工作区里的下标。
fn predict4(w: &mut [u8], at: usize, mode: u8) {
    let top = |w: &[u8], i: usize| w[at - BPS + i];
    let left = |w: &[u8], i: usize| w[at + i * BPS - 1];
    let tl = w[at - BPS - 1];
    let (a, b, c, d) = (top(w, 0), top(w, 1), top(w, 2), top(w, 3));
    let (e, f, g, h) = (top(w, 4), top(w, 5), top(w, 6), top(w, 7));
    let (i, j, k, l) = (left(w, 0), left(w, 1), left(w, 2), left(w, 3));
    let x = tl;
    let mut set = |px: u8, py: u8, v: u8| w[at + usize::from(px) + usize::from(py) * BPS] = v;
    match mode {
        0 => {
            let s: u32 = [a, b, c, d, i, j, k, l].iter().map(|&v| u32::from(v)).sum();
            let v = ((s + 4) >> 3) as u8;
            for y in 0..4 {
                for xx in 0..4 {
                    set(xx, y, v);
                }
            }
        }
        1 => {
            for (y, lv) in [i, j, k, l].into_iter().enumerate() {
                for (xx, tv) in [a, b, c, d].into_iter().enumerate() {
                    set(
                        xx as u8,
                        y as u8,
                        clip8(i32::from(tv) + i32::from(lv) - i32::from(tl)),
                    );
                }
            }
        }
        2 => {
            let v = [avg3(x, a, b), avg3(a, b, c), avg3(b, c, d), avg3(c, d, e)];
            for y in 0..4 {
                for xx in 0..4u8 {
                    set(xx, y, v[usize::from(xx)]);
                }
            }
        }
        3 => {
            let v = [avg3(x, i, j), avg3(i, j, k), avg3(j, k, l), avg3(k, l, l)];
            for y in 0..4u8 {
                for xx in 0..4 {
                    set(xx, y, v[usize::from(y)]);
                }
            }
        }
        4 => {
            // RD
            set(0, 3, avg3(j, k, l));
            for (px, py) in [(1, 3), (0, 2)] {
                set(px, py, avg3(i, j, k));
            }
            for (px, py) in [(2, 3), (1, 2), (0, 1)] {
                set(px, py, avg3(x, i, j));
            }
            for (px, py) in [(3, 3), (2, 2), (1, 1), (0, 0)] {
                set(px, py, avg3(a, x, i));
            }
            for (px, py) in [(3, 2), (2, 1), (1, 0)] {
                set(px, py, avg3(b, a, x));
            }
            for (px, py) in [(3, 1), (2, 0)] {
                set(px, py, avg3(c, b, a));
            }
            set(3, 0, avg3(d, c, b));
        }
        5 => {
            // VR
            for (px, py) in [(0, 0), (1, 2)] {
                set(px, py, avg2(x, a));
            }
            for (px, py) in [(1, 0), (2, 2)] {
                set(px, py, avg2(a, b));
            }
            for (px, py) in [(2, 0), (3, 2)] {
                set(px, py, avg2(b, c));
            }
            set(3, 0, avg2(c, d));
            set(0, 3, avg3(k, j, i));
            set(0, 2, avg3(j, i, x));
            for (px, py) in [(0, 1), (1, 3)] {
                set(px, py, avg3(i, x, a));
            }
            for (px, py) in [(1, 1), (2, 3)] {
                set(px, py, avg3(x, a, b));
            }
            for (px, py) in [(2, 1), (3, 3)] {
                set(px, py, avg3(a, b, c));
            }
            set(3, 1, avg3(b, c, d));
        }
        6 => {
            // LD
            set(0, 0, avg3(a, b, c));
            for (px, py) in [(1, 0), (0, 1)] {
                set(px, py, avg3(b, c, d));
            }
            for (px, py) in [(2, 0), (1, 1), (0, 2)] {
                set(px, py, avg3(c, d, e));
            }
            for (px, py) in [(3, 0), (2, 1), (1, 2), (0, 3)] {
                set(px, py, avg3(d, e, f));
            }
            for (px, py) in [(3, 1), (2, 2), (1, 3)] {
                set(px, py, avg3(e, f, g));
            }
            for (px, py) in [(3, 2), (2, 3)] {
                set(px, py, avg3(f, g, h));
            }
            set(3, 3, avg3(g, h, h));
        }
        7 => {
            // VL
            set(0, 0, avg2(a, b));
            for (px, py) in [(1, 0), (0, 2)] {
                set(px, py, avg2(b, c));
            }
            for (px, py) in [(2, 0), (1, 2)] {
                set(px, py, avg2(c, d));
            }
            for (px, py) in [(3, 0), (2, 2)] {
                set(px, py, avg2(d, e));
            }
            set(0, 1, avg3(a, b, c));
            for (px, py) in [(1, 1), (0, 3)] {
                set(px, py, avg3(b, c, d));
            }
            for (px, py) in [(2, 1), (1, 3)] {
                set(px, py, avg3(c, d, e));
            }
            for (px, py) in [(3, 1), (2, 3)] {
                set(px, py, avg3(d, e, f));
            }
            set(3, 2, avg3(e, f, g));
            set(3, 3, avg3(f, g, h));
        }
        8 => {
            // HD
            for (px, py) in [(0, 0), (2, 1)] {
                set(px, py, avg2(i, x));
            }
            for (px, py) in [(0, 1), (2, 2)] {
                set(px, py, avg2(j, i));
            }
            for (px, py) in [(0, 2), (2, 3)] {
                set(px, py, avg2(k, j));
            }
            set(0, 3, avg2(l, k));
            set(3, 0, avg3(a, b, c));
            set(2, 0, avg3(x, a, b));
            for (px, py) in [(1, 0), (3, 1)] {
                set(px, py, avg3(i, x, a));
            }
            for (px, py) in [(1, 1), (3, 2)] {
                set(px, py, avg3(j, i, x));
            }
            for (px, py) in [(1, 2), (3, 3)] {
                set(px, py, avg3(k, j, i));
            }
            set(1, 3, avg3(l, k, j));
        }
        _ => {
            // HU
            set(0, 0, avg2(i, j));
            for (px, py) in [(2, 0), (0, 1)] {
                set(px, py, avg2(j, k));
            }
            for (px, py) in [(2, 1), (0, 2)] {
                set(px, py, avg2(k, l));
            }
            set(1, 0, avg3(i, j, k));
            for (px, py) in [(3, 0), (1, 1)] {
                set(px, py, avg3(j, k, l));
            }
            for (px, py) in [(3, 1), (1, 2)] {
                set(px, py, avg3(k, l, l));
            }
            for (px, py) in [(3, 2), (2, 2), (0, 3), (1, 3), (2, 3), (3, 3)] {
                set(px, py, l);
            }
        }
    }
}

fn predict16(w: &mut [u8], at: usize, mode: u8, has_left: bool, has_top: bool) {
    let topv: [u8; 16] = std::array::from_fn(|i| w[at - BPS + i]);
    let leftv: [u8; 16] = std::array::from_fn(|i| w[at + i * BPS - 1]);
    let tl = i32::from(w[at - BPS - 1]);
    let dc = |w: &mut [u8], v: u8| {
        for y in 0..16 {
            w[at + y * BPS..at + y * BPS + 16].fill(v);
        }
    };
    let sum = |a: &[u8; 16]| a.iter().map(|&v| u32::from(v)).sum::<u32>();
    match mode {
        V_PRED => {
            for y in 0..16 {
                w[at + y * BPS..at + y * BPS + 16].copy_from_slice(&topv);
            }
        }
        H_PRED => {
            for (y, v) in leftv.iter().enumerate() {
                w[at + y * BPS..at + y * BPS + 16].fill(*v);
            }
        }
        TM_PRED => {
            for (y, l) in leftv.iter().enumerate() {
                for (x, t) in topv.iter().enumerate() {
                    w[at + y * BPS + x] = clip8(i32::from(*t) + i32::from(*l) - tl);
                }
            }
        }
        _ => {
            let v = match (has_left, has_top) {
                (true, true) => (sum(&topv) + sum(&leftv) + 16) >> 5,
                (true, false) => (sum(&leftv) + 8) >> 4,
                (false, true) => (sum(&topv) + 8) >> 4,
                (false, false) => 0x80,
            };
            dc(w, v as u8);
        }
    }
}

/// 解关键帧的亮度平面。
pub(super) fn decode_luma(
    data: &[u8],
    check: &dyn Fn(u64, u64) -> Result<(), String>,
) -> Result<(Vec<u8>, usize, usize), String> {
    let head = data.get(..10).ok_or("WebP: truncated VP8 header")?;
    let tag = u32::from(head[0]) | u32::from(head[1]) << 8 | u32::from(head[2]) << 16;
    if tag & 1 != 0 {
        return Err("WebP: VP8 data is not a key frame".into());
    }
    let first_size = (tag >> 5) as usize;
    if head[3..6] != [0x9D, 0x01, 0x2A] {
        return Err("WebP: wrong VP8 start code".into());
    }
    let width = usize::from(u16::from_le_bytes([head[6], head[7]]) & 0x3FFF);
    let height = usize::from(u16::from_le_bytes([head[8], head[9]]) & 0x3FFF);
    if width == 0 || height == 0 {
        return Err("WebP: empty image".into());
    }
    check(width as u64, height as u64)?;
    let rest = &data[10..];
    let first = rest
        .get(..first_size)
        .ok_or("WebP: truncated first partition")?;
    let mut b = Bool::new(first);
    let _color_space = b.flag();
    let _clamp = b.flag();
    // 分段
    let mut seg_absolute = false;
    let mut seg_quant = [0i32; 4];
    let mut seg_probs = [255u8; 3];
    let segmentation = b.flag();
    let mut update_map = false;
    if segmentation {
        update_map = b.flag();
        if b.flag() {
            seg_absolute = b.flag();
            for q in &mut seg_quant {
                *q = b.optional(7);
            }
            for _ in 0..4 {
                b.optional(6);
            }
        }
        if update_map {
            for p in &mut seg_probs {
                *p = if b.flag() { b.literal(8) as u8 } else { 255 };
            }
        }
    }
    // 环路滤波参数（读掉不用）
    b.flag();
    b.literal(6);
    b.literal(3);
    if b.flag() && b.flag() {
        for _ in 0..8 {
            b.optional(6);
        }
    }
    let parts = 1usize << b.literal(2);
    let base_q = b.literal(7) as i32;
    let (dy1_dc, dy2_dc, dy2_ac, duv_dc, duv_ac) = (
        b.optional(4),
        b.optional(4),
        b.optional(4),
        b.optional(4),
        b.optional(4),
    );
    let mut quants = [Quant::default(); 4];
    for (i, m) in quants.iter_mut().enumerate() {
        let q = if segmentation {
            seg_quant[i] + if seg_absolute { 0 } else { base_q }
        } else {
            base_q
        };
        m.y1 = [
            i32::from(DC_TABLE[clip_q(q + dy1_dc, 127)]),
            i32::from(AC_TABLE[clip_q(q, 127)]),
        ];
        m.y2 = [
            i32::from(DC_TABLE[clip_q(q + dy2_dc, 127)]) * 2,
            ((i32::from(AC_TABLE[clip_q(q + dy2_ac, 127)]) * 101_581) >> 16).max(8),
        ];
        m.uv = [
            i32::from(DC_TABLE[clip_q(q + duv_dc, 117)]),
            i32::from(AC_TABLE[clip_q(q + duv_ac, 127)]),
        ];
    }
    let _refresh = b.flag();
    let mut probs: Box<Probs> = Box::new([[[[0; 11]; 3]; 8]; 4]);
    let mut n = 0usize;
    for t in probs.iter_mut() {
        for band in t.iter_mut() {
            for ctx in band.iter_mut() {
                for p in ctx.iter_mut() {
                    *p = if b.bit(COEFFS_UPDATE[n]) {
                        b.literal(8) as u8
                    } else {
                        COEFFS_PROBA0[n]
                    };
                    n += 1;
                }
            }
        }
    }
    let use_skip = b.flag();
    let skip_prob = if use_skip { b.literal(8) as u8 } else { 0 };
    // 系数分区
    let sizes = rest.get(first_size..).ok_or("WebP: truncated partitions")?;
    let table = (parts - 1) * 3;
    let mut body = sizes
        .get(table..)
        .ok_or("WebP: truncated partition table")?;
    let mut tokens = Vec::with_capacity(parts);
    for i in 0..parts {
        let size = if i + 1 < parts {
            let s = &sizes[i * 3..i * 3 + 3];
            usize::from(s[0]) | usize::from(s[1]) << 8 | usize::from(s[2]) << 16
        } else {
            body.len()
        };
        let size = size.min(body.len());
        tokens.push(Bool::new(&body[..size]));
        body = &body[size..];
    }
    let (mbw, mbh) = (width.div_ceil(16), height.div_ceil(16));
    let stride = width;
    let mut out = vec![0u8; width * height];
    // 上一行最后 1 行的亮度（滤波前），模式和非零上下文
    let mut top_y = vec![127u8; (mbw + 1) * 16];
    let mut top_modes = vec![DC_PRED; mbw * 4];
    let mut top_nz = vec![0u32; mbw];
    let mut top_dc = vec![0u32; mbw];
    let mut coef = vec![0i32; 16 * 16];
    let mut workspace = vec![0u8; BPS * 18];
    // 左上角在 (1 行, 8 列)：列 -1 到 19 都在工作区内
    let origin = BPS + 8;
    for mby in 0..mbh {
        let part = &mut tokens[mby % parts];
        let mut left_modes = [DC_PRED; 4];
        let (mut left_nz, mut left_dc) = (0u32, 0u32);
        for y in 0..16 {
            workspace[origin + y * BPS - 1] = 129;
        }
        if mby > 0 {
            workspace[origin - BPS - 1] = 129;
        } else {
            workspace[origin - BPS - 1..origin - BPS + 20].fill(127);
        }
        for mbx in 0..mbw {
            if b.overrun() || part.overrun() {
                return Err("WebP: truncated VP8 data".into());
            }
            // 模式（第一分区）
            let segment = if update_map {
                if b.bit(seg_probs[0]) {
                    2 + usize::from(b.bit(seg_probs[2]))
                } else {
                    usize::from(b.bit(seg_probs[1]))
                }
            } else {
                0
            };
            let skip = use_skip && b.bit(skip_prob);
            let ymode = if !b.bit(145) {
                B_PRED
            } else if b.bit(156) {
                if b.bit(128) { TM_PRED } else { H_PRED }
            } else if b.bit(163) {
                V_PRED
            } else {
                DC_PRED
            };
            let mut modes = [DC_PRED; 16];
            let tm = &mut top_modes[mbx * 4..mbx * 4 + 4];
            if ymode == B_PRED {
                for y in 0..4 {
                    let mut l = left_modes[y];
                    for x in 0..4 {
                        let base = (usize::from(tm[x]) * 10 + usize::from(l)) * 9;
                        let prob = &BMODES_PROBA[base..base + 9];
                        let mut i = i32::from(YMODES_INTRA4[usize::from(b.bit(prob[0]))]);
                        while i > 0 {
                            let idx = i as usize;
                            i = i32::from(YMODES_INTRA4[2 * idx + usize::from(b.bit(prob[idx]))]);
                        }
                        l = (-i) as u8;
                        tm[x] = l;
                        modes[y * 4 + x] = l;
                    }
                    left_modes[y] = l;
                }
            } else {
                tm.fill(ymode);
                left_modes.fill(ymode);
            }
            // 色度模式：读掉
            if b.bit(142) && b.bit(114) {
                b.bit(183);
            }
            // 残差
            coef.fill(0);
            let q = quants[segment];
            let (mut mb_nz, mut mb_dc) = (top_nz[mbx], top_dc[mbx]);
            if !skip {
                let mut first = 0usize;
                let mut ac = 3usize;
                if ymode != B_PRED {
                    let mut dc = [0i32; 16];
                    let ctx = (mb_dc + left_dc) as usize;
                    let nz = coeffs(part, &probs[1], ctx, &q.y2, 0, &mut dc);
                    let bit = u32::from(nz > 0);
                    mb_dc = bit;
                    left_dc = bit;
                    if nz > 1 {
                        inverse_wht(&dc, &mut coef);
                    } else {
                        let v = (dc[0] + 3) >> 3;
                        for i in 0..16 {
                            coef[i * 16] = v;
                        }
                    }
                    first = 1;
                    ac = 0;
                }
                let mut tnz = mb_nz & 0x0F;
                let mut lnz = left_nz & 0x0F;
                for y in 0..4 {
                    let mut l = lnz & 1;
                    for x in 0..4 {
                        let ctx = (l + (tnz & 1)) as usize;
                        let block = &mut coef[(y * 4 + x) * 16..(y * 4 + x) * 16 + 16];
                        let nz = coeffs(part, &probs[ac], ctx, &q.y1, first, block);
                        l = u32::from(nz > first);
                        tnz = (tnz >> 1) | (l << 7);
                    }
                    tnz >>= 4;
                    lnz = (lnz >> 1) | (l << 7);
                }
                let (mut out_t, mut out_l) = (tnz, lnz >> 4);
                let mut scratch = [0i32; 16];
                for ch in [0u32, 2] {
                    let mut tnz = mb_nz >> (4 + ch);
                    let mut lnz = left_nz >> (4 + ch);
                    for _ in 0..2 {
                        let mut l = lnz & 1;
                        for _ in 0..2 {
                            let ctx = (l + (tnz & 1)) as usize;
                            scratch.fill(0);
                            let nz = coeffs(part, &probs[2], ctx, &q.uv, 0, &mut scratch);
                            l = u32::from(nz > 0);
                            tnz = (tnz >> 1) | (l << 3);
                        }
                        tnz >>= 2;
                        lnz = (lnz >> 1) | (l << 5);
                    }
                    out_t |= (tnz << 4) << ch;
                    out_l |= (lnz & 0xF0) << ch;
                }
                mb_nz = out_t;
                left_nz = out_l;
            } else {
                mb_nz = 0;
                left_nz = 0;
                if ymode != B_PRED {
                    mb_dc = 0;
                    left_dc = 0;
                }
            }
            top_nz[mbx] = mb_nz;
            top_dc[mbx] = mb_dc;
            // 重建：把上一块的右边 4 列挪到左边界
            if mbx > 0 {
                for j in 0..17 {
                    let row = origin + j * BPS - BPS;
                    workspace.copy_within(row + 12..row + 16, row - 4);
                }
            }
            if mby > 0 {
                let t = &top_y[mbx * 16..mbx * 16 + 16];
                workspace[origin - BPS..origin - BPS + 16].copy_from_slice(t);
            }
            if ymode == B_PRED {
                if mby > 0 {
                    if mbx + 1 >= mbw {
                        let v = top_y[mbx * 16 + 15];
                        workspace[origin - BPS + 16..origin - BPS + 20].fill(v);
                    } else {
                        let t = &top_y[(mbx + 1) * 16..(mbx + 1) * 16 + 4];
                        workspace[origin - BPS + 16..origin - BPS + 20].copy_from_slice(t);
                    }
                }
                // 第 4 列的块用的是宏块上方的右上像素
                let tr: [u8; 4] = std::array::from_fn(|i| workspace[origin - BPS + 16 + i]);
                for r in [3usize, 7, 11] {
                    workspace[origin + r * BPS + 16..origin + r * BPS + 20].copy_from_slice(&tr);
                }
                for n in 0..16 {
                    let at = origin + (n % 4) * 4 + (n / 4) * 4 * BPS;
                    predict4(&mut workspace, at, modes[n]);
                    add_idct(&coef[n * 16..n * 16 + 16], &mut workspace, at);
                }
            } else {
                predict16(&mut workspace, origin, ymode, mbx > 0, mby > 0);
                for n in 0..16 {
                    let at = origin + (n % 4) * 4 + (n / 4) * 4 * BPS;
                    add_idct(&coef[n * 16..n * 16 + 16], &mut workspace, at);
                }
            }
            // 输出和下一行要用的像素
            let last = origin + 15 * BPS;
            top_y[mbx * 16..mbx * 16 + 16].copy_from_slice(&workspace[last..last + 16]);
            let (ox, oy) = (mbx * 16, mby * 16);
            for y in 0..16.min(height - oy) {
                let w = 16.min(width - ox);
                let src = &workspace[origin + y * BPS..origin + y * BPS + w];
                out[(oy + y) * stride + ox..(oy + y) * stride + ox + w].copy_from_slice(src);
            }
        }
    }
    Ok((out, width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_headers_are_errors() {
        let ok = |_: u64, _: u64| Ok(());
        assert!(decode_luma(&[], &ok).is_err());
        assert!(decode_luma(&[1, 0, 0, 0x9D, 1, 0x2A, 1, 0, 1, 0], &ok).is_err());
        assert!(decode_luma(&[0, 0, 0, 0, 0, 0, 1, 0, 1, 0], &ok).is_err());
        assert!(decode_luma(&[0, 0, 0, 0x9D, 1, 0x2A, 0, 0, 1, 0], &ok).is_err());
    }

    #[test]
    fn garbage_body_does_not_panic() {
        let ok = |_: u64, _: u64| Ok(());
        let mut d = vec![0, 0x40, 0, 0x9D, 1, 0x2A, 64, 0, 64, 0];
        d.extend((0..4000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8));
        let _ = decode_luma(&d, &ok);
    }

    #[test]
    fn idct_of_dc_adds_a_constant() {
        let mut w = vec![100u8; BPS * 5];
        let mut c = [0i32; 16];
        c[0] = 80;
        add_idct(&c, &mut w, BPS + 1);
        assert_eq!(w[BPS + 1], 110);
        assert_eq!(w[BPS + 4 + 3 * BPS], 110);
        assert_eq!(w[BPS], 100);
    }
}
