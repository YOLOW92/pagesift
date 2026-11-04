//! 扫描页 OCR 的接入：调用方提供识别引擎（[`PageOcr`]，例如 pagesift-ocr 的 PP-OCRv6），
//! 这里把扫描页上的图解成灰度、交给引擎，再把识别出的每行作为一个 [`Glyph`] 放回页面，
//! 后面的版面重建（分栏、标题、表格）照常处理。本 crate 不依赖任何模型。

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use crate::budget;
use crate::file::Document;
use crate::geom::{Glyph, Placed};
use crate::image::raster::raster;

/// 识别引擎。
pub trait PageOcr: Send + Sync + std::fmt::Debug {
    /// 解图之前问一声引擎能不能用（模型缺了、还在下载就返回原因）：解一张扫描图要几秒，引擎用不了时
    /// 不白解。缺省总是能用。
    fn prepare(&self, _deadline: Option<Instant>) -> Result<(), String> {
        Ok(())
    }

    /// 识别一张灰度图（每像素一字节，0 黑 255 白，文字是深色）。`deadline` 是请求的硬截止时间，
    /// 过了就尽快返回。
    fn recognize(
        &self,
        width: u32,
        height: u32,
        gray: &[u8],
        deadline: Option<Instant>,
    ) -> Result<OcrOutput, String>;

    /// 引擎和模型的名字。解析完才问（设备和模型第一次用到时才定）。设备、退回和失败这几项是引擎
    /// 此刻的状态，不属于某一份文档；按文档的情况见 [`PdfDocument::ocr`](crate::PdfDocument::ocr)。
    fn info(&self) -> OcrInfo {
        OcrInfo::default()
    }
}

/// [`PageOcr::recognize`] 的结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OcrOutput {
    /// 图上的文字行。
    pub lines: Vec<OcrLine>,
    /// 这张图实际用的设备，例如 `cpu`、`cuda:0`。None 时用 [`OcrInfo::device`]。
    pub device: Option<String>,
    /// 想用 GPU 却（全部或部分）在 CPU 上算了的原因。
    pub fallback: Option<String>,
    /// 部分失败（个别公式、版面检测）的原因：`lines` 仍然可用，只是可能少了东西。
    pub partial_error: Option<String>,
}

/// [`PageOcr::info`] 的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OcrInfo {
    pub engine: String,
    pub model: String,
    /// 实际用的设备，例如 `cpu`、`cuda:0`。
    pub device: String,
    /// 想用 GPU 却退回了 CPU 时的原因。
    pub fallback: Option<String>,
    /// 引擎没能启动（模型下载失败等）时的原因；这时扫描页照旧只有说明，没有文字。
    pub error: Option<String>,
}

/// 这份文档走 OCR 的情况，只统计这份文档。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OcrReport {
    /// 这份文档确实走了 OCR：有图交给了 OCR 这一步。
    pub ran: bool,
    /// 实际用的设备（最后一次成功识别时）；前后不同时，退回的原因在 `fallback`。
    pub device: Option<String>,
    /// 想用 GPU 却（全部或部分）在 CPU 上算了的原因，取第一条。
    pub fallback: Option<String>,
    /// 识别失败或部分失败的原因，取第一条。
    pub error: Option<String>,
}

impl OcrReport {
    pub(crate) fn merge(&mut self, page: OcrReport) {
        self.ran |= page.ran;
        self.device = page.device.or(self.device.take());
        self.fallback = self.fallback.take().or(page.fallback);
        self.error = self.error.take().or(page.error);
    }

    fn note(&mut self, output: &OcrOutput) {
        self.device = output.device.clone().or(self.device.take());
        self.fallback = self.fallback.take().or(output.fallback.clone());
        self.error = self.error.take().or(output.partial_error.clone());
    }

    fn note_error(&mut self, e: String) {
        self.error.get_or_insert(e);
    }
}

/// 识别出的一行：图上的外框（像素坐标，y 向下）、文字、置信度（0–1）。
#[derive(Debug, Clone, PartialEq)]
pub struct OcrLine {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub text: String,
    pub score: f32,
    /// `text` 是整行的行间公式（`$$…$$`）：原样成行，不和前后的文字拼成段落。
    pub formula: bool,
}

