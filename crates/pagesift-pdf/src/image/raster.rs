//! 把一张图解成灰度像素（给 OCR 用）：JPEG、CCITT、JBIG2 和无损像素；JPEG 2000 暂不支持。
//! 图本身解不开而带软蒙版（/SMask）时改解蒙版：分层扫描（MRC）的页面里，文字的形状在蒙版里，
//! 前景、背景色是 JPEG 2000。
//!
//! 输出是滤镜和 /Decode 之后的样本按 DeviceGray 解释的灰度（0 黑、255 白）；文字是深色还是浅色
//! 由 OCR 那一步按多数像素判断（蒙版里文字常是 1 = 不透明 = 白）。

use crate::file::{Document, Object};
use crate::geom::ImageSource;
use crate::obj::{get, stream_bytes};

use super::decode::{self, Raster};
use super::export::{self, ColorSpace};
use super::tiff::Ccitt;
use super::{ImageEncoding, Source, describe, load};

/// 解一张图；解不开时返回 None（调用方跳过这张图）。`limit` 是输出字节数上限。
pub(crate) fn raster(doc: &Document, src: &ImageSource, limit: usize) -> Option<Raster> {
    let s = load(doc, src, limit)?;
    if let Some(r) = decode_source(doc, &s, limit) {
        return Some(r);
    }
    // 图本身解不开（JPEG 2000 等）：试软蒙版
    let Some(Object::Reference(id)) = s.dict.get(b"SMask") else {
        return None;
    };
    let m = load(doc, &ImageSource::Object(*id), limit)?;
    decode_source(doc, &m, limit)
}

fn decode_source(doc: &Document, s: &Source, limit: usize) -> Option<Raster> {
    let img = describe(doc, s, String::new(), false);
    let inverted = export::decode_inverted(doc, s);
    let mut r = match img.encoding {
        ImageEncoding::Jpeg => {
            // /ColorTransform（ISO 32000-2 §7.4.8 表 13）
            let ct = s
                .params
                .as_ref()
                .and_then(|p| p.get(b"ColorTransform"))
                .and_then(Object::as_i64)
                .map(|v| v != 0);
            decode::jpeg::decode_gray(&s.data, ct, limit).ok()?
        }
        ImageEncoding::Ccitt => {
            let p = Ccitt::from_params(s.params.as_ref(), img.height);
            decode::ccitt::decode(&s.data, &p, limit).ok()?
        }
        ImageEncoding::Jbig2 => {
            let globals = s
                .params
                .as_ref()
                .and_then(|p| p.get(b"JBIG2Globals"))
                .and_then(|o| stream_bytes(doc, o, limit));
            decode::jbig2::decode(&s.data, globals.as_deref(), limit).ok()?
        }
        ImageEncoding::Pixels => pixels(doc, s, &img, limit)?,
        ImageEncoding::Jpeg2000 => return None,
    };
    // 无损像素在 pixels 里已按 /Decode 处理过
    if inverted && img.encoding != ImageEncoding::Pixels {
        r.data.iter_mut().for_each(|v| *v = 255 - *v); // [协议] 8 位灰度取反
    }
    Some(r)
}

/// 无损像素（没有图像编码层）换成灰度：灰度、RGB、CMYK、调色板，任意位深。
fn pixels(doc: &Document, s: &Source, img: &super::PdfImage, limit: usize) -> Option<Raster> {
    let (w, h) = (img.width as usize, img.height as usize);
    let bpc = usize::from(img.bits_per_component);
    // [协议] 样本位深只能是 1/2/4/8/16（ISO 32000-2 §8.9.2）
    if w == 0 || h == 0 || !matches!(bpc, 1 | 2 | 4 | 8 | 16) || w.saturating_mul(h) > limit {
        return None;
    }
    let mask = img.color_space == "ImageMask";
    let cs = if mask {
        ColorSpace::Gray("ImageMask".into())
    } else {
        export::color_space(doc, get(doc, &s.dict, b"ColorSpace"))
    };
    let comps = cs.components();
    if comps == 0 {
        return None;
    }
    // [协议] 每行补齐到整字节（ISO 32000-2 §8.9.2）
    let row_in = (w * comps * bpc).div_ceil(8);
    let flip = export::decode_inverted(doc, s);
    let mut data = Vec::with_capacity(w * h);
    let mut v = vec![0u8; comps];
    for y in 0..h {
        if crate::budget::expired() {
            return None;
        }
        let a = (y * row_in).min(s.data.len());
        let row = &s.data[a..(a + row_in).min(s.data.len())];
        for x in 0..w {
            let g = match &cs {
                ColorSpace::Indexed { base, lookup, .. } => {
                    let i = export::sample(row, x, bpc) as usize;
                    let n = base.components();
                    let entry: Vec<u8> = (0..n)
                        .map(|k| lookup.get(i * n + k).copied().unwrap_or(0))
                        .collect();
                    let [r, g, b] = export::to_rgb(base, &entry)?;
                    decode::luma(r, g, b)
                }
                _ => {
                    for (k, slot) in v.iter_mut().enumerate() {
                        *slot = export::scale(export::sample(row, x * comps + k, bpc), bpc);
                    }
                    let [r, g, b] = export::to_rgb(&cs, &v)?;
                    decode::luma(r, g, b)
                }
            };
            data.push(if flip { 255 - g } else { g }); // [协议] 8 位灰度取反
        }
    }
    Some(Raster {
        width: img.width,
        height: img.height,
        channels: 1,
        data,
    })
}
