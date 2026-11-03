//! 后处理（`PPDocLayoutV3ImageProcessor.post_process_object_detection`）：逐类 σ 打分，展平取前 Q 个，
//! 按阈值过滤，框从 (cx, cy, w, h) 换成原图像素的 (x0, y0, x1, y1)，按预测的阅读顺序排。
//! 掩码转多边形不做。

use super::query::{Bbox, sigmoid, topk};

/// 一个检测结果。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Detection {
    pub class: usize,
    pub score: f32,
    pub xyxy: Bbox,
}

/// 由阅读顺序 logit `[Q, Q]` 算每个查询的名次（`_get_order_seqs`）：
/// 第 j 个查询的票数 = Σ_{i<j} σ(Lᵢⱼ) + Σ_{i>j} (1 − σ(Lⱼᵢ))，即“排在它前面的有多少”，从小到大排名。
pub(crate) fn order_ranks(logits: &[f32], n: usize) -> Vec<usize> {
    let mut votes = vec![0f32; n];
    for i in 0..n {
        for j in i + 1..n {
            let s = sigmoid(logits[i * n + j]);
            votes[j] += s;
            votes[i] += 1.0 - s;
        }
    }
    let mut pointers: Vec<usize> = (0..n).collect();
    pointers.sort_by(|&a, &b| votes[a].total_cmp(&votes[b]).then(a.cmp(&b)));
    let mut ranks = vec![0; n];
    for (r, &p) in pointers.iter().enumerate() {
        ranks[p] = r;
    }
    ranks
}

/// (cx, cy, w, h)（相对 0–1）→ 原图像素的 (x0, y0, x1, y1)。
/// [策略] 参考实现不截断；这里截到图内，方便调用方直接裁图。
pub(crate) fn decode_box(b: Bbox, img_w: f32, img_h: f32) -> Bbox {
    let [cx, cy, w, h] = b;
    [
        ((cx - 0.5 * w) * img_w).clamp(0.0, img_w),
        ((cy - 0.5 * h) * img_h).clamp(0.0, img_h),
        ((cx + 0.5 * w) * img_w).clamp(0.0, img_w),
        ((cy + 0.5 * h) * img_h).clamp(0.0, img_h),
    ]
}

/// 选结果。`logits` 是 `[Q, K]`（按行），`boxes` 每个查询一个框，`ranks` 是阅读顺序名次。
pub(crate) fn select(
    logits: &[f32],
    classes: usize,
    boxes: &[Bbox],
    ranks: &[usize],
    threshold: f32,
    img_w: f32,
    img_h: f32,
) -> Vec<Detection> {
    let scores: Vec<f32> = logits.iter().map(|&v| sigmoid(v)).collect();
    let q = boxes.len();
    let mut keep: Vec<(usize, Detection)> = Vec::new();
    for i in topk(&scores, q) {
        if scores[i] < threshold {
            continue;
        }
        let query = i / classes;
        keep.push((
            ranks[query],
            Detection {
                class: i % classes,
                score: scores[i],
                xyxy: decode_box(boxes[query], img_w, img_h),
            },
        ));
    }
    keep.sort_by_key(|k| k.0);
    keep.into_iter().map(|k| k.1).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 框解码() {
        let b = decode_box([0.5, 0.25, 0.2, 0.1], 1000.0, 2000.0);
        for (a, e) in b.iter().zip([400.0, 400.0, 600.0, 600.0]) {
            assert!((a - e).abs() < 1e-3, "{b:?}");
        }
        // 越界的框截到图内
        assert_eq!(
            decode_box([0.0, 1.0, 0.4, 0.4], 100.0, 100.0),
            [0.0, 80.0, 20.0, 100.0]
        );
    }

    #[test]
    fn 阅读顺序() {
        // 3 个查询，L[i][j] > 0 表示 i 在 j 前。设真实顺序是 2, 0, 1
        let big = 10.0;
        #[rustfmt::skip]
        let l = [
            -1e4, big, -big, // 0 在 1 前，0 在 2 后
            -1e4, -1e4, -big, // 1 在 2 后
            -1e4, -1e4, -1e4,
        ];
        assert_eq!(order_ranks(&l, 3), [1, 2, 0]);
    }

    #[test]
    fn 阈值与排序() {
        // 2 个查询、3 类
        let inv = |p: f32| (p / (1.0 - p)).ln();
        let logits = [
            inv(0.9),
            inv(0.1),
            inv(0.6), // 查询 0：类 0、类 2 都过阈值
            inv(0.2),
            inv(0.8),
            inv(0.3), // 查询 1：类 1
        ];
        let boxes = [[0.5, 0.5, 1.0, 1.0], [0.5, 0.5, 0.5, 0.5]];
        // 查询 1 的阅读顺序在前；topk 取前 Q = 2 个：只剩 0.9（查询 0）和 0.8（查询 1）
        let r = select(&logits, 3, &boxes, &[1, 0], 0.5, 100.0, 100.0);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].class, 1);
        assert!((r[0].score - 0.8).abs() < 1e-5);
        assert_eq!(r[0].xyxy, [25.0, 25.0, 75.0, 75.0]);
        assert_eq!(r[1].class, 0);
        assert_eq!(r[1].xyxy, [0.0, 0.0, 100.0, 100.0]);
        // 阈值提高到 0.85：只剩查询 0
        let r = select(&logits, 3, &boxes, &[1, 0], 0.85, 100.0, 100.0);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].class, 0);
    }
}
