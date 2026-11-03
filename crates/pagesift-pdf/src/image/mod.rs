//! 图片识别与按原编码导出。
//!
//! 识别：每页解释时记下每次画的图像（`geom::Placed`），这里按对象号（内联图像按内容）合并成
//! 文档级的图片表：编码、像素尺寸、色彩空间、字节数、内容哈希和每次出现的位置。同一张图画了
//! 多次只记一条，位置都列上。不解码像素，只读图像字典；哈希是图像编码层数据（解密、去掉
//! Flate 等通用过滤器之后）的 SHA-256。
//!
//! 导出（默认关）：不重新编码，JPEG、JPEG 2000 原样；CCITT 套 TIFF 头；JBIG2 拼上全局段；
//! 其余（无损像素）自己写 PNG。见 [`export`]。

pub(crate) mod decode;
mod export;
mod jbig2;
mod png;
pub(crate) mod raster;
pub(crate) mod tiff;

use std::collections::HashMap;

use sha2::{Digest, Sha256};

use crate::file::{Dictionary, Document, Object, ObjectId, inline_dict};
use crate::geom::{ImageSource, Placed};
use crate::obj::{deref, get, get_num};

/// 图像在 PDF 里的编码（ISO 32000-2 §7.4.1 表 6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageEncoding {
    /// DCTDecode。
    Jpeg,
    /// JPXDecode。
    Jpeg2000,
    /// CCITTFaxDecode（ITU-T T.4 / T.6 传真编码）。
    Ccitt,
    /// JBIG2Decode（ITU-T T.88）。
    Jbig2,
    /// 没有图像编码层：Flate、LZW、RunLength 或不压缩的像素。
    Pixels,
}

impl ImageEncoding {
    pub fn name(self) -> &'static str {
        match self {
            ImageEncoding::Jpeg => "jpeg",
            ImageEncoding::Jpeg2000 => "jpeg2000",
            ImageEncoding::Ccitt => "ccitt",
            ImageEncoding::Jbig2 => "jbig2",
            ImageEncoding::Pixels => "pixels",
        }
    }

    fn of(codec: Option<&[u8]>) -> ImageEncoding {
        match codec {
            Some(b"DCTDecode" | b"DCT") => ImageEncoding::Jpeg,
            Some(b"JPXDecode") => ImageEncoding::Jpeg2000,
            Some(b"CCITTFaxDecode" | b"CCF") => ImageEncoding::Ccitt,
            Some(b"JBIG2Decode") => ImageEncoding::Jbig2,
            _ => ImageEncoding::Pixels,
        }
    }
}

/// 图片在页面上的一次出现。坐标单位是 PDF 点（1/72 英寸），原点在页面（裁剪框、按 /Rotate
/// 转正后）的左上角，y 向下。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImagePlacement {
    /// 从 1 开始的页码。
    pub page: usize,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// 文档里的一张图（同一张图画多次只有一条）。
#[derive(Debug, Clone, PartialEq)]
pub struct PdfImage {
    /// 图像编码层数据的 SHA-256（小写十六进制，64 位）。
    pub hash: String,
    pub encoding: ImageEncoding,
    /// 像素宽高（/Width、/Height）。
    pub width: u32,
    pub height: u32,
    /// 每个分量的位数；模板蒙版（/ImageMask）是 1。JPEG 2000 可能不写，为 0。
    pub bits_per_component: u8,
    /// 色彩空间的描述：`DeviceRGB`、`ICCBased(3)`、`Indexed(DeviceRGB,255)`；蒙版是 `ImageMask`。
    pub color_space: String,
    /// 图像编码层数据的字节数。
    pub bytes: usize,
    /// 内联图像（写在内容流里的 `BI … EI`）。
    pub inline: bool,
    /// 每次出现的位置，按页码和阅读顺序。
    pub placements: Vec<ImagePlacement>,
    /// 按原编码导出的文件（[`crate::ImageOptions::export`] 打开时才有）。
    pub data: Option<Vec<u8>>,
    /// 导出文件的扩展名：jpg、jp2（裸码流是 j2k）、tif、jb2、png。
    pub extension: &'static str,
    /// 没能导出的原因（打开了导出却没有 `data` 时）。
    pub export_error: Option<String>,
}

impl PdfImage {
    /// 导出用的文件名：哈希的前 32 位十六进制（128 位）加扩展名，例如 `3fa0….jpg`。
    /// Markdown 里的图片引用是 `images/<文件名>`。
    pub fn file_name(&self) -> String {
        format!(
            "{}.{}",
            &self.hash[..self.hash.len().min(32)], // [协议] 哈希前缀
            self.extension
        ) // [协议] SHA-256 十六进制与位运算；32 位十六进制 = 128 位哈希前缀
    }
}