/// 一页识别出的内容：文字（参与版面重建）和行间公式（页面坐标）。
#[derive(Default)]
pub(crate) struct Recognized {
    pub(crate) glyphs: Vec<Glyph>,
    pub(crate) formulas: Vec<Formula>,
    /// 有一张出字的图单独就够得上扫描页的覆盖率：文字层可以换成 OCR 的结果。
    pub(crate) covers_page: bool,
    pub(crate) report: OcrReport,
}

impl Recognized {
    pub(crate) fn is_empty(&self) -> bool {
        self.glyphs.is_empty() && self.formulas.is_empty()
    }
}

/// 行间公式在页面上的位置。
pub(crate) struct Formula {
    pub(crate) text: String,
    pub(crate) x0: f64,
    pub(crate) x1: f64,
    /// 框顶（页面的 y 向上）。
    pub(crate) top: f64,
}

/// [策略] OCR 解图时的内存上限是单个流上限的几倍：OCR 是调用方明确打开的，整页扫描常是
/// 300–600 dpi 的大图（olmOCR-bench 的扫描页最大 6379×5327，JPEG 三个分量的系数约 200 MB），
/// 单个流 64 MB 的缺省上限放不下；4 倍即 256 MB，仍挡得住几百字节声明上亿像素的解压炸弹
pub(crate) const RASTER_BUDGET: usize = 4;

/// [推导] OCR 处理一张灰度图时每像素的内存峰值（字节）：检测预处理的三个 f32 平面 3 × 4，解出的灰度图
/// 1，引擎为它拷的一份 1（版面预处理的 f32 平面和检测不同时存在）
const TENSOR_BYTES_PER_PIXEL: usize = 14;

/// [策略] 张量内存上限是解图上限（[`RASTER_BUDGET`] 换算出的字节数）的几倍：缺省 512 MB，放得下
/// olmOCR-bench 最大的 600 dpi 扫描页（6379×5327，476 MB），挡得住几 KB 的 CCITT 流声明的
/// 16000×16000（3.6 GB，工人数等于核数时几个并发就会撑爆内存）
const TENSOR_BUDGET: usize = 2;

/// [策略] 解一张图的时限：解码器的循环按它停下，没有请求时限时也不让畸形的图一直占着工人。正常的
/// 大扫描页几秒解完，满载时也留有几十倍余量
const RASTER_TIMEOUT: Duration = Duration::from_secs(120);

/// [策略] 新的图有这么大比例落在已经认出字的图上时算同一块内容：分层扫描（MRC）的前景、背景、
/// 蒙版都是整页图，再认一遍只会得到重复的文字；分块扫描的块之间至多有窄窄的重叠
const SAME_REGION: f64 = 0.5;

/// [策略] 行高里字号占的比例：文字行框（DB 检测框外扩过）比字号大，取常见的 0.8
const SIZE_OF_LINE_HEIGHT: f64 = 0.8;

fn area(p: &Placed) -> f64 {
    (p.x1 - p.x0) * (p.y1 - p.y0)
}

/// `p` 落在 `q` 上的比例。
fn overlap(p: &Placed, q: &Placed) -> f64 {
    let w = p.x1.min(q.x1) - p.x0.max(q.x0);
    let h = p.y1.min(q.y1) - p.y0.max(q.y0);
    (w.max(0.0) * h.max(0.0)) / area(p)
}

/// 这么大的图放得进张量内存上限。
fn fits_tensors(width: usize, height: usize, limit: usize) -> bool {
    width
        .saturating_mul(height)
        .saturating_mul(TENSOR_BYTES_PER_PIXEL)
        <= limit.saturating_mul(TENSOR_BUDGET)
}

/// OCR 花掉的时间不算进按字节估的时限（估的时候没算它）：把截止时间后移，但不超过硬上限。
pub(crate) fn extend_deadline(
    deadline: Option<Instant>,
    spent: Duration,
    hard: Option<Instant>,
) -> Option<Instant> {
    let moved = deadline.map(|d| d.checked_add(spent).unwrap_or(d));
    match (moved, hard) {
        (Some(d), Some(h)) => Some(d.min(h)),
        (d, _) => d,
    }
}

