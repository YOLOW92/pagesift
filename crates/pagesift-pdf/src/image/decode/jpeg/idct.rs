//! 反量化与 8×8 反 DCT。定义见 ITU-T T.81 §A.3.3：
//! s(y,x) = 1/4 · Σu Σv C(u) C(v) S(v,u) cos((2x+1)uπ/16) cos((2y+1)vπ/16)，C(0) = 1/√2，其余为 1；
//! 结果加电平偏移 128（8 位样本，§A.3.1）后取整、截到 0–255。
//!
//! 用 Arai–Agui–Nakajima 的快速算法（Y. Arai, T. Agui, M. Nakajima, "A Fast DCT-SQ Scheme for
//! Images", Trans. IEICE E71(11), 1988；Pennebaker & Mitchell《JPEG Still Image Data Compression
//! Standard》§4.3.5 的蝶形图）：一维 8 点只要 5 次乘法，其余的缩放并进反量化表。

use super::marker::{BLOCK, COEFS};

/// 反量化表并上 AAN 的缩放：q'(u,v) = q(u,v) · a(u) · a(v) / 8，a(0) = 1，a(k) = √2·cos(kπ/16)。
pub(super) struct Idct {
    scaled: [f32; COEFS],
}

/// [推导] AAN 蝶形里的四个乘数：2cos(π/4)、2cos(π/8)、2(cos(π/8) − cos(3π/8))、2(cos(π/8) + cos(3π/8))
const M_C4: f32 = std::f32::consts::SQRT_2;
const M_C2: f32 = 1.847_759; // [推导] 2cos(π/8)
const M_C2_MINUS_C6: f32 = 1.082_392_2; // [推导] 2(cos(π/8) − cos(3π/8))
const M_C2_PLUS_C6: f32 = 2.613_126; // [推导] 2(cos(π/8) + cos(3π/8))

impl Idct {
    pub(super) fn new(quant: &[u16; COEFS]) -> Idct {
        let a = |k: usize| {
            if k == 0 {
                1.0
            } else {
                // [协议] T.81 §A.3.3 的余弦 kπ/16，乘 √2 是 AAN 的归一化
                (std::f64::consts::SQRT_2 * (k as f64 * std::f64::consts::PI / 16.0).cos()) as f32
            }
        };
        let mut scaled = [0f32; COEFS];
        for v in 0..BLOCK {
            for u in 0..BLOCK {
                // [推导] 两遍一维变换合起来多出因子 8，除掉
                scaled[v * BLOCK + u] = f32::from(quant[v * BLOCK + u]) * a(u) * a(v) / 8.0;
            }
        }
        Idct { scaled }
    }

    /// 反量化一块系数（自然顺序）并做 IDCT，写进 `out`（行距 `stride`）的 8×8 区域。
    pub(super) fn block(&self, coefs: &[i16], out: &mut [u8], stride: usize) {
        if coefs[1..].iter().all(|&c| c == 0) {
            // 只有 DC：整块同一个值 F(0,0)·C(0)²/4 = F(0,0)/8，正好是缩放表里的 q'(0,0)
            let v = level(f32::from(coefs[0]) * self.scaled[0]);
            for y in 0..BLOCK {
                out[y * stride..y * stride + BLOCK].fill(v);
            }
            return;
        }
        let mut w = [0f32; COEFS];
        // 第一遍：按列（固定 u，沿 v）
        for u in 0..BLOCK {
            let at = |v: usize| f32::from(coefs[v * BLOCK + u]) * self.scaled[v * BLOCK + u];
            if (1..BLOCK).all(|v| coefs[v * BLOCK + u] == 0) {
                let dc = at(0);
                for v in 0..BLOCK {
                    w[v * BLOCK + u] = dc;
                }
                continue;
            }
            // [协议] 一列 8 个系数，v = 0..7（8×8 块，T.81 §A.3.3）
            let col = butterfly([at(0), at(1), at(2), at(3), at(4), at(5), at(6), at(7)]);
            for v in 0..BLOCK {
                w[v * BLOCK + u] = col[v];
            }
        }
        // 第二遍：按行（固定 y，沿 u）
        for y in 0..BLOCK {
            let r = &w[y * BLOCK..(y + 1) * BLOCK];
            let row = butterfly([r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]]);
            let o = &mut out[y * stride..y * stride + BLOCK];
            for x in 0..BLOCK {
                o[x] = level(row[x]);
            }
        }
    }
}

/// 一维 8 点 IDCT 的 AAN 蝶形（输入已按 AAN 缩放）。
#[inline(always)]
fn butterfly(i: [f32; BLOCK]) -> [f32; BLOCK] {
    // 偶数部分
    let (t10, t11) = (i[0] + i[4], i[0] - i[4]);
    let t13 = i[2] + i[6];
    let t12 = (i[2] - i[6]) * M_C4 - t13;
    let (e0, e3, e1, e2) = (t10 + t13, t10 - t13, t11 + t12, t11 - t12);
    // 奇数部分
    let (z13, z10) = (i[5] + i[3], i[5] - i[3]);
    let (z11, z12) = (i[1] + i[7], i[1] - i[7]);
    let o7 = z11 + z13;
    let o11 = (z11 - z13) * M_C4;
    let z5 = (z10 + z12) * M_C2;
    let o10 = z5 - z12 * M_C2_MINUS_C6;
    let o12 = z5 - z10 * M_C2_PLUS_C6;
    let o6 = o12 - o7;
    let o5 = o11 - o6;
    let o4 = o10 - o5;
    [
        e0 + o7,
        e1 + o6,
        e2 + o5,
        e3 + o4,
        e3 - o4,
        e2 - o5,
        e1 - o6,
        e0 - o7,
    ]
}

/// [协议] 8 位样本的电平偏移 128（T.81 §A.3.1），截到 0–255
#[inline(always)]
fn level(s: f32) -> u8 {
    // [协议] 加电平偏移 128，截到 8 位样本范围 0–255（T.81 §A.3.1）
    (s + 128.0).round().clamp(0.0, 255.0) as u8
}
