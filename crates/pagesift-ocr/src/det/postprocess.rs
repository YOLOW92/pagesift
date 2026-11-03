//! DB 后处理：概率图 → 二值图 → 连通域外轮廓 → 最小外接矩形 → 框内平均分 → unclip 外扩 →
//! 再取最小外接矩形 → 映射回原图坐标。语义照 transformers 的
//! `PPOCRV5ServerDetImageProcessor.post_process_object_detection`（Apache-2.0）。

use super::contour::{self, Bitmap};
use super::geometry::{self, P};
use crate::Quad;

/// DB 后处理参数。
#[derive(Debug, Clone)]
pub struct DbParams {
    /// 概率高于它的像素算文字。
    pub thresh: f32,
    /// 框内平均概率不低于它才保留。
    pub box_thresh: f32,
    /// 最多处理的轮廓数。
    pub max_candidates: usize,
    /// 外扩前短边至少这么长（概率图像素）；外扩后至少再长 2。
    pub min_size: f64,
    /// 外扩比例。
    pub unclip_ratio: f64,
}

impl Default for DbParams {
    fn default() -> Self {
        // [协议] post_process_object_detection 的缺省：threshold 0.3、box_threshold 0.6、
        // max_candidates 1000、min_size 3、unclip_ratio 1.5
        DbParams {
            thresh: 0.3,
            box_thresh: 0.6,
            max_candidates: 1000,
            min_size: 3.0,
            unclip_ratio: 1.5,
        }
    }
}

/// 一张概率图（按行存放，`width × height`）。
pub struct ProbMap<'a> {
    pub data: &'a [f32],
    pub width: usize,
    pub height: usize,
}

/// 框内平均概率（参考实现的 `_get_box_score`，fast 模式）：取框的轴对齐外接矩形，
/// 在其中把框（顶点截断成整数）填满作掩码，求掩码内概率的平均。
pub fn box_score_fast(prob: &ProbMap, quad: &[P; 4]) -> f32 {
    let (w, h) = (prob.width as i64, prob.height as i64);
    let xs = quad.iter().map(|p| p.0);
    let ys = quad.iter().map(|p| p.1);
    let clamp = |v: f64, hi: i64| (v as i64).clamp(0, hi - 1);
    let xmin = clamp(xs.clone().fold(f64::MAX, f64::min).floor(), w);
    let xmax = clamp(xs.fold(f64::MIN, f64::max).ceil(), w);
    let ymin = clamp(ys.clone().fold(f64::MAX, f64::min).floor(), h);
    let ymax = clamp(ys.fold(f64::MIN, f64::max).ceil(), h);
    // [协议] 参考实现先减去外接矩形左上角，再 astype(int32)（向零截断）
    let poly: Vec<(i32, i32)> = quad
        .iter()
        .map(|p| ((p.0 - xmin as f64) as i32, (p.1 - ymin as f64) as i32))
        .collect();
    let bw = (xmax - xmin + 1) as usize;
    let bh = (ymax - ymin + 1) as usize;
    let mut sum = 0f64;
    let mut n = 0usize;
    for (y, x0, x1) in geometry::fill_convex_rows(&poly, bw, bh) {
        let row = (ymin as usize + y) * prob.width + xmin as usize;
        sum += prob.data[row + x0..=row + x1]
            .iter()
            .map(|&v| v as f64)
            .sum::<f64>();
        n += x1 - x0 + 1;
    }
    // cv2.mean 在掩码为空时给 0
    if n == 0 { 0.0 } else { (sum / n as f64) as f32 }
}

/// 概率图 → 原图坐标下的文字框和分数。`dest_w`、`dest_h` 是原图尺寸。
pub fn boxes_from_prob(
    prob: &ProbMap,
    dest_w: usize,
    dest_h: usize,
    p: &DbParams,
) -> Vec<(Quad, f32)> {
    let bits: Vec<bool> = prob.data.iter().map(|&v| v > p.thresh).collect();
    let contours = contour::outer_contours(&Bitmap {
        data: &bits,
        width: prob.width,
        height: prob.height,
    });
    let sx = dest_w as f64 / prob.width as f64;
    let sy = dest_h as f64 / prob.height as f64;
    let mut out = Vec::new();
    for c in contours.iter().take(p.max_candidates) {
        let pts: Vec<P> = c.iter().map(|&(x, y)| (x as f64, y as f64)).collect();
        let (quad, short) = geometry::mini_box(&pts);
        if short < p.min_size {
            continue;
        }
        let score = box_score_fast(prob, &quad);
        if score < p.box_thresh {
            continue;
        }
        let Some(expanded) = geometry::unclip(&quad, p.unclip_ratio) else {
            continue;
        };
        let (quad, short) = geometry::mini_box(&expanded);
        // [协议] 参考实现外扩后要求短边 ≥ min_size + 2
        if short < p.min_size + 2.0 {
            continue;
        }
        // [协议] 参考实现按比例放回原图后 round（numpy 的四舍六入五成双）并裁到 [0, 原图宽高]
        let map = |(x, y): P| {
            (
                (x * sx).round_ties_even().clamp(0.0, dest_w as f64) as f32,
                (y * sy).round_ties_even().clamp(0.0, dest_h as f64) as f32,
            )
        };
        out.push((quad.map(map), score));
    }
    out
}

