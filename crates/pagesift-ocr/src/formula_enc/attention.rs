//! SAM 式多头自注意力，带分解的相对位置编码（MViTv2），照参考实现的
//! `PPFormulaNetVisionAttention`：
//!
//! ```text
//! qkv = Linear(x)                      # 列 = s·C + h·hd + d（s = 0 q、1 k、2 v）
//! A   = (q·scale) @ kᵀ + rel_h[.., kh] + rel_w[.., kw]
//! rel_h[(y,x), kh] = q[(y,x)] · Rh[y, kh]，Rh[y, kh] = 插值后的 rel_pos_h[y − kh + s − 1]
//! out = proj(softmax(A) @ v)
//! ```
//!
//! 相对位置项用的是未乘 scale 的 q。输入是 G 组（窗口）各 s×s 个 token 叠在一起的 `[G·s·s, C]`，
//! 每组单独做注意力；全局注意力就是 G = 1、s = 48。

use pagesift_infer::{Linear, Tensor, Weights, ops};

use super::par;

/// `F.interpolate(mode="linear", align_corners=False)` 沿第 0 维把 `[len, d]` 的表插值到 `out` 行。
/// 源坐标 = (目标 + 0.5)·len/out − 0.5，负数截到 0。
pub(crate) fn interp_linear(table: &[f32], d: usize, out: usize) -> Vec<f32> {
    let len = table.len() / d;
    if len == out {
        return table.to_vec();
    }
    let scale = len as f64 / out as f64;
    let mut res = Vec::with_capacity(out * d);
    for i in 0..out {
        let src = ((i as f64 + 0.5) * scale - 0.5).max(0.0);
        let i0 = (src as usize).min(len - 1);
        let i1 = (i0 + 1).min(len - 1);
        let f = (src - i0 as f64) as f32;
        let (a, b) = (&table[i0 * d..(i0 + 1) * d], &table[i1 * d..(i1 + 1) * d]);
        res.extend(a.iter().zip(b).map(|(&a, &b)| a + (b - a) * f));
    }
    res
}

/// `get_rel_pos`：表 `[len, d]` 插值到 2·max(q, k) − 1 行后，按相对坐标取出 `[q, k, d]`。
/// 相对坐标 = q_i·max(k/q, 1) − k_j·max(q/k, 1) + (k − 1)·max(q/k, 1)，向零取整。
pub(crate) fn rel_pos(table: &[f32], d: usize, q: usize, k: usize) -> Vec<f32> {
    let max_rel = 2 * q.max(k) - 1;
    let resized = interp_linear(table, d, max_rel);
    let (sq, sk) = (
        (k as f64 / q as f64).max(1.0),
        (q as f64 / k as f64).max(1.0),
    );
    let mut out = Vec::with_capacity(q * k * d);
    for i in 0..q {
        for j in 0..k {
            let r = (i as f64 * sq - j as f64 * sk + (k - 1) as f64 * sk) as usize;
            out.extend_from_slice(&resized[r * d..(r + 1) * d]);
        }
    }
    out
}

/// 一层的注意力。
pub(crate) struct Attention {
    qkv: Linear,
    proj: Linear,
    heads: usize,
    /// 每组的边长（窗口大小，或全局时的特征图边长）
    side: usize,
    /// 取好的相对位置 `[side, side, hd]`：高、宽两个方向
    rel_h: Vec<f32>,
    rel_w: Vec<f32>,
}

impl Attention {
    /// 读 `{p}.qkv`、`{p}.proj`、`{p}.rel_pos_h/w`；`side` 是每组的边长。
    pub(crate) fn load(w: &Weights, p: &str, heads: usize, side: usize) -> Result<Self, String> {
        let qkv = Linear::load(w, &format!("{p}.qkv"))?;
        let proj = Linear::load(w, &format!("{p}.proj"))?;
        let c = proj.weight_t.shape[1];
        if c % heads != 0 || qkv.weight_t.shape[1] != 3 * c {
            return Err(format!("{p}：通道 {c} 与头数 {heads} 或 qkv 形状不符"));
        }
        let hd = c / heads;
        let table = |name: &str| -> Result<Vec<f32>, String> {
            let t = w.get(&format!("{p}.{name}"))?;
            if t.shape.len() != 2 || t.shape[1] != hd {
                return Err(format!("{p}.{name} 形状 {:?} 不是 [L, {hd}]", t.shape));
            }
            Ok(rel_pos(&t.data, hd, side, side))
        };
        Ok(Attention {
            rel_h: table("rel_pos_h")?,
            rel_w: table("rel_pos_w")?,
            qkv,
            proj,
            heads,
            side,
        })
    }

    /// `x` 是 `[G·side², C]`（G 组叠在一起），返回同形状。
    pub(crate) fn forward(&self, x: &Tensor) -> Tensor {
        let (n, c) = x.tc();
        let t = self.side * self.side;
        assert_eq!(n % t, 0, "token 数 {n} 不是 {t} 的倍数");
        let groups = n / t;
        let hd = c / self.heads;
        let qkv = self.qkv.forward(x);
        let mut out = vec![0f32; n * c];
        let mut put = |g: usize, h: usize, o: &[f32]| {
            for (i, row) in o.chunks_exact(hd).enumerate() {
                let at = (g * t + i) * c + h * hd;
                out[at..at + hd].copy_from_slice(row);
            }
        };
        if groups > 1 {
            // 窗口注意力：每个 (组, 头) 是一个小任务（196 个 token），任务间并行，任务内单线程
            let res = par::map(groups * self.heads, |i| {
                self.head(&qkv.data, c, i / self.heads, i % self.heads, false)
            });
            for (i, o) in res.iter().enumerate() {
                put(i / self.heads, i % self.heads, o);
            }
        } else {
            // 全局注意力：逐头做，矩阵乘和 softmax 各自多线程
            for h in 0..self.heads {
                let o = self.head(&qkv.data, c, 0, h, true);
                put(0, h, &o);
            }
        }
        self.proj.forward(&Tensor::new(vec![n, c], out))
    }

