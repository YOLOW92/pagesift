//! 整页流程：检测文字行 → 逐行裁出拉正 → 识别。裁行照 PaddleOCR 的 `get_rotate_crop_image`：
//! 四边形按两组对边的长度定输出宽高，透视（这里用双线性四边形映射）拉成矩形；
//! 高是宽的 1.5 倍以上时当作竖排，转 90°。

use std::path::Path;

use crate::det::Det;
use crate::rec::Rec;
use crate::{Image, Line, Quad};

/// [协议] PaddleOCR `get_rotate_crop_image`：裁出的图高宽比 ≥ 1.5 时转 90°
const ROTATE_RATIO: f32 = 1.5;
/// [策略] 识别置信度低于它的行丢掉（PaddleOCR 的 `text_rec_score_thresh` 缺省 0.0，
/// 这里取 0.5：扫描页上的污点、装饰线常被识别成一两个低置信度的字）
const MIN_SCORE: f32 = 0.5;

/// 检测加识别。
pub struct Engine {
    det: Det,
    rec: Rec,
}

impl Engine {
    pub fn load(det_dir: &Path, rec_dir: &Path) -> Result<Engine, String> {
        Ok(Engine {
            det: Det::load(det_dir)?,
            rec: Rec::load(rec_dir)?,
        })
    }

    /// 识别整页图，按检测给的顺序（从上到下、从左到右）返回各行。
    pub fn run(&self, page: &Image) -> Result<Vec<Line>, String> {
        let mut out = Vec::new();
        for quad in self.det.detect(page)? {
            let crop = crop(page, &quad);
            if crop.width == 0 || crop.height == 0 {
                continue;
            }
            let (text, score) = self.rec.recognize(&crop)?;
            if score >= MIN_SCORE && !text.trim().is_empty() {
                out.push(Line { quad, text, score });
            }
        }
        crate::script::drop_stray_east_asian(&mut out);
        Ok(out)
    }
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// 按四边形裁出一行并拉正；输出与输入同样的通道数。
pub fn crop(img: &Image, q: &Quad) -> Image {
    let [tl, tr, br, bl] = *q;
    let w = dist(tl, tr).max(dist(bl, br)).round().max(1.0) as usize;
    let h = dist(tl, bl).max(dist(tr, br)).round().max(1.0) as usize;
    let c = img.channels;
    let mut data = vec![0u8; w * h * c];
    for y in 0..h {
        let v = (y as f32 + 0.5) / h as f32;
        for x in 0..w {
            let u = (x as f32 + 0.5) / w as f32;
            // 双线性四边形映射：上边、下边各按 u 插值，再按 v 插值
            let top = (tl.0 + (tr.0 - tl.0) * u, tl.1 + (tr.1 - tl.1) * u);
            let bot = (bl.0 + (br.0 - bl.0) * u, bl.1 + (br.1 - bl.1) * u);
            let sx = top.0 + (bot.0 - top.0) * v - 0.5;
            let sy = top.1 + (bot.1 - top.1) * v - 0.5;
            for ch in 0..c {
                data[(y * w + x) * c + ch] = bilinear(img, sx, sy, ch);
            }
        }
    }
    let out = Image {
        width: w,
        height: h,
        channels: c,
        data,
    };
    if h as f32 >= w as f32 * ROTATE_RATIO {
        rotate90(&out)
    } else {
        out
    }
}

/// 双线性取样，图外按边缘像素延伸（OpenCV 的 BORDER_REPLICATE）。
fn bilinear(img: &Image, x: f32, y: f32, c: usize) -> u8 {
    let xm = img.width as f32 - 1.0;
    let ym = img.height as f32 - 1.0;
    let x = x.clamp(0.0, xm);
    let y = y.clamp(0.0, ym);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(img.width - 1), (y0 + 1).min(img.height - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let p = |x: usize, y: usize| f32::from(img.at(x, y, c));
    let top = p(x0, y0) * (1.0 - fx) + p(x1, y0) * fx;
    let bot = p(x0, y1) * (1.0 - fx) + p(x1, y1) * fx;
    (top * (1.0 - fy) + bot * fy).round().clamp(0.0, 255.0) as u8 // [协议] 8 位样本取值 0–255
}

/// 逆时针转 90°（PaddleOCR 用 `np.rot90`）。
fn rotate90(img: &Image) -> Image {
    let (w, h, c) = (img.width, img.height, img.channels);
    let mut data = vec![0u8; w * h * c];
    for y in 0..h {
        for x in 0..w {
            // 原 (x, y) → 新 (y, w − 1 − x)，新图宽 h、高 w
            let (nx, ny) = (y, w - 1 - x);
            for ch in 0..c {
                data[(ny * h + nx) * c + ch] = img.data[(y * w + x) * c + ch];
            }
        }
    }
    Image {
        width: h,
        height: w,
        channels: c,
        data,
    }
}
