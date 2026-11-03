//! 按原编码导出：JPEG、JPEG 2000 原样；CCITT 套 TIFF 头；JBIG2 拼全局段；
//! 无损像素编成 PNG（色彩空间换算见 [`pixels`]）。都不解码图像编码层。

use crate::file::{Document, Object};
use crate::obj::{deref, get, get_num, num_array, stream_bytes};

use super::png::{self, ColorType};
use super::tiff::{self, Ccitt};
use super::{ImageEncoding, PdfImage, Source, jbig2};

/// 导出的文件。
pub(crate) struct Exported {
    pub(crate) bytes: Vec<u8>,
    pub(crate) extension: &'static str,
}

/// [协议] JP2 文件格式的签名框（ISO/IEC 15444-1 附录 I.5.1）；没有它的是裸码流（.j2k）。
const JP2_SIGNATURE: [u8; 12] = [
    0, 0, 0, 0x0C, b'j', b'P', b' ', b' ', 0x0D, 0x0A, 0x87, 0x0A,
];

/// 不导出时也给出扩展名（Markdown 引用的文件名要用）。
pub(crate) fn extension(s: &Source, img: &PdfImage) -> &'static str {
    match img.encoding {
        ImageEncoding::Jpeg => "jpg",
        ImageEncoding::Jpeg2000 if s.data.starts_with(&JP2_SIGNATURE) => "jp2",
        ImageEncoding::Jpeg2000 => "j2k",
        ImageEncoding::Ccitt => "tif",
        ImageEncoding::Jbig2 => "jb2",
        ImageEncoding::Pixels => "png",
    }
}

pub(crate) fn export(
    doc: &Document,
    s: &Source,
    img: &PdfImage,
    limit: usize,
) -> Result<Exported, String> {
    let ext = extension(s, img);
    let bytes = match img.encoding {
        ImageEncoding::Jpeg => {
            // [协议] JPEG 的 SOI 标记 FFD8（ITU-T T.81 §B.1.1.3）
            if !s.data.starts_with(&[0xFF, 0xD8]) {
                return Err("DCT 数据不以 SOI 标记开头".into());
            }
            s.data.clone()
        }
        ImageEncoding::Jpeg2000 => s.data.clone(),
        ImageEncoding::Ccitt => {
            let p = Ccitt::from_params(s.params.as_ref(), img.height);
            tiff::wrap(&s.data, &p, decode_inverted(doc, s))
        }
        ImageEncoding::Jbig2 => {
            let globals = s
                .params
                .as_ref()
                .and_then(|p| p.get(b"JBIG2Globals"))
                .and_then(|o| stream_bytes(doc, o, limit))
                .unwrap_or_default();
            jbig2::wrap(&globals, &s.data)?
        }
        ImageEncoding::Pixels => pixels(doc, s, img, limit)?,
    };
    Ok(Exported {
        bytes,
        extension: ext,
    })
}

/// /Decode 的前两项是 [1 0]：一分量图像（含模板蒙版）显示反相。
pub(super) fn decode_inverted(doc: &Document, s: &Source) -> bool {
    get(doc, &s.dict, b"Decode")
        .map(|o| num_array(doc, o))
        .is_some_and(|d| d.len() >= 2 && d[0] > d[1])
}

/// 色彩空间（ISO 32000-2 §8.6）。只区分导出需要的几类。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ColorSpace {
    Gray(String),
    Rgb(String),
    Cmyk(String),
    /// 分色、DeviceN：色调值越大颜色越深；分量数。
    Tint(String, usize),
    Indexed {
        base: Box<ColorSpace>,
        hival: usize,
        lookup: Vec<u8>,
    },
    Other(String, usize),
}

impl ColorSpace {
    pub(crate) fn components(&self) -> usize {
        match self {
            ColorSpace::Gray(_) | ColorSpace::Indexed { .. } => 1,
            ColorSpace::Rgb(_) => 3, // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
            ColorSpace::Cmyk(_) => 4, // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
            ColorSpace::Tint(_, n) | ColorSpace::Other(_, n) => *n,
        }
    }

    pub(crate) fn describe(&self) -> String {
        match self {
            ColorSpace::Gray(n) | ColorSpace::Rgb(n) | ColorSpace::Cmyk(n) => n.clone(),
            ColorSpace::Tint(n, _) | ColorSpace::Other(n, _) => n.clone(),
            ColorSpace::Indexed { base, hival, .. } => {
                format!("Indexed({},{hival})", base.describe())
            }
        }
    }
}

