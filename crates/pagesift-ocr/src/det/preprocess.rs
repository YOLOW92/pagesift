//! 检测的预处理：按 `limit_side_len` / `limit_type` 定尺寸（对齐到 32 的倍数），双线性缩放，
//! 缩放到 0–1、按通道减均值除标准差，最后排成 BGR 通道顺序。
//! 语义照 transformers 的 `PPOCRV5ServerDetImageProcessor`（Apache-2.0）。

use pagesift_infer::Tensor;

use crate::Image;

/// 尺寸限制的方式（`limit_type`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitType {
    /// 长边超过 `limit_side_len` 才缩小到它。
    Max,
    /// 短边不足 `limit_side_len` 才放大到它。
    Min,
    /// 长边总是缩放到 `limit_side_len`。
    ResizeLong,
}

/// 预处理参数（preprocessor_config.json）。
#[derive(Debug, Clone)]
pub struct Preprocess {
    pub limit_side_len: usize,
    pub limit_type: LimitType,
    pub max_side_limit: Option<usize>,
    pub rescale: f32,
    /// 均值、标准差按输入图的 RGB 通道顺序排。
    pub mean: [f32; 3],
    pub std: [f32; 3],
}

impl Preprocess {
    pub fn from_json(v: &serde_json::Value) -> Result<Preprocess, String> {
        let limit_type = match v["limit_type"].as_str().unwrap_or("max") {
            "max" => LimitType::Max,
            "min" => LimitType::Min,
            "resize_long" => LimitType::ResizeLong,
            t => return Err(format!("不支持的 limit_type：{t}")),
        };
        let triple = |key: &str, def: [f32; 3]| -> Result<[f32; 3], String> {
            match v[key].as_array() {
                None => Ok(def),
                Some(a) if a.len() == 3 => {
                    let mut out = [0f32; 3];
                    for (o, x) in out.iter_mut().zip(a) {
                        *o = x.as_f64().ok_or(format!("{key} 应为数字"))? as f32;
                    }
                    Ok(out)
                }
                Some(_) => Err(format!("{key} 应有 3 个数")),
            }
        };
        if v["do_resize"].as_bool() == Some(false)
            || v["do_rescale"].as_bool() == Some(false)
            || v["do_normalize"].as_bool() == Some(false)
        {
            return Err("只支持缩放、归一化全开的预处理".into());
        }
        // [协议] 缺省值取 PPOCRV5ServerDetImageProcessor 的类属性：limit_side_len 960、
        // max_side_limit 4000、image_mean/std 如下、rescale 1/255
        Ok(Preprocess {
            limit_side_len: v["limit_side_len"].as_u64().unwrap_or(960) as usize,
            limit_type,
            max_side_limit: match &v["max_side_limit"] {
                serde_json::Value::Null => Some(4000),
                x => x.as_u64().map(|n| n as usize),
            },
            rescale: v["rescale_factor"].as_f64().unwrap_or(1.0 / 255.0) as f32,
            mean: triple("image_mean", [0.406, 0.456, 0.485])?,
            std: triple("image_std", [0.225, 0.224, 0.229])?,
        })
    }

    /// 网络输入的 (高, 宽)，对应 `get_image_size`。
    pub fn target_size(&self, h: usize, w: usize) -> (usize, usize) {
        let (hf, wf) = (h as f64, w as f64);
        let lim = self.limit_side_len as f64;
        let ratio = match self.limit_type {
            LimitType::Max if hf.max(wf) > lim => lim / hf.max(wf),
            // [策略] 参考实现只放大短边不足的图；这里大图也缩到短边 = limit_side_len：
            // 300 dpi 整页扫描按原尺寸检测会漏行（手写页实测），且慢十几倍；识别从原图切行，不受影响
            LimitType::Min => lim / hf.min(wf),
            LimitType::ResizeLong => lim / hf.max(wf),
            _ => 1.0,
        };
        // 参考实现用 int() 截断
        let mut rh = (hf * ratio) as usize;
        let mut rw = (wf * ratio) as usize;
        if let Some(m) = self.max_side_limit
            && rh.max(rw) > m
        {
            let r = m as f64 / rh.max(rw) as f64;
            rh = (rh as f64 * r) as usize;
            rw = (rw as f64 * r) as usize;
        }
        // [协议] 对齐到 32 的倍数（四舍五入、至少 32）：骨干总步长 32
        let align = |n: usize| (((n as f64 / 32.0).round_ties_even() as usize) * 32).max(32);
        (align(rh), align(rw))
    }

    /// 整页图 → `[3, H', W']` 的网络输入（通道顺序 B、G、R）。
    pub fn run(&self, img: &Image) -> Tensor {
        let (th, tw) = self.target_size(img.height, img.width);
        // 先把 8 位图转成 RGB 浮点平面（灰度图三个通道相同，即 do_convert_rgb）
        let plane = img.width * img.height;
        let mut rgb = vec![0f32; 3 * plane];
        for y in 0..img.height {
            for x in 0..img.width {
                for c in 0..3 {
                    rgb[c * plane + y * img.width + x] = img.at(x, y, c) as f32;
                }
            }
        }
        let resized = resize_bilinear_aa(&rgb, 3, img.height, img.width, th, tw);
        let out_plane = th * tw;
        let mut out = vec![0f32; 3 * out_plane];
        for c in 0..3 {
            // [协议] 在 RGB 顺序上归一化后取 `[:, [2, 1, 0]]` 换成 BGR
            let dst = 2 - c;
            let (s, m, d) = (self.rescale, self.mean[c], self.std[c]);
            for (o, v) in out[dst * out_plane..(dst + 1) * out_plane]
                .iter_mut()
                .zip(&resized[c * out_plane..(c + 1) * out_plane])
            {
                *o = (v * s - m) / d;
            }
        }
        Tensor::new(vec![3, th, tw], out)
    }
}

