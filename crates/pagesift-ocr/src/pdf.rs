//! 把 [`Engine`] 接到 PDF 的扫描页接口上：第一张扫描图来时才准备模型（缺的先下载，由调用方提供
//! 下载函数，本 crate 不碰网络）、才载入，没遇到扫描页的文档一点开销都没有。

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use pagesift_pdf::{OcrInfo, OcrLine, PageOcr};

use crate::device::{Device, ModelChoice, Quality, select_models};
use crate::math::Math;
use crate::models::{FORMULA, LAYOUT, Model, SMALL_REC};
use crate::pipeline::Engine;
use crate::{Image, Line};

/// 把模型下载到 `<缓存根>/<仓库名>`（只在文件不全时调用），边下边校验。
pub type Fetch = Arc<dyn Fn(&Model) -> Result<(), String> + Send + Sync>;

/// 模型和设备怎么定。
pub struct PdfOcrConfig {
    pub root: PathBuf,
    pub quality: Quality,
    pub device: Device,
    /// 调用方明确要了 GPU（`cuda`、`cuda:N`）。`auto` 时本机没有 N 卡不算退回，不报 `gpu_fallback`。
    pub gpu_required: bool,
    /// 也识别行间公式（多下载 860 MB 模型，每个公式要几秒）。
    pub formulas: bool,
    /// 没有时缺模型就报错（离线环境，提前用 `pagesift models fetch` 下好）。
    pub fetch: Option<Fetch>,
}

struct Loaded {
    engine: Engine,
    choice: ModelChoice,
}

/// 懒加载的 PDF OCR 引擎。
pub struct PdfOcr {
    config: PdfOcrConfig,
    loaded: OnceLock<Result<Loaded, String>>,
}

impl PdfOcr {
    pub fn new(config: PdfOcrConfig) -> Self {
        Self {
            config,
            loaded: OnceLock::new(),
        }
    }

    /// 模型目录直接给（测评用）：立刻载入，不走缓存和下载。
    pub fn from_dirs(
        det_dir: PathBuf,
        rec_dir: PathBuf,
        math_dirs: Option<(PathBuf, PathBuf)>,
        device: Device,
    ) -> Result<Self, String> {
        let this = Self::new(PdfOcrConfig {
            root: PathBuf::new(),
            quality: Quality::Fast,
            device,
            gpu_required: false,
            formulas: false,
            fetch: None,
        });
        let mut engine = Engine::load_with(&det_dir, &rec_dir, device)?;
        if let Some((layout, formula)) = math_dirs {
            engine = engine.with_math(Math::load(&layout, &formula)?);
        }
        let choice = select_models(Quality::Fast, device);
        let _ = this.loaded.set(Ok(Loaded { engine, choice }));
        Ok(this)
    }

    fn load(&self) -> Result<Loaded, String> {
        let c = &self.config;
        let choice = select_models(c.quality, c.device);
        let math: &[Model] = if c.formulas { &[LAYOUT, FORMULA] } else { &[] };
        for m in [&choice.det, &choice.rec].into_iter().chain(math) {
            if m.present(&c.root) {
                continue;
            }
            match &c.fetch {
                Some(fetch) => fetch(m)?,
                None => {
                    return Err(format!(
                        "model {} is missing from {}; run `pagesift models fetch`",
                        m.repo,
                        c.root.display()
                    ));
                }
            }
        }
        let mut engine =
            Engine::load_with(&choice.det.dir(&c.root), &choice.rec.dir(&c.root), c.device)?;
        if c.formulas {
            engine = engine.with_math(Math::load(&LAYOUT.dir(&c.root), &FORMULA.dir(&c.root))?);
        }
        Ok(Loaded { engine, choice })
    }
}

impl std::fmt::Debug for PdfOcr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfOcr")
            .field("quality", &self.config.quality)
            .field("device", &self.config.device)
            .finish_non_exhaustive()
    }
}

/// 外框（各角 x、y 的最小、最大）。
fn to_ocr_line(l: Line) -> OcrLine {
    let xs = l.quad.map(|p| p.0);
    let ys = l.quad.map(|p| p.1);
    let min = |v: [f32; 4]| v.into_iter().fold(f32::MAX, f32::min);
    let max = |v: [f32; 4]| v.into_iter().fold(f32::MIN, f32::max);
    OcrLine {
        x0: min(xs),
        y0: min(ys),
        x1: max(xs),
        y1: max(ys),
        formula: l.formula,
        text: l.text,
        score: l.score,
    }
}

impl PageOcr for PdfOcr {
    fn recognize(&self, width: u32, height: u32, gray: &[u8]) -> Result<Vec<OcrLine>, String> {
        let loaded = self
            .loaded
            .get_or_init(|| self.load())
            .as_ref()
            .map_err(Clone::clone)?;
        let img = Image {
            width: width as usize,
            height: height as usize,
            channels: 1,
            data: gray.to_vec(),
        };
        Ok(loaded
            .engine
            .run(&img)?
            .into_iter()
            .map(to_ocr_line)
            .collect())
    }

    fn info(&self) -> OcrInfo {
        let mut info = OcrInfo {
            engine: ENGINE.into(),
            model: SMALL_REC.repo.into(),
            device: "cpu".into(),
            ..Default::default()
        };
        match self.loaded.get() {
            None => {}
            Some(Err(e)) => info.error = Some(e.clone()),
            Some(Ok(l)) => {
                info.model = l.choice.rec.repo.into();
                let status = l.engine.device_status();
                info.device = device_name(status.actual);
                // auto 模式在没有 N 卡的机器上用 CPU 是正常情况，不算退回
                let expected = self.config.gpu_required || pagesift_gpu::device_count() > 0;
                if status.fell_back() && expected {
                    info.fallback = status.fallback;
                }
            }
        }
        info
    }
}

/// [`OcrInfo::engine`]。
const ENGINE: &str = "pp-ocrv6";

/// `cpu` 或 `cuda:N`。
fn device_name(d: Device) -> String {
    match d {
        Device::Cpu => "cpu".into(),
        Device::Gpu { index } => format!("cuda:{index}"),
    }
}