/// 一张图的来源：对象号或内联图像的内容。
#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Object(ObjectId),
    Inline(Vec<u8>),
}

/// 文档级的图片表。
#[derive(Default)]
pub(crate) struct Images {
    list: Vec<PdfImage>,
    by_key: HashMap<Key, usize>,
    by_hash: HashMap<String, usize>,
    /// 识别不了的图像（字典坏了、数据解不开）对象号：不再重试。
    bad: std::collections::HashSet<ObjectId>,
}

/// 图像字典和图像编码层的数据。
pub(crate) struct Source {
    dict: Dictionary,
    /// 图像编码层的数据（没有编码层时是像素）。
    data: Vec<u8>,
    /// 图像编码层的过滤器名和参数。
    codec: Option<Vec<u8>>,
    params: Option<Dictionary>,
}

impl Images {
    /// 记下一页上画的图，返回每次放置对应的图片下标（和 `placed` 一一对应，识别不了的是 None）。
    pub(crate) fn add(
        &mut self,
        doc: &Document,
        page: usize,
        page_height: f64,
        placed: &[Placed],
        export: bool,
        limit: usize,
    ) -> Vec<Option<usize>> {
        placed
            .iter()
            .map(|p| {
                let idx = self.lookup(doc, &p.source, export, limit)?;
                self.list[idx].placements.push(ImagePlacement {
                    page,
                    x: p.x0,
                    y: page_height - p.y1,
                    width: p.x1 - p.x0,
                    height: p.y1 - p.y0,
                });
                Some(idx)
            })
            .collect()
    }

    fn lookup(
        &mut self,
        doc: &Document,
        src: &ImageSource,
        export: bool,
        limit: usize,
    ) -> Option<usize> {
        let key = match src {
            ImageSource::Object(id) => Key::Object(*id),
            ImageSource::Inline(rc) => Key::Inline([rc.0.as_slice(), &rc.1].concat()),
        };
        if let Some(&i) = self.by_key.get(&key) {
            return Some(i);
        }
        if let ImageSource::Object(id) = src
            && self.bad.contains(id)
        {
            return None;
        }
        let Some(source) = load(doc, src, limit) else {
            if let ImageSource::Object(id) = src {
                self.bad.insert(*id);
            }
            return None;
        };
        let hash = hex(&Sha256::digest(&source.data));
        // 不同对象、内容相同（写入方重复嵌入同一张图）：仍是一张图
        if let Some(&i) = self.by_hash.get(&hash) {
            self.by_key.insert(key, i);
            return Some(i);
        }
        let mut img = describe(
            doc,
            &source,
            hash.clone(),
            matches!(src, ImageSource::Inline(_)),
        );
        if export {
            match export::export(doc, &source, &img, limit) {
                Ok(e) => {
                    img.extension = e.extension;
                    img.data = Some(e.bytes);
                }
                Err(why) => img.export_error = Some(why),
            }
        } else {
            img.extension = export::extension(&source, &img);
        }
        let i = self.list.len();
        self.list.push(img);
        self.by_key.insert(key, i);
        self.by_hash.insert(hash, i);
        Some(i)
    }

    pub(crate) fn get(&self, i: usize) -> &PdfImage {
        &self.list[i]
    }

    pub(crate) fn into_list(self) -> Vec<PdfImage> {
        self.list
    }
}

/// 读图像字典、解到图像编码层。
fn load(doc: &Document, src: &ImageSource, limit: usize) -> Option<Source> {
    match src {
        ImageSource::Object(id) => {
            let Object::Stream(s) = deref(doc, doc.get_object(*id)?) else {
                return None;
            };
            let data = doc.image_data(s, limit)?;
            Some(Source {
                dict: s.dict.clone(),
                codec: data.codec.map(|(n, _)| n.to_vec()),
                params: data.codec.and_then(|(_, p)| p.cloned()),
                data: data.bytes,
            })
        }
        ImageSource::Inline(rc) => {
            let dict = expand_inline(&inline_dict(&rc.0));
            let names: Vec<Vec<u8>> = match dict.get(b"Filter") {
                Some(Object::Name(n)) => vec![n.clone()],
                Some(Object::Array(a)) => a
                    .iter()
                    .filter_map(|o| o.as_name().map(<[u8]>::to_vec))
                    .collect(),
                _ => Vec::new(),
            };
            let params: Vec<Option<&Dictionary>> = match dict.get(b"DecodeParms") {
                Some(Object::Dictionary(d)) => vec![Some(d); names.len()],
                Some(Object::Array(a)) => a.iter().map(Object::as_dict).collect(),
                _ => Vec::new(),
            };
            let cut = names
                .iter()
                .position(|n| crate::file::is_image_filter(n))
                .unwrap_or(names.len());
            let generic: Vec<(&[u8], Option<&Dictionary>)> = names[..cut]
                .iter()
                .enumerate()
                .map(|(i, n)| (n.as_slice(), params.get(i).copied().flatten()))
                .collect();
            let data = crate::file::decode_filters(&rc.1, &generic, limit).ok()?;
            let codec = names.get(cut).cloned();
            let p = params.get(cut).copied().flatten().cloned();
            Some(Source {
                codec,
                params: p,
                data,
                dict,
            })
        }
    }
}