/// 把文字框排成阅读顺序：先按左上角 y、再按 x 排；之后相邻两框左上角的 y 相差不到
/// `line_tol` 像素时视为同一行，按 x 从左到右调整（同 PaddleOCR 的 `sorted_boxes`）。
pub fn sort_reading_order(boxes: &mut [(Quad, f32)], line_tol: f32) {
    boxes.sort_by(|a, b| {
        let (pa, pb) = (a.0[0], b.0[0]);
        pa.1.total_cmp(&pb.1).then(pa.0.total_cmp(&pb.0))
    });
    for i in 0..boxes.len().saturating_sub(1) {
        for j in (0..=i).rev() {
            let (a, b) = (boxes[j].0[0], boxes[j + 1].0[0]);
            if (b.1 - a.1).abs() < line_tol && b.0 < a.0 {
                boxes.swap(j, j + 1);
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 框内平均分() {
        // 4 × 4 概率图，左上 2 × 2 是 1，其余 0
        let mut data = vec![0f32; 16];
        for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            data[y * 4 + x] = 1.0;
        }
        let pm = ProbMap {
            data: &data,
            width: 4,
            height: 4,
        };
        let q = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        assert!((box_score_fast(&pm, &q) - 1.0).abs() < 1e-6);
        let q = [(0.0, 0.0), (3.0, 0.0), (3.0, 1.0), (0.0, 1.0)];
        assert!((box_score_fast(&pm, &q) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn 一个矩形块变成外扩后的框() {
        // 40 × 20 的概率图，(5..=24, 6..=11) 是一块 20 × 6 的文字
        let (w, h) = (40, 20);
        let mut data = vec![0f32; w * h];
        for y in 6..=11 {
            for x in 5..=24 {
                data[y * w + x] = 0.9;
            }
        }
        let pm = ProbMap {
            data: &data,
            width: w,
            height: h,
        };
        // 原图是概率图的 2 倍
        let r = boxes_from_prob(&pm, 2 * w, 2 * h, &DbParams::default());
        assert_eq!(r.len(), 1);
        let (q, s) = r[0];
        assert!((s - 0.9).abs() < 1e-6);
        // 像素中心构成 19 × 5 的矩形：d = 95 × 1.5 / 48 ≈ 2.97
        let d = 95.0 * 1.5 / 48.0;
        let want = [
            (5.0 - d, 6.0 - d),
            (24.0 + d, 6.0 - d),
            (24.0 + d, 11.0 + d),
            (5.0 - d, 11.0 + d),
        ];
        for (got, (x, y)) in q.iter().zip(want) {
            let (ex, ey) = (
                (2.0f64 * x).round_ties_even(),
                (2.0f64 * y).round_ties_even(),
            );
            assert_eq!((got.0 as f64, got.1 as f64), (ex, ey), "{q:?}");
        }
    }

    #[test]
    fn 太小或分低的块不要() {
        let (w, h) = (20, 10);
        let mut data = vec![0f32; w * h];
        // 2 像素高：短边 1 < 3
        for x in 2..15 {
            data[2 * w + x] = 0.9;
            data[3 * w + x] = 0.9;
        }
        // 分数 0.4 < 0.6
        for y in 5..10 {
            for x in 2..15 {
                data[y * w + x] = 0.4;
            }
        }
        let pm = ProbMap {
            data: &data,
            width: w,
            height: h,
        };
        assert!(boxes_from_prob(&pm, w, h, &DbParams::default()).is_empty());
    }

    #[test]
    fn 阅读顺序() {
        let b = |x: f32, y: f32| -> (Quad, f32) {
            (
                [(x, y), (x + 5.0, y), (x + 5.0, y + 3.0), (x, y + 3.0)],
                1.0,
            )
        };
        let mut v = vec![b(50.0, 100.0), b(10.0, 104.0), b(10.0, 10.0), b(60.0, 8.0)];
        sort_reading_order(&mut v, 10.0);
        let xs: Vec<(f32, f32)> = v.iter().map(|q| q.0[0]).collect();
        assert_eq!(
            xs,
            vec![(10.0, 10.0), (60.0, 8.0), (10.0, 104.0), (50.0, 100.0)]
        );
    }
}
