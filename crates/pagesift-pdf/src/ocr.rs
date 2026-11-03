//! 扫描页 OCR 的接入：调用方提供识别引擎（[`PageOcr`]，例如 pagesift-ocr 的 PP-OCRv6），
//! 这里把扫描页上的图解成灰度、交给引擎，再把识别出的每行作为一个 [`Glyph`] 放回页面，
//! 后面的版面重建（分栏、标题、表格）照常处理。本 crate 不依赖任何模型。

use crate::file::Document;
use crate::geom::{Glyph, Placed};
use crate::image::raster::raster;

/// 识别引擎。
pub trait PageOcr: Send + Sync + std::fmt::Debug {
    /// 识别一张灰度图（每像素一字节，0 黑 255 白，文字是深色），返回图上的文字行。
    fn recognize(&self, width: u32, height: u32, gray: &[u8]) -> Result<Vec<OcrLine>, String>;
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
}

/// [策略] OCR 解图时的内存上限是单个流上限的几倍：OCR 是调用方明确打开的，整页扫描常是
/// 300–600 dpi 的大图（olmOCR-bench 的扫描页最大 6379×5327，JPEG 三个分量的系数约 200 MB），
/// 单个流 64 MB 的缺省上限放不下；4 倍即 256 MB，仍挡得住几百字节声明上亿像素的解压炸弹
pub(crate) const RASTER_BUDGET: usize = 4;

/// [策略] 行高里字号占的比例：文字行框（DB 检测框外扩过）比字号大，取常见的 0.8
const SIZE_OF_LINE_HEIGHT: f64 = 0.8;

/// 识别扫描页：从最大的图开始试，第一张解得开且识别出文字的图就用它。返回加进页面的字形；
/// 一张都没成功时返回空（调用方照旧写扫描页说明）。
pub(crate) fn recognize_page(
    doc: &Document,
    placed: &[Placed],
    engine: &dyn PageOcr,
    limit: usize,
) -> Vec<Glyph> {
    let area = |p: &Placed| (p.x1 - p.x0) * (p.y1 - p.y0);
    let mut order: Vec<&Placed> = placed.iter().collect();
    order.sort_by(|a, b| area(b).total_cmp(&area(a)));
    for p in order {
        let Some(mut r) = raster(doc, &p.source, limit) else {
            continue;
        };
        if r.width == 0 || r.height == 0 {
            continue;
        }
        // 文字要深色：多数像素是深色时（蒙版、反白扫描）整张取反
        let dark = r.data.iter().filter(|&&v| v < 128).count(); // [协议] 8 位灰度的中点
        if dark * 2 > r.data.len() {
            r.data.iter_mut().for_each(|v| *v = 255 - *v); // [协议] 8 位灰度取反
        }
        let Ok(lines) = engine.recognize(r.width, r.height, &r.data) else {
            continue;
        };
        let glyphs = to_glyphs(&lines, p, r.width, r.height);
        if !glyphs.is_empty() {
            return glyphs;
        }
    }
    Vec::new()
}

/// 图上的行换到页面坐标（y 向上），每行一个字形。整行不拆成词：手写和打字稿的相邻两行常在
/// y 方向重叠，拆开后版面重建会把两行的词按 x 交错拼成一行。字号全页统一取行高的中位数：
/// 识别框的高低随笔迹起伏，按各行高度给字号会把较高的行当成标题。
fn to_glyphs(lines: &[OcrLine], p: &Placed, iw: u32, ih: u32) -> Vec<Glyph> {
    let sx = (p.x1 - p.x0) / f64::from(iw);
    let sy = (p.y1 - p.y0) / f64::from(ih);
    let lines: Vec<&OcrLine> = lines.iter().filter(|l| !l.text.trim().is_empty()).collect();
    let mut heights: Vec<f64> = lines.iter().map(|l| f64::from(l.y1 - l.y0) * sy).collect();
    heights.sort_by(f64::total_cmp);
    let Some(&median) = heights.get(heights.len() / 2) else {
        return Vec::new();
    };
    let size = (median * SIZE_OF_LINE_HEIGHT).max(1.0);
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