/// 一张出字的图。
struct Hit<'a> {
    place: &'a Placed,
    text: Vec<OcrLine>,
    formulas: Vec<OcrLine>,
    width: u32,
    height: u32,
}

/// 解一张图交给引擎。解不开时返回 None（调用方试下一张）。
fn recognize_image(
    doc: &Document,
    p: &Placed,
    engine: &dyn PageOcr,
    limit: usize,
    deadline: Option<Instant>,
) -> Result<Option<(OcrOutput, u32, u32)>, String> {
    let decode_by = Instant::now() + RASTER_TIMEOUT;
    let decode_by = deadline.map_or(decode_by, |d| d.min(decode_by));
    let decoded = {
        let _budget = budget::enter(Some(decode_by));
        raster(doc, &p.source, limit)
    };
    let Some(mut r) = decoded else {
        return Ok(None);
    };
    if r.width == 0 || r.height == 0 {
        return Ok(None);
    }
    let (w, h) = (r.width as usize, r.height as usize);
    if r.data.len() != w.saturating_mul(h) {
        return Err(format!(
            "the decoded {w}x{h} image has {} samples",
            r.data.len()
        ));
    }
    if !fits_tensors(w, h, limit) {
        return Err(format!("the {w}x{h} image is too large for OCR"));
    }
    // 文字要深色：多数像素是深色时（蒙版、反白扫描）整张取反
    let dark = r.data.iter().filter(|&&v| v < 128).count(); // [协议] 8 位灰度的中点
    if dark * 2 > r.data.len() {
        r.data.iter_mut().for_each(|v| *v = 255 - *v); // [协议] 8 位灰度取反
    }
    let output = engine.recognize(r.width, r.height, &r.data, deadline)?;
    Ok(Some((output, r.width, r.height)))
}

/// 识别一页。从最大的图开始：够得上扫描页覆盖率的图都认（一页由几块图拼成时结果合起来，和已认出字的图
/// 基本重叠的跳过）；一张都不够时第一张解得开且识别出文字的图就用它。一张都没成功时结果为空（调用方
/// 照旧写扫描页说明）。
///
/// `last_first`：面积相同时后画的图在前（调用方没要图片列表时，要和说明里引用的那张图一致）。
/// `deadline` 是硬上限。
pub(crate) fn recognize_page(
    doc: &Document,
    images: &[Placed],
    last_first: bool,
    engine: &dyn PageOcr,
    limit: usize,
    page_area: f64,
    deadline: Option<Instant>,
) -> Recognized {
    let mut out = Recognized::default();
    if images.is_empty() {
        return out;
    }
    out.report.ran = true;
    if let Err(e) = engine.prepare(deadline) {
        out.report.note_error(e);
        return out;
    }
    let mut order: Vec<&Placed> = if last_first {
        images.iter().rev().collect()
    } else {
        images.iter().collect()
    };
    order.sort_by(|a, b| area(b).total_cmp(&area(a)));
    let big = |p: &Placed| area(p) / page_area >= crate::run::SCANNED_MIN_COVERAGE;
    let mut hits: Vec<Hit> = Vec::new();
    for p in order {
        if !hits.is_empty() {
            // 从大到小排的：不够大的之后都不够大
            if !big(p) {
                break;
            }
            if hits.iter().any(|h| overlap(p, h.place) >= SAME_REGION) {
                continue;
            }
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            out.report.note_error("the OCR deadline passed".into());
            break;
        }
        // 一张图解不开、引擎 panic 都不牵连这页的文字层和别的页
        let tried = catch_unwind(AssertUnwindSafe(|| {
            recognize_image(doc, p, engine, limit, deadline)
        }));
        let (output, width, height) = match tried {
            Ok(Ok(Some(found))) => found,
            Ok(Ok(None)) => continue,
            Ok(Err(e)) => {
                out.report.note_error(e);
                continue;
            }
            Err(_) => {
                out.report.note_error("OCR panicked on an image".into());
                continue;
            }
        };
        out.report.note(&output);
        let (formulas, mut text): (Vec<OcrLine>, Vec<OcrLine>) =
            output.lines.into_iter().partition(|l| l.formula);
        text.retain(|l| !l.text.trim().is_empty());
        if !(text.is_empty() && formulas.is_empty()) {
            out.covers_page |= big(p);
            hits.push(Hit {
                place: p,
                text,
                formulas,
                width,
                height,
            });
        }
    }
    let size = text_size(&hits);
    for h in &hits {
        out.glyphs
            .extend(to_glyphs(&h.text, h.place, h.width, h.height, size));
        out.formulas
            .extend(to_formulas(&h.formulas, h.place, h.width, h.height));
    }
    out
}

