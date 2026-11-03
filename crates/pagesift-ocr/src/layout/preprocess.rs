//! 预处理：整页图拉伸到固定尺寸（不保长宽比），双三次插值（不抗锯齿），取整回 8 位，再缩放到 0–1、
//! 减均值除标准差。语义照 transformers 的 `PPDocLayoutV3ImageProcessor`（Apache-2.0）：torchvision 的
//! `resize(antialias=False)` 走 `F.interpolate(mode="bicubic", align_corners=False)`。

use pagesift_infer::Tensor;

use crate::Image;

/// [协议] 网络输入是 RGB 三通道
const CHANNELS: usize = 3;
/// [协议] `PPDocLayoutV3ImageProcessor.size` 的缺省值
const DEFAULT_SIZE: u64 = 800;
/// [协议] 8 位像素的最大值
const MAX_U8: f32 = 255.0;
/// [协议] 双三次插值每个方向 4 个抽头
const TAPS: usize = 4;

/// 预处理参数（preprocessor_config.json）。
#[derive(Debug, Clone)]
pub struct Preprocess {
    pub height: usize,
    pub width: usize,
    pub rescale: f32,
    pub mean: [f32; CHANNELS],
    pub std: [f32; CHANNELS],
}

/// [协议] PIL 的重采样编号：3 是 BICUBIC
const RESAMPLE_BICUBIC: u64 = 3;

impl Preprocess {
    pub fn from_json(v: &serde_json::Value) -> Result<Preprocess, String> {
        if v["do_resize"].as_bool() == Some(false)
            || v["do_rescale"].as_bool() == Some(false)
            || v["do_normalize"].as_bool() == Some(false)
        {
            return Err("只支持缩放、归一化全开的预处理".into());
        }
        if v["resample"].as_u64().unwrap_or(RESAMPLE_BICUBIC) != RESAMPLE_BICUBIC {
            return Err("只支持双三次插值".into());
        }
        let triple = |key: &str, def: f32| -> Result<[f32; CHANNELS], String> {
            match v[key].as_array() {
                None => Ok([def; CHANNELS]),
                Some(a) if a.len() == CHANNELS => {
                    let mut out = [0f32; CHANNELS];
                    for (o, x) in out.iter_mut().zip(a) {
                        *o = x.as_f64().ok_or(format!("{key} 应为数字"))? as f32;
                    }
                    Ok(out)
                }
                Some(_) => Err(format!("{key} 应有 {CHANNELS} 个数")),
            }
        };
        // 缺省值取 PPDocLayoutV3ImageProcessor 的类属性：均值 0、标准差 1、1/255
        Ok(Preprocess {
            height: v["size"]["height"].as_u64().unwrap_or(DEFAULT_SIZE) as usize,
            width: v["size"]["width"].as_u64().unwrap_or(DEFAULT_SIZE) as usize,
            rescale: v["rescale_factor"]
                .as_f64()
                .unwrap_or(1.0 / f64::from(MAX_U8)) as f32,
            mean: triple("image_mean", 0.0)?,
            std: triple("image_std", 1.0)?,
        })
    }

    /// 整页图 → `[3, H, W]`，RGB 顺序；灰度图三个通道相同。
    pub fn run(&self, img: &Image) -> Tensor {
        let planes = if img.channels == 1 { 1 } else { CHANNELS };
        let (h, w) = (self.height, self.width);
        let mut out = vec![0f32; CHANNELS * h * w];
        for c in 0..planes {
            let src: Vec<f32> = (0..img.height * img.width)
                .map(|i| f32::from(img.data[i * img.channels + c]))
                .collect();
            let r = resize_bicubic(&src, img.height, img.width, h, w);
            let dst = &mut out[c * h * w..(c + 1) * h * w];
            for (d, v) in dst.iter_mut().zip(r) {
                // torchvision 对 8 位图：插值后四舍五入（到偶）并截到 0–255，再做后面的归一化
                let v = v.round_ties_even().clamp(0.0, MAX_U8);
                *d = (v * self.rescale - self.mean[c]) / self.std[c];
            }
        }
        if planes == 1 {
            let (first, rest) = out.split_at_mut(h * w);
            rest[..h * w].copy_from_slice(first);
            rest[h * w..].copy_from_slice(first);
        }
        Tensor::new(vec![CHANNELS, h, w], out)
    }
}

/// 一个输出坐标的四个抽头：起始下标（可越界，取值时截到边上）和权重。
fn cubic_taps(input: usize, output: usize) -> Vec<(isize, [f32; TAPS])> {
    let scale = input as f32 / output as f32;
    (0..output)
        .map(|o| {
            // align_corners=False：源坐标 = (目标 + ½)·比例 − ½；双三次不把负数截到 0
            let src = (o as f32 + 0.5) * scale - 0.5;
            let floor = src.floor();
            (floor as isize - 1, cubic_weights(src - floor))
        })
        .collect()
}

