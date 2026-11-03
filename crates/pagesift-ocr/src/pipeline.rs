//! 整页流程：检测文字行 → 逐行裁出拉正 → 识别。裁行照 PaddleOCR 的 `get_rotate_crop_image`：
//! 四边形按两组对边的长度定输出宽高，透视（这里用双线性四边形映射）拉成矩形；
//! 高是宽的 1.5 倍以上时当作竖排，转 90°。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use pagesift_gpu::{GpuBackend, is_oom};
use pagesift_infer::{Tensor, backend};

use crate::det::Det;
use crate::device::{Device, DeviceStatus};
use crate::gpu_run::{GpuRunner, GpuStats};
use crate::layout::Region;
use crate::math::{Math, inside_any};
use crate::rec::Rec;
use crate::{Image, Line, Quad};

/// [协议] PaddleOCR `get_rotate_crop_image`：裁出的图高宽比 ≥ 1.5 时转 90°
const ROTATE_RATIO: f32 = 1.5;
/// [策略] 识别置信度低于它的行丢掉（PaddleOCR 的 `text_rec_score_thresh` 缺省 0.0，
/// 这里取 0.5：扫描页上的污点、装饰线常被识别成一两个低置信度的字）
const MIN_SCORE: f32 = 0.5;

/// 检测加识别。
pub struct Engine {
    det: Arc<Det>,
    rec: Arc<Rec>,
    /// GPU 打不开或出过无法恢复的错时为 None / 被 `gpu_off` 关掉。
    gpu: Option<GpuRunner>,
    gpu_off: AtomicBool,
    status: Mutex<DeviceStatus>,
    /// 透明分流的后端，第一次要用时才校准。
    backend: OnceLock<Option<GpuBackend>>,
    /// 行间公式（可选，模型大、每个公式要几秒）。
    math: Option<Math>,
}

impl Engine {
    /// 在 CPU 上跑，同 `load_with(det_dir, rec_dir, Device::Cpu)`。
    pub fn load(det_dir: &Path, rec_dir: &Path) -> Result<Engine, String> {
        Engine::load_with(det_dir, rec_dir, Device::Cpu)
    }

    /// 载入模型并打开 `device`。GPU 打不开（没有驱动、PTX 编译失败、`PAGESIFT_GPU=off` 等）不算
    /// 错误：退回 CPU，原因见 [`Engine::device_status`]。只有模型文件的问题才返回 `Err`。
    pub fn load_with(det_dir: &Path, rec_dir: &Path, device: Device) -> Result<Engine, String> {
        let det = Arc::new(Det::load(det_dir)?);
        let rec = Arc::new(Rec::load(rec_dir)?);
        let (gpu, status) = match device {
            Device::Cpu => (None, DeviceStatus::cpu()),
            Device::Gpu { index } => match GpuRunner::start(index, det.clone(), rec.clone()) {
                Ok(g) => {
                    let info = g.worker().info().clone();
                    (Some(g), DeviceStatus::on_gpu(index, info))
                }
                Err(why) => (None, DeviceStatus::fallback(index, why)),
            },
        };
        Ok(Engine {
            det,
            rec,
            gpu,
            gpu_off: AtomicBool::new(false),
            status: Mutex::new(status),
            backend: OnceLock::new(),
            math: None,
        })
    }

    /// 同时识别行间公式，输出 `$$…$$`。
    pub fn with_math(mut self, math: Math) -> Engine {
        self.math = Some(math);
        self
    }

    /// 实际用的设备、是否退回了 CPU、退回原因。
    pub fn device_status(&self) -> DeviceStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// GPU 的显存和批次统计；没在用 GPU 时为 None。
    pub fn gpu_stats(&self) -> Option<GpuStats> {
        self.gpu.as_ref().map(GpuRunner::stats)
    }

    /// 在 `f` 里，当前线程直接调 `pagesift_infer::ops` 和 `nn` 层的模型（公式、版面）按实测开销把大算子
    /// 分流到 GPU。没有 GPU 时就是直接调 `f`。第一次调用会校准几十毫秒。
    pub fn with_gpu_ops<R>(&self, f: impl FnOnce() -> R) -> R {
        let b = self.backend.get_or_init(|| {
            let g = self.gpu.as_ref()?;
            match GpuBackend::new(g.worker().clone()) {
                Ok(b) => Some(b),
                Err(e) => {
                    self.note_fallback(&format!("GPU calibration failed: {e}"), false);
                    None
                }
            }
        });
        match (b, self.gpu_active()) {
            (Some(b), Some(_)) => backend::scoped(b, f),
            _ => f(),
        }
    }

    fn gpu_active(&self) -> Option<&GpuRunner> {
        self.gpu
            .as_ref()
            .filter(|_| !self.gpu_off.load(Ordering::Relaxed))
    }

    /// 记下退回的原因；`permanent` 时之后都不再用 GPU。
    fn note_fallback(&self, why: &str, permanent: bool) {
        let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
        s.fallback = Some(why.to_owned());
        if permanent {
            self.gpu_off.store(true, Ordering::Relaxed);
            s.actual = Device::Cpu;
        }
    }

