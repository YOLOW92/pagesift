//! 多尺度可变形注意力（Deformable DETR，`PPDocLayoutV3MultiscaleDeformableAttention`）：每个查询在
//! 每层特征图上按预测的偏移采几个点，按预测的权重加权求和。采样语义同
//! `F.grid_sample(mode="bilinear", padding_mode="zeros", align_corners=False)`。

use pagesift_infer::{Linear, Tensor, Weights, ops};

use super::query::Bbox;

/// 一层特征在展平序列里的位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Level {
    pub h: usize,
    pub w: usize,
    /// 这层第一个位置在序列里的下标。
    pub start: usize,
}

/// 在展平的特征表上双线性采一点，乘 `weight` 累加进 `out`。
/// `value` 每行 `stride` 个数；取每行 `[off, off + out.len())` 这几列。
/// (lx, ly) 是 0–1 的归一化坐标（0 是左上角像素的外边缘）；越界的角按 0 算。
#[allow(clippy::too_many_arguments)]
pub(crate) fn sample_add(
    value: &[f32],
    stride: usize,
    off: usize,
    lv: &Level,
    lx: f32,
    ly: f32,
    weight: f32,
    out: &mut [f32],
) {
    // grid = 2·loc − 1；align_corners=False 时像素坐标 = ((grid + 1)·尺寸 − 1) / 2
    let pixel = |loc: f32, n: usize| ((2.0 * loc - 1.0 + 1.0) * n as f32 - 1.0) / 2.0;
    let (ix, iy) = (pixel(lx, lv.w), pixel(ly, lv.h));
    let (fx, fy) = (ix.floor(), iy.floor());
    let (tx, ty) = (ix - fx, iy - fy);
    let (x0, y0) = (fx as i64, fy as i64);
    let n = out.len();
    let corners = [
        (x0, y0, (1.0 - tx) * (1.0 - ty)),
        (x0 + 1, y0, tx * (1.0 - ty)),
        (x0, y0 + 1, (1.0 - tx) * ty),
        (x0 + 1, y0 + 1, tx * ty),
    ];
    for (x, y, c) in corners {
        if x < 0 || y < 0 || x >= lv.w as i64 || y >= lv.h as i64 {
            continue;
        }
        let at = (lv.start + y as usize * lv.w + x as usize) * stride + off;
        let k = weight * c;
        for (o, v) in out.iter_mut().zip(&value[at..at + n]) {
            *o += k * v;
        }
    }
}

/// 可变形注意力层。
#[derive(Debug, Clone)]
pub(crate) struct DeformAttn {
    offsets: Linear,
    weights: Linear,
    value: Linear,
    output: Linear,
    heads: usize,
    points: usize,
}

impl DeformAttn {
    pub(crate) fn load(
        w: &Weights,
        p: &str,
        heads: usize,
        points: usize,
    ) -> Result<DeformAttn, String> {
        Ok(DeformAttn {
            offsets: Linear::load(w, &format!("{p}.sampling_offsets"))?,
            weights: Linear::load(w, &format!("{p}.attention_weights"))?,
            value: Linear::load(w, &format!("{p}.value_proj"))?,
            output: Linear::load(w, &format!("{p}.output_proj"))?,
            heads,
            points,
        })
    }

