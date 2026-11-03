//! 公式图预处理，照 transformers 的 `image_processing_pp_formulanet.py`（Apache-2.0）：
//!
//! 8 位 RGB → 裁白边（`crop_margin`）→ 短边缩放到 768（双线性、抗锯齿）→ `thumbnail`
//! 把长边缩到 768（双三次、抗锯齿）→ 居中补 0 到 768×768 → v/255 → (v − 0.7931)/0.1738。
//!
//! 两次缩放之间、补边之前都是 8 位图（torchvision 对 uint8 张量插值后舍入回 uint8）。
//! 补的是 0（黑），归一化后约 −4.56；参考实现如此，这里照做。

use pagesift_infer::Tensor;

use crate::Image;

/// [协议] processor_config.json 的 `size`：768×768
pub const SIZE: usize = 768;
/// [协议] `image_mean`，三通道相同
const MEAN: f32 = 0.7931;
/// [协议] `image_std`，三通道相同
const STD: f32 = 0.1738;
/// [协议] `crop_margin(gray_threshold=200)`：拉伸到 0–255 后低于 200 的算内容
const GRAY_THRESHOLD: f32 = 200.0;
/// [协议] torchvision `rgb_to_grayscale` 的系数（ITU-R 601-2 亮度）
const LUMA: [f32; 3] = [0.2989, 0.587, 0.114];
/// [策略] 第一次缩放（短边到 768）的中间图最多这么多像素。参考实现对极扁的图（宽高比上百）
/// 会生成上亿像素的中间图；超过时把中间图的长边压到这个上限内（仍不小于 768），
/// 之后的 thumbnail 反正要缩到 768，只影响抗锯齿核的取样，不影响输出尺寸。
const MAX_MID_PIXELS: usize = 1 << 26;

/// 一张 8 位单通道平面。
struct Plane {
    w: usize,
    h: usize,
    data: Vec<u8>,
}

/// 预处理成 `[3, 768, 768]`。空图给全补边的输出。
pub fn preprocess(img: &Image) -> Tensor {
    let n = SIZE * SIZE;
    // [协议] 补边值 0，归一化后为 (0 − mean)/std
    let pad_val = -MEAN / STD;
    let mut out = vec![pad_val; 3 * n];
    if img.width == 0 || img.height == 0 {
        return Tensor::new(vec![3, SIZE, SIZE], out);
    }
    let (x0, y0, cw, ch) = crop_box(img);
    let (mh, mw) = mid_size(ch, cw);
    let (th, tw) = thumb_size(mh, mw);
    // 灰度图三个通道相同，只算一次
    let planes = if img.channels == 1 { 1 } else { 3 };
    let (top, left) = ((SIZE - th) / 2, (SIZE - tw) / 2);
    for c in 0..planes {
        let mut p = Plane {
            w: cw,
            h: ch,
            data: Vec::with_capacity(cw * ch),
        };
        for y in y0..y0 + ch {
            for x in x0..x0 + cw {
                p.data.push(img.at(x, y, c));
            }
        }
        let p = resize(&p, mh, mw, Filter::Triangle);
        let p = resize(&p, th, tw, Filter::Cubic);
        let dst = &mut out[c * n..(c + 1) * n];
        for (y, row) in p.data.chunks_exact(tw).enumerate() {
            let at = (top + y) * SIZE + left;
            for (o, &v) in dst[at..at + tw].iter_mut().zip(row) {
                // [协议] rescale 1/255 后按 mean/std 归一化
                *o = (f32::from(v) / 255.0 - MEAN) / STD;
            }
        }
    }
    if planes == 1 {
        let (first, rest) = out.split_at_mut(n);
        rest[..n].copy_from_slice(first);
        rest[n..].copy_from_slice(first);
    }
    Tensor::new(vec![3, SIZE, SIZE], out)
}

