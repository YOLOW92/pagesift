//! 把 [`Engine`] 接到 PDF 的扫描页接口上：第一张扫描图来时才准备模型（缺的先下载，由调用方提供
//! 下载函数，本 crate 不碰网络）、才载入，没遇到扫描页的文档一点开销都没有。

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use pagesift_pdf::{OcrInfo, OcrLine, PageOcr};

use crate::models::Model;
use crate::pipeline::Engine;
use crate::{Image, Line};

/// 把模型下载到 `<缓存根>/<仓库名>`（只在文件不全时调用），边下边校验。
pub type Fetch = Arc<dyn Fn(&Model) -> Result<(), String> + Send + Sync>;

/// 懒加载的 PDF OCR 引擎。
pub struct PdfOcr {
    root: PathBuf,
    det: Model,
    rec: Model,
    /// 没有时缺模型就报错（离线环境，提前用 `pagesift models fetch` 下好）。
    fetch: Option<Fetch>,
    engine: OnceLock<Result<Engine, String>>,
}

impl PdfOcr {
    pub fn new(root: PathBuf, det: Model, rec: Model, fetch: Option<Fetch>) -> Self {
        Self {
            root,
            det,
            rec,
            fetch,
            engine: OnceLock::new(),
        }
    }

    /// 模型目录直接给（测评用）：立刻载入，不走缓存和下载。
    pub fn from_dirs(det_dir: PathBuf, rec_dir: PathBuf) -> Result<Self, String> {
        let this = Self::new(
            PathBuf::new(),
            crate::models::SMALL_DET,
            crate::models::SMALL_REC,
            None,
        );
        let _ = this.engine.set(Ok(Engine::load(&det_dir, &rec_dir)?));
        Ok(this)
    }

    fn load(&self) -> Result<Engine, String> {
        for m in [&self.det, &self.rec] {
            if m.present(&self.root) {
                continue;
            }
            match &self.fetch {
                Some(fetch) => fetch(m)?,
                None => {
                    return Err(format!(
                        "model {} is missing from {}; run `pagesift models fetch`",
                        m.repo,
                        self.root.display()
                    ));
                }
            }
        }
        Engine::load(&self.det.dir(&self.root), &self.rec.dir(&self.root))
    }
}

impl std::fmt::Debug for PdfOcr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfOcr")
            .field("det", &self.det.repo)
            .field("rec", &self.rec.repo)
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
        text: l.text,
        score: l.score,
    }
}

impl PageOcr for PdfOcr {
    fn recognize(&self, width: u32, height: u32, gray: &[u8]) -> Result<Vec<OcrLine>, String> {
        let engine = self
            .engine
            .get_or_init(|| self.load())
            .as_ref()
            .map_err(Clone::clone)?;
        let img = Image {
            width: width as usize,
            height: height as usize,
            channels: 1,
            data: gray.to_vec(),
        };
        Ok(engine.run(&img)?.into_iter().map(to_ocr_line).collect())
    }

    fn info(&self) -> OcrInfo {
        OcrInfo {
            engine: "pp-ocrv6".into(),
            model: self.rec.repo.into(),
            device: "cpu".into(),
            fallback: None,
            error: self.engine.get().and_then(|r| r.as_ref().err().cloned()),
        }
    }
}