/// 一维重采样的权重表：每个输出位置的起始下标和各输入的权重。
struct Taps {
    start: Vec<usize>,
    weights: Vec<Vec<f32>>,
}

/// 抗锯齿双线性（PIL / torchvision `antialias=True` 的做法）：缩小时三角核按比例放宽，
/// 放大时就是半像素对齐的普通双线性。
fn taps(inp: usize, out: usize) -> Taps {
    let scale = inp as f64 / out as f64;
    // [协议] 双线性核半径 1，缩小时乘以 scale
    let support = scale.max(1.0);
    let inv = 1.0 / scale.max(1.0);
    let mut start = Vec::with_capacity(out);
    let mut weights = Vec::with_capacity(out);
    for i in 0..out {
        let center = scale * (i as f64 + 0.5);
        let lo = ((center - support + 0.5).floor().max(0.0)) as usize;
        let hi = ((center + support + 0.5).floor() as usize).min(inp);
        let mut w: Vec<f64> = (lo..hi)
            .map(|j| (1.0 - ((j as f64 - center + 0.5) * inv).abs()).max(0.0))
            .collect();
        let sum: f64 = w.iter().sum();
        if sum > 0.0 {
            for v in &mut w {
                *v /= sum;
            }
        }
        start.push(lo);
        weights.push(w.into_iter().map(|v| v as f32).collect());
    }
    Taps { start, weights }
}

/// 把 `c` 个 `h × w` 平面缩放到 `oh × ow`（先横后纵，可分离）。
pub(crate) fn resize_bilinear_aa(
    src: &[f32],
    c: usize,
    h: usize,
    w: usize,
    oh: usize,
    ow: usize,
) -> Vec<f32> {
    if (h, w) == (oh, ow) {
        return src.to_vec();
    }
    let tx = taps(w, ow);
    let ty = taps(h, oh);
    let mut mid = vec![0f32; c * h * ow];
    for (srow, drow) in src.chunks_exact(w).zip(mid.chunks_exact_mut(ow)) {
        for (x, d) in drow.iter_mut().enumerate() {
            let s0 = tx.start[x];
            *d = tx.weights[x]
                .iter()
                .enumerate()
                .map(|(k, wt)| srow[s0 + k] * wt)
                .sum();
        }
    }
    let mut out = vec![0f32; c * oh * ow];
    for ch in 0..c {
        let m = &mid[ch * h * ow..(ch + 1) * h * ow];
        for y in 0..oh {
            let drow = &mut out[(ch * oh + y) * ow..(ch * oh + y + 1) * ow];
            for (k, wt) in ty.weights[y].iter().enumerate() {
                let srow = &m[(ty.start[y] + k) * ow..(ty.start[y] + k + 1) * ow];
                for (d, s) in drow.iter_mut().zip(srow) {
                    *d += s * wt;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pre(limit_type: LimitType) -> Preprocess {
        Preprocess {
            limit_side_len: 736,
            limit_type,
            max_side_limit: Some(4000),
            rescale: 1.0 / 255.0,
            mean: [0.406, 0.456, 0.485],
            std: [0.225, 0.224, 0.229],
        }
    }

    #[test]
    fn 尺寸规则() {
        let p = pre(LimitType::Min);
        // 短边超过 736：缩到 736（与参考实现不同，见 target_size）
        assert_eq!(p.target_size(2902, 2244), (960, 736));
        // 短边不足：放大到 736
        assert_eq!(p.target_size(500, 300), (1216, 736));
        // 长边超过 4000 再缩回
        assert_eq!(p.target_size(9000, 1000), (4000, 448));
        assert_eq!(pre(LimitType::Max).target_size(1000, 500), (736, 384));
    }

    #[test]
    fn 缩放保常数且放大是双线性() {
        let src = vec![5.0f32; 6 * 4];
        assert!(
            resize_bilinear_aa(&src, 1, 6, 4, 3, 2)
                .iter()
                .all(|v| (v - 5.0).abs() < 1e-5)
        );
        // 1×2 放大到 1×4：半像素对齐的双线性 → 0, 0.25, 0.75, 1
        let up = resize_bilinear_aa(&[0.0, 1.0], 1, 1, 2, 1, 4);
        let want = [0.0, 0.25, 0.75, 1.0];
        for (a, b) in up.iter().zip(want) {
            assert!((a - b).abs() < 1e-6, "{up:?}");
        }
    }

    #[test]
    fn 通道换成_bgr() {
        let img = Image {
            width: 32,
            height: 32,
            channels: 3,
            data: [255u8, 0, 0].repeat(32 * 32),
        };
        let t = pre(LimitType::Max).run(&img);
        let plane = 32 * 32;
        // 第 0 通道是 B（原图 0），第 2 通道是 R（原图 255）
        assert!((t.data[0] - (0.0 - 0.485) / 0.229).abs() < 1e-5);
        assert!((t.data[2 * plane] - (1.0 - 0.406) / 0.225).abs() < 1e-5);
    }
}