/// 裁白边的框 (x0, y0, 宽, 高)：灰度拉伸到 0–255 后小于 200 的像素的外接矩形；
/// 全图同一灰度时不裁。
fn crop_box(img: &Image) -> (usize, usize, usize, usize) {
    let (w, h) = (img.width, img.height);
    let gray: Vec<u8> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| {
            let l: f32 = (0..3).map(|c| f32::from(img.at(x, y, c)) * LUMA[c]).sum();
            // [协议] torchvision 对整数图的灰度结果舍入后转回 uint8
            l.round_ties_even().clamp(0.0, 255.0) as u8
        })
        .collect();
    let (lo, hi) = gray
        .iter()
        .fold((u8::MAX, u8::MIN), |(a, b), &v| (a.min(v), b.max(v)));
    if lo == hi {
        return (0, 0, w, h);
    }
    let range = f32::from(hi - lo);
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (i, &g) in gray.iter().enumerate() {
        // [协议] `(data − min)/(max − min)·255 < 200`
        if f32::from(g - lo) / range * 255.0 < GRAY_THRESHOLD {
            let (x, y) = (i % w, i / w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    // 最暗的像素拉伸后是 0，一定有内容像素
    (x0, y0, x1 - x0 + 1, y1 - y0 + 1)
}

/// 第一次缩放的目标 (高, 宽)：短边到 768，长边 ⌊768·长/短⌋（`get_resize_output_image_size`，
/// `default_to_square=False`）；长边超过像素上限时压到上限内。
fn mid_size(h: usize, w: usize) -> (usize, usize) {
    let (short, long) = if w <= h { (w, h) } else { (h, w) };
    let cap = (MAX_MID_PIXELS / SIZE).max(SIZE);
    let new_long = (SIZE as f64 * long as f64 / short as f64) as usize;
    let new_long = new_long.min(cap);
    if w <= h {
        (new_long, SIZE)
    } else {
        (SIZE, new_long)
    }
}

/// `thumbnail` 的目标 (高, 宽)：两边都不超过 768，长边缩到 768 时短边 ⌊短·768/长⌋。
fn thumb_size(h: usize, w: usize) -> (usize, usize) {
    let (mut th, mut tw) = (h.min(SIZE), w.min(SIZE));
    if (th, tw) == (h, w) {
        return (h, w);
    }
    if h > w {
        tw = w * th / h;
    } else if w > h {
        th = h * tw / w;
    }
    (th.max(1), tw.max(1))
}

/// 重采样核。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Filter {
    /// 双线性（三角核，半径 1）
    Triangle,
    /// 双三次（Keys 核 a = −0.5，半径 2；PIL 与 PyTorch 抗锯齿版用的参数）
    Cubic,
}

impl Filter {
    fn support(self) -> f64 {
        match self {
            Filter::Triangle => 1.0,
            // [协议] 双三次核半径 2
            Filter::Cubic => 2.0,
        }
    }

    fn weight(self, x: f64) -> f64 {
        let x = x.abs();
        match self {
            Filter::Triangle => (1.0 - x).max(0.0),
            Filter::Cubic => {
                // [协议] PyTorch `upsample_aa` 的 bicubic 用 a = −0.5（与 PIL 相同）
                let a = -0.5;
                if x < 1.0 {
                    ((a + 2.0) * x - (a + 3.0)) * x * x + 1.0
                } else if x < 2.0 {
                    // [推导] Keys 三次卷积核在 1 ≤ |x| < 2 段：a·(|x|³ − 5|x|² + 8|x| − 4)
                    (((x - 5.0) * x + 8.0) * x - 4.0) * a
                } else {
                    0.0
                }
            }
        }
    }
}

/// 一维重采样表：每个输出位置的起点和各输入的权重（已归一化）。
struct Taps {
    start: Vec<usize>,
    weights: Vec<Vec<f32>>,
}

/// 抗锯齿重采样表（PIL / PyTorch `antialias=True` 的做法）：缩小时核按比例放宽，
/// 放大时就是半像素对齐的普通插值。
fn taps(inp: usize, out: usize, f: Filter) -> Taps {
    let scale = inp as f64 / out as f64;
    let support = f.support() * scale.max(1.0);
    let inv = 1.0 / scale.max(1.0);
    let mut start = Vec::with_capacity(out);
    let mut weights = Vec::with_capacity(out);
    for i in 0..out {
        let center = scale * (i as f64 + 0.5);
        let lo = (center - support + 0.5).floor().max(0.0) as usize;
        let hi = ((center + support + 0.5).floor() as usize).min(inp);
        let mut w: Vec<f64> = (lo..hi)
            .map(|j| f.weight((j as f64 - center + 0.5) * inv))
            .collect();
        let sum: f64 = w.iter().sum();
        if sum != 0.0 {
            for v in &mut w {
                *v /= sum;
            }
        }
        start.push(lo);
        weights.push(w.into_iter().map(|v| v as f32).collect());
    }
    Taps { start, weights }
}

/// 舍入回 8 位（先截到 0–255 再四舍六入五成双，同 torch 的 `clamp_` + `round_`）。
fn to_u8(v: f32) -> u8 {
    v.clamp(0.0, 255.0).round_ties_even() as u8
}

/// 把平面缩放到 `oh × ow`：先横后纵（PyTorch 可分离抗锯齿插值的顺序）。
/// 双线性时横向结果先舍入回 8 位（PyTorch 的 uint8 专用路径与 PIL 一样）；双三次在浮点里做完两遍再舍入
/// （torchvision 对 uint8 双三次先转 float、插值后截断舍入）。
fn resize(p: &Plane, oh: usize, ow: usize, f: Filter) -> Plane {
    if (p.h, p.w) == (oh, ow) {
        return Plane {
            w: ow,
            h: oh,
            data: p.data.clone(),
        };
    }
    let tx = taps(p.w, ow, f);
    let ty = taps(p.h, oh, f);
    let round_mid = f == Filter::Triangle;
    let mut mid = vec![0f32; p.h * ow];
    for (srow, drow) in p.data.chunks_exact(p.w).zip(mid.chunks_exact_mut(ow)) {
        for (x, d) in drow.iter_mut().enumerate() {
            let s0 = tx.start[x];
            let v: f32 = tx.weights[x]
                .iter()
                .zip(&srow[s0..])
                .map(|(wt, &s)| f32::from(s) * wt)
                .sum();
            *d = if round_mid { f32::from(to_u8(v)) } else { v };
        }
    }
    let mut acc = vec![0f32; ow];
    let mut data = Vec::with_capacity(oh * ow);
    for y in 0..oh {
        acc.fill(0.0);
        for (k, wt) in ty.weights[y].iter().enumerate() {
            let r = ty.start[y] + k;
            for (a, s) in acc.iter_mut().zip(&mid[r * ow..(r + 1) * ow]) {
                *a += s * wt;
            }
        }
        data.extend(acc.iter().map(|&v| to_u8(v)));
    }
    Plane { w: ow, h: oh, data }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray(w: usize, h: usize, f: impl Fn(usize, usize) -> u8) -> Image {
        let data = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| f(x, y))
            .collect();
        Image {
            width: w,
            height: h,
            channels: 1,
            data,
        }
    }

    #[test]
    fn 裁白边取暗像素的外接框() {
        let img = gray(50, 40, |x, y| {
            if (10..20).contains(&x) && (5..30).contains(&y) {
                0
            } else {
                255
            }
        });
        assert_eq!(crop_box(&img), (10, 5, 10, 25));
        // 全白不裁
        assert_eq!(crop_box(&gray(7, 3, |_, _| 255)), (0, 0, 7, 3));
    }

    #[test]
    fn 尺寸规则照参考实现() {
        // 宽图：短边（高）到 768，长边 ⌊768·400/100⌋；再缩回长边 768
        assert_eq!(mid_size(100, 400), (768, 3072));
        assert_eq!(thumb_size(768, 3072), (192, 768));
        // 高图
        assert_eq!(mid_size(300, 200), (1152, 768));
        assert_eq!(thumb_size(1152, 768), (768, 512));
        // 方图不再缩
        assert_eq!(mid_size(10, 10), (768, 768));
        assert_eq!(thumb_size(768, 768), (768, 768));
        // 极扁的图中间图长边有上限，仍不小于 768
        let (h, w) = mid_size(1, 100_000);
        assert_eq!(h, 768);
        assert!(w * h <= MAX_MID_PIXELS && w >= 768);
    }

    #[test]
    fn 缩放保常数_放大是半像素双线性() {
        let p = Plane {
            w: 5,
            h: 3,
            data: vec![77; 15],
        };
        for f in [Filter::Triangle, Filter::Cubic] {
            assert!(resize(&p, 7, 2, f).data.iter().all(|&v| v == 77));
        }
        let t = taps(2, 4, Filter::Triangle);
        // 1×2 放大到 1×4：0, 0.25, 0.75, 1 倍的右像素
        let right: Vec<f32> = (0..4)
            .map(|i| {
                let w = &t.weights[i];
                (0..w.len())
                    .map(|k| if t.start[i] + k == 1 { w[k] } else { 0.0 })
                    .sum()
            })
            .collect();
        for (a, b) in right.iter().zip([0.0, 0.25, 0.75, 1.0]) {
            assert!((a - b).abs() < 1e-6, "{right:?}");
        }
    }

    #[test]
    fn 双三次核在整数点插值() {
        assert!((Filter::Cubic.weight(0.0) - 1.0).abs() < 1e-12);
        for x in [1.0, 2.0, -1.0, 3.0] {
            assert!(Filter::Cubic.weight(x).abs() < 1e-12);
        }
        // 权重和为 1 的性质由归一化保证；缩小时每个输出的权重和为 1
        let t = taps(100, 7, Filter::Cubic);
        for w in &t.weights {
            assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn 输出形状_居中补边_归一化() {
        // 100×25 的白底黑块：裁边后 80×20，缩成 768×192，上下各补 288 行
        let img = gray(100, 25, |x, y| {
            if (10..90).contains(&x) && (2..22).contains(&y) {
                0
            } else {
                255
            }
        });
        let t = preprocess(&img);
        assert_eq!(t.shape, vec![3, SIZE, SIZE]);
        let pad = -MEAN / STD;
        let black = (0.0 - MEAN) / STD;
        assert_eq!(t.data[0], pad);
        // 中间一行全是内容（黑）
        let mid = SIZE / 2 * SIZE;
        assert!(
            t.data[mid..mid + SIZE]
                .iter()
                .all(|&v| (v - black).abs() < 1e-6)
        );
        // 第 287 行是补边，第 288 行是内容
        assert_eq!(t.data[287 * SIZE + 5], pad);
        assert!((t.data[288 * SIZE + 5] - black).abs() < 1e-6);
        // 三个通道相同
        let n = SIZE * SIZE;
        assert_eq!(t.data[..n], t.data[n..2 * n]);
        assert!(t.data.iter().all(|v| v.is_finite()));
    }
}