/// 读色彩空间。资源里的命名色彩空间（内联图像才会有）读不到，按 Other 处理。
pub(crate) fn color_space(doc: &Document, o: Option<&Object>) -> ColorSpace {
    let Some(o) = o.map(|o| deref(doc, o)) else {
        return ColorSpace::Other("none".into(), 0);
    };
    let name_of = |o: &Object| {
        deref(doc, o)
            .as_name()
            .map(|n| String::from_utf8_lossy(n).into_owned())
    };
    match o {
        Object::Name(n) => match n.as_slice() {
            b"DeviceGray" | b"CalGray" => ColorSpace::Gray(String::from_utf8_lossy(n).into()),
            b"DeviceRGB" | b"CalRGB" => ColorSpace::Rgb(String::from_utf8_lossy(n).into()),
            b"DeviceCMYK" => ColorSpace::Cmyk("DeviceCMYK".into()),
            _ => ColorSpace::Other(String::from_utf8_lossy(n).into(), 0),
        },
        Object::Array(a) if !a.is_empty() => {
            let family = name_of(&a[0]).unwrap_or_default();
            match family.as_str() {
                "CalGray" => ColorSpace::Gray(family),
                "CalRGB" => ColorSpace::Rgb(family),
                // [协议] Lab 有 3 个分量（§8.6.5.4）
                "Lab" => ColorSpace::Other(family, 3),
                "ICCBased" => {
                    let n = a
                        .get(1)
                        .and_then(|s| deref(doc, s).as_dict())
                        .and_then(|d| get_num(doc, d, b"N"))
                        .unwrap_or(0.0) as usize;
                    let label = format!("ICCBased({n})");
                    // [协议] ICC 流的 /N：1 灰度、3 RGB、4 CMYK（§8.6.5.5）
                    match n {
                        1 => ColorSpace::Gray(label),
                        3 => ColorSpace::Rgb(label), // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
                        4 => ColorSpace::Cmyk(label), // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
                        _ => ColorSpace::Other(label, n),
                    }
                }
                "Indexed" | "I" => {
                    let base = color_space(doc, a.get(1));
                    let hival = a
                        .get(2)
                        .and_then(|o| crate::obj::num(deref(doc, o)))
                        .unwrap_or(0.0)
                        .clamp(0.0, f64::from(u8::MAX)) as usize;
                    // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值（ISO 32000-2 §8.9.2、§8.6）
                    let lookup = match a.get(3).map(|o| deref(doc, o)) {
                        Some(Object::String(s, _)) => s.clone(),
                        Some(o @ Object::Stream(_)) => {
                            stream_bytes(doc, o, usize::from(u16::MAX)).unwrap_or_default()
                        }
                        _ => Vec::new(),
                    };
                    ColorSpace::Indexed {
                        base: Box::new(base),
                        hival,
                        lookup,
                    }
                }
                "Separation" => ColorSpace::Tint(family, 1),
                "DeviceN" => {
                    let n = match a.get(1).map(|o| deref(doc, o)) {
                        Some(Object::Array(names)) => names.len(),
                        _ => 0,
                    };
                    ColorSpace::Tint(format!("DeviceN({n})"), n)
                }
                _ => ColorSpace::Other(family, 0),
            }
        }
        _ => ColorSpace::Other("unknown".into(), 0),
    }
}

/// 取一行里第 `i` 个样本（`bpc` 位，高位在前；ISO 32000-2 §8.9.2）。
pub(super) fn sample(row: &[u8], i: usize, bpc: usize) -> u32 {
    match bpc {
        8 => u32::from(row.get(i).copied().unwrap_or(0)), // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
        // [协议] 16 位样本取高字节（PNG 位深按 8 位写）
        16 => u32::from(row.get(2 * i).copied().unwrap_or(0)),
        _ => {
            let bit = i * bpc;
            let byte = row.get(bit / 8).copied().unwrap_or(0); // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
            let shift = 8 - bpc - bit % 8; // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
            u32::from(byte >> shift) & ((1 << bpc) - 1)
        }
    }
}

/// 把 `bpc` 位的值放大到 0–255。
pub(super) fn scale(v: u32, bpc: usize) -> u8 {
    // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值（ISO 32000-2 §8.9.2、§8.6）
    if bpc >= 8 {
        return v.min(255) as u8; // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
    }
    (v * 255 / ((1 << bpc) - 1)) as u8 // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
}

/// CMYK 换成 RGB：没有 ICC 换算，用朴素公式 R = (1−C)(1−K)（PDF 规范 §10.4.2.4 的反向）。
// [协议] 样本位深 1/2/4/8/16、分量数、8 位取值（ISO 32000-2 §8.9.2、§8.6）
fn cmyk_rgb(c: u8, m: u8, y: u8, k: u8) -> [u8; 3] {
    let f = |x: u8| ((255 - u32::from(x)) * (255 - u32::from(k)) / 255) as u8; // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
    [f(c), f(m), f(y)]
}

/// 一个颜色（base 色彩空间的分量，0–255）换成 RGB。
// [协议] 样本位深 1/2/4/8/16、分量数、8 位取值（ISO 32000-2 §8.9.2、§8.6）
pub(super) fn to_rgb(cs: &ColorSpace, v: &[u8]) -> Option<[u8; 3]> {
    Some(match cs {
        ColorSpace::Gray(_) => [v[0]; 3], // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
        ColorSpace::Rgb(_) => [v[0], v[1], v[2]],
        ColorSpace::Cmyk(_) => cmyk_rgb(v[0], v[1], v[2], v[3]),
        ColorSpace::Tint(_, 1) => [255 - v[0]; 3], // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
        _ => return None,
    })
}

