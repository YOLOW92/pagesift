//! PDF 转干净的 Markdown（直接重导出 [`yolowpdf`]）。
//!
//! ```no_run
//! let bytes = std::fs::read("paper.pdf").unwrap();
//! let doc = pagesift_pdf::extract(&bytes).unwrap();
//! println!("{}", doc.markdown);
//! ```

#![forbid(unsafe_code)]

use std::panic::{AssertUnwindSafe, catch_unwind};

pub use yolowpdf::{
    Error, GrayImage, ImageEncoding, ImageOptions, ImagePlacement, Metadata, OcrInfo, OcrLine,
    OcrOutput, OcrReport, Options, PageOcr, PageQuality, PdfDocument, PdfImage, PdfPage, extract,
    extract_with, fuzzing, image_mime, observe_parse, parse_timeout,
};

/// 独立图像解码（PNG、JPEG、GIF、BMP、TIFF、WebP 转灰度），与 [`extract_with`] 对齐 `catch_unwind`
/// 边界，确保畸形输入万一触发内部断言/越界时返回结构化错误而不穿透破坏 CPU 工人线程。
pub fn decode_image_file(bytes: &[u8], max_pixels: u64) -> Result<GrayImage, String> {
    catch_unwind(AssertUnwindSafe(|| {
        yolowpdf::decode_image_file(bytes, max_pixels)
    }))
    .unwrap_or_else(|_| Err("malformed image: decoder panicked".into()))
}