    /// 第 `g` 组第 `h` 头的注意力输出 `[t, hd]`。`parallel` 时 softmax 按行多线程。
    fn head(&self, qkv: &[f32], c: usize, g: usize, h: usize, parallel: bool) -> Vec<f32> {
        let (s, t) = (self.side, self.side * self.side);
        let hd = c / self.heads;
        // [协议] scale = head_dim^−0.5
        let scale = (hd as f32).powf(-0.5);
        let take = |part: usize| -> Vec<f32> {
            let mut d = Vec::with_capacity(t * hd);
            for row in qkv[g * t * 3 * c..(g + 1) * t * 3 * c].chunks_exact(3 * c) {
                let at = part * c + h * hd;
                d.extend_from_slice(&row[at..at + hd]);
            }
            d
        };
        let q = take(0);
        let qs = Tensor::new(vec![t, hd], q.iter().map(|v| v * scale).collect());
        let kt = ops::transpose(&Tensor::new(vec![t, hd], take(1)));
        let v = Tensor::new(vec![t, hd], take(2));
        let mut a = ops::matmul(&qs, &kt); // [t, t]
        let row_fn = |i: usize, row: &mut [f32]| {
            let qi = &q[i * hd..(i + 1) * hd];
            let (y, x) = (i / s, i % s);
            let dot = |r: &[f32]| qi.iter().zip(r).map(|(a, b)| a * b).sum::<f32>();
            let rh: Vec<f32> = (0..s)
                .map(|kh| dot(&self.rel_h[(y * s + kh) * hd..(y * s + kh + 1) * hd]))
                .collect();
            let rw: Vec<f32> = (0..s)
                .map(|kw| dot(&self.rel_w[(x * s + kw) * hd..(x * s + kw + 1) * hd]))
                .collect();
            for (kh, seg) in row.chunks_exact_mut(s).enumerate() {
                for (v, r) in seg.iter_mut().zip(&rw) {
                    *v += rh[kh] + r;
                }
            }
            softmax(row);
        };
        if parallel {
            par::chunks_mut(&mut a.data, t, row_fn);
        } else {
            for (i, row) in a.data.chunks_exact_mut(t).enumerate() {
                row_fn(i, row);
            }
        }
        ops::matmul(&a, &v).data
    }
}

/// 一行就地 softmax。
fn softmax(row: &mut [f32]) {
    let m = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut sum = 0f32;
    for v in row.iter_mut() {
        *v = (*v - m).exp();
        sum += *v;
    }
    let inv = 1.0 / sum;
    for v in row.iter_mut() {
        *v *= inv;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 同长插值是恒等_放大保两端() {
        let t: Vec<f32> = (0..6).map(|v| v as f32).collect(); // [3, 2]
        assert_eq!(interp_linear(&t, 2, 3), t);
        // 3 行放大到 5 行：源坐标 (i+0.5)·0.6−0.5 = 0, 0.4, 1.0, 1.6, 2.2→截到 2
        let up = interp_linear(&t, 2, 5);
        let col0: Vec<f32> = up.chunks(2).map(|r| r[0]).collect();
        let want = [0.0, 0.8, 2.0, 3.2, 4.0];
        for (a, b) in col0.iter().zip(want) {
            assert!((a - b).abs() < 1e-5, "{col0:?}");
        }
        // 缩小：5 行到 3 行，中间一行取第 2 行
        let t5: Vec<f32> = (0..5).map(|v| v as f32).collect();
        let down = interp_linear(&t5, 1, 3);
        assert!((down[1] - 2.0).abs() < 1e-6, "{down:?}");
    }

    #[test]
    fn 相对位置按差取表() {
        // 表第 r 行的值就是 r：q = k = 4 时取出的是 i − j + 3
        let table: Vec<f32> = (0..7).map(|v| v as f32).collect();
        let r = rel_pos(&table, 1, 4, 4);
        assert_eq!(r.len(), 16);
        for i in 0..4 {
            for j in 0..4 {
                assert_eq!(r[i * 4 + j], (i + 3 - j) as f32);
            }
        }
        // 表长不符时先插值：5 行的表插到 7 行
        let short: Vec<f32> = (0..5).map(|v| v as f32).collect();
        let r = rel_pos(&short, 1, 4, 4);
        assert_eq!(r[3], 0.0); // i=0, j=3 → 第 0 行
        assert_eq!(r[12], 4.0); // i=3, j=0 → 第 6 行（插值后末行 = 原末行）
    }

    #[test]
    fn softmax_归一() {
        let mut r = vec![1.0, 2.0, 3.0, -1e30];
        softmax(&mut r);
        assert!((r.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert_eq!(r[3], 0.0);
    }
}