    /// 识别整页图，按检测给的顺序（从上到下、从左到右）返回各行；有公式模型时公式排在最后。
    /// 版面和公式模型直接调 `ops`，经 [`Engine::with_gpu_ops`] 把大算子分流到 GPU。
    pub fn run(&self, page: &Image) -> Result<Vec<Line>, String> {
        let formulas = match &self.math {
            Some(m) => self.with_gpu_ops(|| m.formula_boxes(page))?,
            None => Vec::new(),
        };
        let mut out = self.run_text(page, &formulas)?;
        crate::script::drop_stray_east_asian(&mut out);
        if let Some(m) = &self.math {
            for r in &formulas {
                out.extend(self.with_gpu_ops(|| m.recognize(page, r))?);
            }
        }
        Ok(out)
    }

    /// 文字行：优先 GPU，GPU 出了显存不足以外的错就从此退回 CPU。
    fn run_text(&self, page: &Image, formulas: &[Region]) -> Result<Vec<Line>, String> {
        if let Some(g) = self.gpu_active() {
            match self.run_gpu(g, page, formulas) {
                Ok(lines) => return Ok(lines),
                // 模型输入本身的问题（空图等）换设备也没用，别的都是 GPU 的问题
                Err(RunError::Input(e)) => return Err(e),
                Err(RunError::Gpu(e)) => self.note_fallback(&e, true),
            }
        }
        self.run_cpu(page, formulas)
    }

    fn run_cpu(&self, page: &Image, formulas: &[Region]) -> Result<Vec<Line>, String> {
        let mut out = Vec::new();
        for quad in self.det.detect(page)? {
            if inside_any(&quad, formulas) {
                continue;
            }
            let crop = crop(page, &quad);
            if crop.width == 0 || crop.height == 0 {
                continue;
            }
            let (text, score) = self.rec.recognize(&crop)?;
            if score >= MIN_SCORE && !text.trim().is_empty() {
                out.push(Line {
                    quad,
                    text,
                    score,
                    formula: false,
                });
            }
        }
        Ok(out)
    }

    /// 流程和 [`Engine::run_cpu`] 一样，只是检测网络在工人上跑、各行拼批。显存不够的页、行单独退回 CPU。
    fn run_gpu(
        &self,
        g: &GpuRunner,
        page: &Image,
        formulas: &[Region],
    ) -> Result<Vec<Line>, RunError> {
        let _page = g.page();
        let x = self.det.preprocess(page).map_err(RunError::Input)?;
        let (prob, w, h) = match g.prob_map(&x) {
            Ok(r) => r,
            Err(e) if is_oom(&e) => {
                self.note_fallback(&e, false);
                self.det.prob_map(page).map_err(RunError::Input)?
            }
            Err(e) => return Err(RunError::Gpu(e)),
        };
        drop(x);
        let quads: Vec<Quad> = self
            .det
            .boxes_from_prob(&prob, w, h, page.width, page.height)
            .into_iter()
            .map(|(q, _)| q)
            .filter(|q| !inside_any(q, formulas))
            .collect();
        drop(prob);
        let inputs = self.crop_inputs(page, &quads).map_err(RunError::Input)?;
        let (kept, xs): (Vec<usize>, Vec<Tensor>) = inputs.into_iter().unzip();
        let digests = g.recognize_many(xs);
        let mut out = Vec::new();
        for (i, r) in kept.into_iter().zip(digests) {
            let quad = quads[i];
            let (text, score) = match r {
                Ok(rows) => self.rec.decode(&rows),
                Err(e) if is_oom(&e) => {
                    self.note_fallback(&e, false);
                    self.rec
                        .recognize(&crop(page, &quad))
                        .map_err(RunError::Input)?
                }
                Err(e) => return Err(RunError::Gpu(e)),
            };
            if score >= MIN_SCORE && !text.trim().is_empty() {
                out.push(Line {
                    quad,
                    text,
                    score,
                    formula: false,
                });
            }
        }
        Ok(out)
    }

    /// 裁出各行并预处理成网络输入，多线程。返回 (四边形下标, 输入)，跳过空的裁剪。
    fn crop_inputs(&self, page: &Image, quads: &[Quad]) -> Result<Vec<(usize, Tensor)>, String> {
        let threads = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(quads.len().max(1));
        let chunk = quads.len().div_ceil(threads).max(1);
        let one = |base: usize, part: &[Quad]| -> Result<Vec<(usize, Tensor)>, String> {
            let mut v = Vec::with_capacity(part.len());
            for (i, q) in part.iter().enumerate() {
                let c = crop(page, q);
                if c.width > 0 && c.height > 0 {
                    v.push((base + i, self.rec.preprocess(&c)?));
                }
            }
            Ok(v)
        };
        std::thread::scope(|s| {
            let hs: Vec<_> = quads
                .chunks(chunk)
                .enumerate()
                .map(|(i, part)| s.spawn(move || one(i * chunk, part)))
                .collect();
            let mut out = Vec::with_capacity(quads.len());
            for h in hs {
                out.extend(
                    h.join()
                        .map_err(|_| "a crop thread panicked".to_owned())??,
                );
            }
            Ok(out)
        })
    }
}

/// 整页跑 GPU 时的错误：分清是输入的问题还是 GPU 的问题。
enum RunError {
    Input(String),
    Gpu(String),
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