    /// `query` 是 `[Q, C]`（已加位置编码），`src` 是编码器特征 `[S, C]`，
    /// `refs` 是每个查询的参考框 (cx, cy, w, h)，0–1。
    pub(crate) fn forward(
        &self,
        query: &Tensor,
        src: &Tensor,
        levels: &[Level],
        refs: &[Bbox],
    ) -> Tensor {
        let (q, c) = query.tc();
        let (heads, points, nl) = (self.heads, self.points, levels.len());
        let d = c / heads;
        let lp = nl * points;
        let value = self.value.forward(src);
        let off = self.offsets.forward(query);
        // 每个 (查询, 头) 在所有 (层, 点) 上做 softmax
        let mut aw = self.weights.forward(query).reshape(vec![q * heads, lp]);
        ops::softmax_rows(&mut aw);
        let mut out = vec![0f32; q * c];
        let half = 0.5f32;
        for (qi, row) in out.chunks_exact_mut(c).enumerate() {
            let [cx, cy, bw, bh] = refs[qi];
            for (h, acc) in row.chunks_exact_mut(d).enumerate() {
                let at = qi * heads + h;
                for (l, lv) in levels.iter().enumerate() {
                    for p in 0..points {
                        let i = (at * nl + l) * points + p;
                        // 坐标 = 框心 + 偏移 / 点数 · 框尺寸 · ½
                        let lx = cx + off.data[i * 2] / points as f32 * bw * half;
                        let ly = cy + off.data[i * 2 + 1] / points as f32 * bh * half;
                        let wt = aw.data[at * lp + l * points + p];
                        sample_add(&value.data, c, h * d, lv, lx, ly, wt, acc);
                    }
                }
            }
        }
        self.output.forward(&Tensor::new(vec![q, c], out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 按 `grid_sample` 的接口包一层：`x` 是 `[C, H, W]`，`grid` 是 −1…1 的 (gx, gy)，结果每点 C 个数。
    fn grid_sample(x: &Tensor, grid: &[(f32, f32)]) -> Vec<Vec<f32>> {
        let (c, h, w) = x.chw();
        let seq = ops::chw_to_seq(x);
        let lv = Level { h, w, start: 0 };
        grid.iter()
            .map(|&(gx, gy)| {
                let mut o = vec![0f32; c];
                sample_add(
                    &seq.data,
                    c,
                    0,
                    &lv,
                    (gx + 1.0) / 2.0,
                    (gy + 1.0) / 2.0,
                    1.0,
                    &mut o,
                );
                o
            })
            .collect()
    }

    fn close(a: &[f32], b: &[f32]) {
        for (p, q) in a.iter().zip(b) {
            assert!((p - q).abs() < 1e-5, "{a:?} {b:?}");
        }
    }

    #[test]
    fn grid_sample_语义() {
        // 2×2：[[1, 2], [3, 4]]。align_corners=False 时像素中心在 grid = ±0.5
        let x = Tensor::new(vec![1, 2, 2], vec![1.0, 2.0, 3.0, 4.0]);
        let r = grid_sample(
            &x,
            &[
                (-0.5, -0.5), // 左上像素中心
                (0.5, 0.5),   // 右下像素中心
                (0.0, 0.0),   // 四个像素正中
                (0.0, -0.5),  // 上排两像素中间
            ],
        );
        close(&r[0], &[1.0]);
        close(&r[1], &[4.0]);
        close(&r[2], &[2.5]);
        close(&r[3], &[1.5]);
    }

    #[test]
    fn grid_sample_越界补零() {
        let x = Tensor::new(vec![1, 2, 2], vec![1.0, 2.0, 3.0, 4.0]);
        // grid = −1 是左上像素的外边缘：像素坐标 −0.5，一半落在界外
        let r = grid_sample(&x, &[(-1.0, -0.5), (-1.0, -1.0), (-3.0, 0.0), (1.0, 0.5)]);
        close(&r[0], &[0.5]);
        close(&r[1], &[0.25]);
        close(&r[2], &[0.0]);
        close(&r[3], &[2.0]);
    }

    #[test]
    fn 多通道与层起点() {
        // 序列里前 1 个位置属于别的层；这一层 1×2，每位置 4 通道，只取后 2 通道
        let value = [9., 9., 9., 9., 1., 2., 3., 4., 5., 6., 7., 8.];
        let lv = Level {
            h: 1,
            w: 2,
            start: 1,
        };
        let mut o = [0f32; 2];
        // 两个像素正中（x = 0.5 归一化），纵向在像素中心
        sample_add(&value, 4, 2, &lv, 0.5, 0.5, 2.0, &mut o);
        close(&o, &[10.0, 12.0]);
    }
}