/// 无损像素编成 PNG。
fn pixels(doc: &Document, s: &Source, img: &PdfImage, limit: usize) -> Result<Vec<u8>, String> {
    let (w, h) = (img.width as usize, img.height as usize);
    let mask = img.color_space == "ImageMask";
    let bpc = usize::from(img.bits_per_component);
    // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值（ISO 32000-2 §8.9.2、§8.6）
    if w == 0 || h == 0 || !matches!(bpc, 1 | 2 | 4 | 8 | 16) {
        return Err(format!("图像尺寸或位深不合规：{w}×{h}，{bpc} 位"));
    }
    let cs = if mask {
        ColorSpace::Gray("ImageMask".into())
    } else {
        color_space(doc, get(doc, &s.dict, b"ColorSpace"))
    };
    let comps = cs.components();
    if comps == 0 {
        return Err(format!("色彩空间 {} 不导出", cs.describe()));
    }
    let row_in = (w * comps * bpc).div_ceil(8); // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
    // 输出最多 3 字节/像素：超过单个流的上限就不导出
    // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值（ISO 32000-2 §8.9.2、§8.6）
    if w.saturating_mul(h).saturating_mul(3) > limit {
        return Err("图像超过单个流的大小上限".into());
    }
    let row = |r: usize| -> &[u8] {
        let a = (r * row_in).min(s.data.len());
        &s.data[a..(a + row_in).min(s.data.len())]
    };
    let inverted = decode_inverted(doc, s);
    match &cs {
        // 灰度（含模板蒙版、单色分色）：位深不变，按需反相（分色的色调 1 是满墨，显示为黑）
        ColorSpace::Gray(_) | ColorSpace::Tint(_, 1) => {
            let flip = inverted != matches!(cs, ColorSpace::Tint(..));
            let depth = bpc.min(8); // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
            let row_out = (w * depth).div_ceil(8); // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
            let mut out = Vec::with_capacity(row_out * h);
            for r in 0..h {
                let src = row(r);
                // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值（ISO 32000-2 §8.9.2、§8.6）
                if bpc == 16 {
                    out.extend((0..w).map(|i| sample(src, i, 16) as u8)); // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
                } else {
                    out.extend_from_slice(src);
                    out.resize((r + 1) * row_out, 0);
                }
            }
            if flip {
                out.iter_mut().for_each(|b| *b = !*b);
            }
            Ok(png::encode(
                img.width,
                img.height,
                depth as u8,
                ColorType::Gray,
                None,
                &out,
                row_out,
            ))
        }
        ColorSpace::Indexed {
            base,
            hival,
            lookup,
        } => {
            let n = base.components();
            // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值（ISO 32000-2 §8.9.2、§8.6）
            if n == 0 || bpc == 16 {
                return Err(format!("调色板的基色彩空间 {} 不导出", base.describe()));
            }
            let mut palette = Vec::with_capacity(3 * (hival + 1)); // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
            for i in 0..=*hival {
                let entry: Vec<u8> = (0..n)
                    .map(|k| lookup.get(i * n + k).copied().unwrap_or(0))
                    .collect();
                palette.extend(to_rgb(base, &entry).ok_or("调色板的基色彩空间不导出")?);
            }
            let mut out = Vec::with_capacity(row_in * h);
            for r in 0..h {
                out.extend_from_slice(row(r));
                out.resize((r + 1) * row_in, 0);
            }
            Ok(png::encode(
                img.width,
                img.height,
                bpc as u8,
                ColorType::Palette,
                Some(&palette),
                &out,
                row_in,
            ))
        }
        ColorSpace::Rgb(_) | ColorSpace::Cmyk(_) => {
            let mut out = Vec::with_capacity(3 * w * h); // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
            let mut v = vec![0u8; comps];
            for r in 0..h {
                let src = row(r);
                for i in 0..w {
                    for (k, slot) in v.iter_mut().enumerate() {
                        *slot = scale(sample(src, i * comps + k, bpc), bpc);
                    }
                    out.extend(to_rgb(&cs, &v).unwrap_or([0; 3])); // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
                }
            }
            Ok(png::encode(
                img.width,
                img.height,
                8, // [协议] 位深与分量数（ISO 32000-2 §8.9.2）
                ColorType::Rgb,
                None,
                &out,
                3 * w, // [协议] 位深与分量数（ISO 32000-2 §8.9.2）
            )) // [协议] 样本位深 1/2/4/8/16、分量数、8 位取值 0–255（ISO 32000-2 §8.9.2、§8.6）
        }
        _ => Err(format!("色彩空间 {} 不导出", cs.describe())),
    }
}