/// 内联图像的缩写键和缩写值换成全名（ISO 32000-2 §8.9.7 表 91、92）。
fn expand_inline(d: &Dictionary) -> Dictionary {
    let key = |k: &[u8]| -> Vec<u8> {
        match k {
            b"BPC" => b"BitsPerComponent".to_vec(),
            b"CS" => b"ColorSpace".to_vec(),
            b"D" => b"Decode".to_vec(),
            b"DP" => b"DecodeParms".to_vec(),
            b"F" => b"Filter".to_vec(),
            b"H" => b"Height".to_vec(),
            b"IM" => b"ImageMask".to_vec(),
            b"I" => b"Interpolate".to_vec(),
            b"W" => b"Width".to_vec(),
            _ => k.to_vec(),
        }
    };
    let name = |n: &[u8]| -> Vec<u8> {
        match n {
            b"G" => b"DeviceGray".to_vec(),
            b"RGB" => b"DeviceRGB".to_vec(),
            b"CMYK" => b"DeviceCMYK".to_vec(),
            b"I" => b"Indexed".to_vec(),
            b"AHx" => b"ASCIIHexDecode".to_vec(),
            b"A85" => b"ASCII85Decode".to_vec(),
            b"LZW" => b"LZWDecode".to_vec(),
            b"Fl" => b"FlateDecode".to_vec(),
            b"RL" => b"RunLengthDecode".to_vec(),
            b"CCF" => b"CCITTFaxDecode".to_vec(),
            b"DCT" => b"DCTDecode".to_vec(),
            _ => n.to_vec(),
        }
    };
    fn map(o: &Object, name: &dyn Fn(&[u8]) -> Vec<u8>) -> Object {
        match o {
            Object::Name(n) => Object::Name(name(n)),
            Object::Array(a) => Object::Array(a.iter().map(|x| map(x, name)).collect()),
            other => other.clone(),
        }
    }
    let mut out = Dictionary::default();
    for (k, v) in d.iter() {
        let k = key(k);
        let v = match k.as_slice() {
            b"ColorSpace" | b"Filter" => map(v, &name),
            _ => v.clone(),
        };
        out.push(k, v);
    }
    out
}

/// 从图像字典读出描述。
fn describe(doc: &Document, s: &Source, hash: String, inline: bool) -> PdfImage {
    let num = |k: &[u8]| get_num(doc, &s.dict, k).unwrap_or(0.0);
    let mask = matches!(get(doc, &s.dict, b"ImageMask"), Some(Object::Boolean(true)));
    let encoding = ImageEncoding::of(s.codec.as_deref());
    let bpc = if mask { 1.0 } else { num(b"BitsPerComponent") };
    PdfImage {
        hash,
        encoding,
        width: num(b"Width").clamp(0.0, f64::from(u32::MAX)) as u32,
        height: num(b"Height").clamp(0.0, f64::from(u32::MAX)) as u32,
        bits_per_component: bpc.clamp(0.0, f64::from(u8::MAX)) as u8,
        color_space: if mask {
            "ImageMask".into()
        } else {
            export::color_space(doc, get(doc, &s.dict, b"ColorSpace")).describe()
        },
        bytes: s.data.len(),
        inline,
        placements: Vec::new(),
        data: None,
        extension: "bin",
        export_error: None,
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef"; // [协议] SHA-256 十六进制与位运算；32 位十六进制 = 128 位哈希前缀
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[usize::from(b >> 4)] as char); // [协议] SHA-256 十六进制与位运算；32 位十六进制 = 128 位哈希前缀
        s.push(DIGITS[usize::from(b & 15)] as char); // [协议] SHA-256 十六进制与位运算；32 位十六进制 = 128 位哈希前缀
    }
    s
}