/// PyTorch `upsample_bicubic2d` 的 `get_cubic_upsample_coefficients`，t 是到左邻点的距离。
fn cubic_weights(t: f32) -> [f32; TAPS] {
    [
        cubic_outer(t + 1.0),
        cubic_inner(t),
        cubic_inner(1.0 - t),
        cubic_outer(2.0 - t),
    ]
}

/// [协议] PyTorch 双三次核的参数 A = −0.75（OpenCV 的 INTER_CUBIC 也是）
const CUBIC_A: f32 = -0.75;

/// |x| ≤ 1 段：(A + 2)x³ − (A + 3)x² + 1。
fn cubic_inner(x: f32) -> f32 {
    // [协议] PyTorch `cubic_convolution1` 的系数
    ((CUBIC_A + 2.0) * x - (CUBIC_A + 3.0)) * x * x + 1.0
}

/// 1 < |x| < 2 段：Ax³ − 5Ax² + 8Ax − 4A。
fn cubic_outer(x: f32) -> f32 {
    // [协议] PyTorch `cubic_convolution2` 的系数
    ((CUBIC_A * x - 5.0 * CUBIC_A) * x + 8.0 * CUBIC_A) * x - 4.0 * CUBIC_A
}

/// 把一个 `h × w` 平面缩放到 `oh × ow`，先横后纵；越界的抽头取边上的像素。
fn resize_bicubic(src: &[f32], h: usize, w: usize, oh: usize, ow: usize) -> Vec<f32> {
    let tx = cubic_taps(w, ow);
    let ty = cubic_taps(h, oh);
    let at = |i: isize, n: usize| i.clamp(0, n as isize - 1) as usize;
    // 只有被纵向抽头用到的行才做横向一遍：大图缩小时省掉大部分行
    let mut used = vec![false; h];
    for &(s, _) in &ty {
        for k in 0..TAPS as isize {
            used[at(s + k, h)] = true;
        }
    }
    let mut mid = vec![0f32; h * ow];
    for y in (0..h).filter(|&y| used[y]) {
        let row = &src[y * w..(y + 1) * w];
        for (d, (s, wt)) in mid[y * ow..(y + 1) * ow].iter_mut().zip(&tx) {
            *d = (0..TAPS)
                .map(|k| row[at(s + k as isize, w)] * wt[k])
                .sum::<f32>();
        }
    }
    let mut out = vec![0f32; oh * ow];
    for (drow, (s, wt)) in out.chunks_exact_mut(ow).zip(&ty) {
        for (k, wk) in wt.iter().enumerate() {
            let y = at(s + k as isize, h);
            let srow = &mid[y * ow..(y + 1) * ow];
            for (d, v) in drow.iter_mut().zip(srow) {
                *d += v * wk;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 权重和为一且在节点上取原值() {
        for t in [0.0f32, 0.25, 0.5, 0.9] {
            let s: f32 = cubic_weights(t).iter().sum();
            assert!((s - 1.0).abs() < 1e-5, "{t}: {s}");
        }
        let w0 = cubic_weights(0.0);
        assert!(w0[0].abs() < 1e-6 && (w0[1] - 1.0).abs() < 1e-6 && w0[2].abs() < 1e-6);
    }

    #[test]
    fn 同尺寸不变_常数保持() {
        let src: Vec<f32> = (0..16).map(|v| v as f32).collect();
        let same = resize_bicubic(&src, 4, 4, 4, 4);
        for (a, b) in same.iter().zip(&src) {
            assert!((a - b).abs() < 1e-5);
        }
        let flat = resize_bicubic(&[7.0; 30], 5, 6, 3, 2);
        assert!(flat.iter().all(|v| (v - 7.0).abs() < 1e-4));
    }

    #[test]
    fn 灰度图三通道相同且缩放到一() {
        let p = Preprocess {
            height: 4,
            width: 4,
            rescale: 1.0 / 255.0,
            mean: [0.0; 3],
            std: [1.0; 3],
        };
        let img = Image {
            width: 8,
            height: 8,
            channels: 1,
            data: vec![255; 64],
        };
        let t = p.run(&img);
        assert_eq!(t.shape, [3, 4, 4]);
        assert!(t.data.iter().all(|v| (v - 1.0).abs() < 1e-6));
    }

    #[test]
    fn 彩色图通道不混() {
        let p = Preprocess {
            height: 2,
            width: 2,
            rescale: 1.0 / 255.0,
            mean: [0.0; 3],
            std: [1.0; 3],
        };
        let img = Image {
            width: 2,
            height: 2,
            channels: 3,
            data: [255u8, 0, 51].repeat(4),
        };
        let t = p.run(&img);
        assert!((t.data[0] - 1.0).abs() < 1e-6);
        assert!(t.data[4].abs() < 1e-6);
        assert!((t.data[8] - 0.2).abs() < 1e-6);
    }
}