/// 字号：这一页所有图上的行高（页面坐标）取中位数。识别框的高低随笔迹起伏，按各行高度给字号会把较高的行
/// 当成标题；同一页的几块图各取各的中位数，字号的细微差别也会被当成标题。
fn text_size(hits: &[Hit]) -> f64 {
    let mut heights: Vec<f64> = hits
        .iter()
        .flat_map(|h| {
            let sy = (h.place.y1 - h.place.y0) / f64::from(h.height);
            h.text.iter().map(move |l| f64::from(l.y1 - l.y0) * sy)
        })
        .collect();
    heights.sort_by(f64::total_cmp);
    let median = heights.get(heights.len() / 2).copied().unwrap_or(0.0);
    (median * SIZE_OF_LINE_HEIGHT).max(1.0)
}

/// 公式框换到页面坐标。
fn to_formulas(lines: &[OcrLine], p: &Placed, iw: u32, ih: u32) -> Vec<Formula> {
    let sx = (p.x1 - p.x0) / f64::from(iw);
    let sy = (p.y1 - p.y0) / f64::from(ih);
    lines
        .iter()
        .map(|l| Formula {
            text: l.text.clone(),
            x0: p.x0 + f64::from(l.x0) * sx,
            x1: p.x0 + f64::from(l.x1) * sx,
            top: p.y1 - f64::from(l.y0) * sy,
        })
        .collect()
}

/// 图上的行换到页面坐标（y 向上），每行一个字形。整行不拆成词：手写和打字稿的相邻两行常在
/// y 方向重叠，拆开后版面重建会把两行的词按 x 交错拼成一行。字号见 [`text_size`]。
fn to_glyphs(lines: &[OcrLine], p: &Placed, iw: u32, ih: u32, size: f64) -> Vec<Glyph> {
    let sx = (p.x1 - p.x0) / f64::from(iw);
    let sy = (p.y1 - p.y0) / f64::from(ih);
    lines
        .iter()
        .map(|l| Glyph {
            text: l
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .into(),
            x: p.x0 + f64::from(l.x0) * sx,
            // 基线取框底（图的 y 向下，页面的 y 向上）
            y: p.y1 - f64::from(l.y1) * sy,
            x_end: p.x0 + f64::from(l.x1) * sx,
            size,
            bold: false,
            math: Default::default(),
            mcid: None,
            artifact: false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tensor_budget_takes_real_scans_and_stops_bombs() {
        // [协议] 缺省的解图上限：64 MB × RASTER_BUDGET
        let limit = (64 << 20) * RASTER_BUDGET;
        assert!(fits_tensors(6379, 5327, limit));
        assert!(!fits_tensors(16000, 16000, limit));
    }

    #[test]
    fn deadline_moves_by_the_ocr_time_up_to_the_hard_limit() {
        // wall-clock: 只取一个起点做加减，没有等待
        let now = Instant::now();
        let s = Duration::from_secs;
        assert_eq!(extend_deadline(Some(now), s(5), None), Some(now + s(5)),);
        assert_eq!(
            extend_deadline(Some(now), s(5), Some(now + s(3))),
            Some(now + s(3)),
        );
        assert_eq!(extend_deadline(None, s(5), Some(now)), None);
    }
}
