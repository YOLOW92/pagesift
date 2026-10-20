//! 字体解码：把显示串里的字节切成字符码，转成 Unicode 并给出字宽。

use std::collections::HashMap;

use lopdf::{Dictionary, Document, Object};

use crate::cmap::CMap;
use crate::encoding::{
    BaseEncoding, Standard14, base_char, glyph_to_unicode, standard_width, standard14,
};
use crate::obj::{deref, get, get_dict, get_name, get_num, num, num_array, stream_bytes};

/// 一个字符码解码后的结果。`width` 是字号为 1 时在文本空间里的前进宽度。
#[derive(Debug, Clone)]
pub(crate) struct DecodedChar {
    pub(crate) text: String,
    pub(crate) width: f64,
    pub(crate) is_space: bool,
}

#[derive(Debug, Clone, Copy)]
enum Legacy {
    Gbk,
    Big5,
    Sjis,
    EucJp,
    EucKr,
    /// ISO-2022-JP 去掉转义后的 JIS X 0208 双字节码。
    Jis,
}

#[derive(Debug)]
enum CodeSource {
    Identity,
    Embedded(CMap),
    Ucs2,
    Utf16,
    Legacy(Legacy),
}

#[derive(Debug)]
struct Composite {
    source: CodeSource,
    dw: f64,
    widths: HashMap<u32, f64>,
    width_ranges: Vec<(u32, u32, f64)>,
}

#[derive(Debug)]
pub(crate) struct Font {
    composite: Option<Composite>,
    to_unicode: Option<CMap>,
    simple_map: Vec<Option<String>>,
    first_char: u32,
    widths: Vec<f64>,
    missing_width: f64,
    std14: Option<Standard14>,
    /// 字形空间到文本空间的缩放，Type3 用 FontMatrix，其他字体是 0.001。
    scale: f64,
    /// Type3 字体的纵向缩放相对 0.001 的倍数，用来校正字号。
    pub(crate) size_factor: f64,
    pub(crate) bold: bool,
}

impl Font {
    pub(crate) fn fallback() -> Font {
        Font {
            composite: None,
            to_unicode: None,
            simple_map: (0..=255u8)
                .map(|c| base_char(BaseEncoding::WinAnsi, c).map(String::from))
                .collect(),
            first_char: 0,
            widths: Vec::new(),
            missing_width: 0.0,
            std14: Some(Standard14::Helvetica),
            scale: 0.001,
            size_factor: 1.0,
            bold: false,
        }
    }

    pub(crate) fn load(doc: &Document, dict: &Dictionary, stream_limit: usize) -> Font {
        let subtype = get_name(doc, dict, b"Subtype").unwrap_or(b"");
        let base_font = get_name(doc, dict, b"BaseFont")
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .unwrap_or_default();
        let to_unicode = get(doc, dict, b"ToUnicode")
            .and_then(|o| match o {
                Object::Stream(_) => stream_bytes(doc, o, stream_limit),
                _ => None,
            })
            .map(|b| CMap::parse(&b))
            .filter(|c| !c.is_empty_mapping());

        if subtype == b"Type0" {
            return Font::load_composite(doc, dict, &base_font, to_unicode, stream_limit);
        }

        let descriptor = get_dict(doc, dict, b"FontDescriptor");
        let mut font = Font::fallback();
        font.to_unicode = to_unicode;
        font.bold = is_bold(doc, &base_font, descriptor);
        font.std14 = if descriptor.is_none() || get(doc, dict, b"Widths").is_none() {
            standard14(&base_font)
        } else {
            None
        };
        if subtype == b"Type3" {
            let m = get(doc, dict, b"FontMatrix")
                .map(|o| num_array(doc, o))
                .unwrap_or_default();
            if m.len() == 6 {
                font.scale = sane(m[0], 0.001);
                font.size_factor = sane((m[3] / 0.001).abs(), 1.0);
            }
        }
        font.first_char = get_num(doc, dict, b"FirstChar").unwrap_or(0.0).max(0.0) as u32;
        font.widths = get(doc, dict, b"Widths")
            .map(|o| num_array(doc, o))
            .unwrap_or_default();
        font.widths.truncate(256);
        font.missing_width = descriptor
            .and_then(|d| get_num(doc, d, b"MissingWidth"))
            .unwrap_or(0.0);

        let default_base = if subtype == b"TrueType" {
            BaseEncoding::WinAnsi
        } else {
            BaseEncoding::Standard
        };
        let mut base = default_base;
        let mut differences: Option<&Vec<Object>> = None;
        match get(doc, dict, b"Encoding") {
            Some(Object::Name(n)) => base = named_base(n).unwrap_or(default_base),
            Some(Object::Dictionary(d)) => {
                if let Some(n) = get_name(doc, d, b"BaseEncoding") {
                    base = named_base(n).unwrap_or(default_base);
                }
                if let Some(Object::Array(a)) = get(doc, d, b"Differences") {
                    differences = Some(a);
                }
            }
            _ => {}
        }
        font.simple_map = (0..=255u8)
            .map(|c| base_char(base, c).map(String::from))
            .collect();
        if let Some(diffs) = differences {
            let mut code = 0usize;
            for item in diffs {
                match deref(doc, item) {
                    Object::Integer(i) => code = (*i).clamp(0, 256) as usize,
                    Object::Name(n) => {
                        if code < 256 {
                            font.simple_map[code] = glyph_to_unicode(n);
                        }
                        code += 1;
                    }
                    _ => {}
                }
            }
        }
        font
    }

    fn load_composite(
        doc: &Document,
        dict: &Dictionary,
        base_font: &str,
        to_unicode: Option<CMap>,
        stream_limit: usize,
    ) -> Font {
        let source = match get(doc, dict, b"Encoding") {
            Some(Object::Name(n)) => predefined_source(n),
            Some(o @ Object::Stream(_)) => {
                let cm = stream_bytes(doc, o, stream_limit)
                    .map(|b| CMap::parse(&b))
                    .unwrap_or_default();
                match &cm.parent {
                    _ if cm.has_codespace() => CodeSource::Embedded(cm),
                    Some(p) => predefined_source(p),
                    None => CodeSource::Identity,
                }
            }
            _ => CodeSource::Identity,
        };
        let descendant = match get(doc, dict, b"DescendantFonts") {
            Some(Object::Array(a)) => a.first().map(|o| deref(doc, o)),
            _ => None,
        };
        let cid_font = match descendant {
            Some(Object::Dictionary(d)) => Some(d),
            _ => None,
        };
        let descriptor = cid_font.and_then(|d| get_dict(doc, d, b"FontDescriptor"));
        let mut comp = Composite {
            source,
            dw: 1000.0,
            widths: HashMap::new(),
            width_ranges: Vec::new(),
        };
        if let Some(cf) = cid_font {
            comp.dw = get_num(doc, cf, b"DW").unwrap_or(1000.0);
            if let Some(Object::Array(w)) = get(doc, cf, b"W") {
                parse_w(doc, w, &mut comp);
            }
        }
        let mut font = Font::fallback();
        font.std14 = None;
        font.to_unicode = to_unicode;
        font.bold = is_bold(doc, base_font, descriptor);
        font.composite = Some(comp);
        font
    }

    /// 把显示串解码成字符序列。
    pub(crate) fn decode(&self, bytes: &[u8], out: &mut Vec<DecodedChar>) {
        match &self.composite {
            None => {
                for &b in bytes {
                    let code = u32::from(b);
                    let text = self
                        .to_unicode
                        .as_ref()
                        .and_then(|c| c.unicode(code))
                        .or_else(|| self.simple_map[usize::from(b)].clone())
                        .unwrap_or_default();
                    let width = self.simple_width(code, &text);
                    out.push(DecodedChar {
                        text,
                        width,
                        is_space: b == b' ',
                    });
                }
            }
            Some(comp) => {
                let mut i = 0;
                while i < bytes.len() {
                    let rest = &bytes[i..];
                    let len = code_len(&comp.source, rest).clamp(1, 4).min(rest.len());
                    let raw = &rest[..len];
                    let code = raw.iter().fold(0u32, |v, &b| v << 8 | u32::from(b));
                    i += len;
                    let text = self
                        .to_unicode
                        .as_ref()
                        .and_then(|c| c.unicode(code))
                        .or_else(|| fallback_text(&comp.source, raw, code))
                        .unwrap_or_default();
                    let cid = match &comp.source {
                        CodeSource::Identity => Some(code),
                        CodeSource::Embedded(cm) => cm.cid(code),
                        _ => None,
                    };
                    let w = cid.map_or(comp.dw, |c| comp.width(c));
                    out.push(DecodedChar {
                        text,
                        width: w * self.scale,
                        is_space: len == 1 && code == 32,
                    });
                }
            }
        }
    }

    fn simple_width(&self, code: u32, text: &str) -> f64 {
        let w = if let Some(idx) = code.checked_sub(self.first_char)
            && let Some(&w) = self.widths.get(idx as usize)
        {
            w
        } else if !self.widths.is_empty() {
            self.missing_width
        } else if let Some(s) = self.std14 {
            standard_width(s, text.chars().next().unwrap_or(' '))
        } else {
            500.0
        };
        w * self.scale
    }
}

impl Composite {
    fn width(&self, cid: u32) -> f64 {
        if let Some(&w) = self.widths.get(&cid) {
            return w;
        }
        let idx = self.width_ranges.partition_point(|r| r.0 <= cid);
        match idx.checked_sub(1).and_then(|i| self.width_ranges.get(i)) {
            Some(&(_, hi, w)) if cid <= hi => w,
            _ => self.dw,
        }
    }
}

fn parse_w(doc: &Document, w: &[Object], comp: &mut Composite) {
    let items: Vec<&Object> = w.iter().map(|o| deref(doc, o)).collect();
    let mut i = 0;
    while i < items.len() {
        let Some(first) = num(items[i]) else {
            i += 1;
            continue;
        };
        let first = first.max(0.0) as u32;
        match items.get(i + 1) {
            Some(Object::Array(ws)) => {
                for (k, o) in ws.iter().enumerate() {
                    if let Some(v) = num(deref(doc, o)) {
                        comp.widths.insert(first.saturating_add(k as u32), v);
                    }
                }
                i += 2;
            }
            Some(o) => {
                if let (Some(last), Some(v)) = (num(o), items.get(i + 2).and_then(|x| num(x))) {
                    comp.width_ranges.push((first, last.max(0.0) as u32, v));
                }
                i += 3;
            }
            None => break,
        }
    }
    comp.width_ranges.sort_by_key(|r| r.0);
}

fn sane(v: f64, default: f64) -> f64 {
    if v.is_finite() && v.abs() > 1e-9 && v.abs() < 1e6 {
        v
    } else {
        default
    }
}

fn named_base(n: &[u8]) -> Option<BaseEncoding> {
    match n {
        b"WinAnsiEncoding" => Some(BaseEncoding::WinAnsi),
        b"MacRomanEncoding" => Some(BaseEncoding::MacRoman),
        b"StandardEncoding" => Some(BaseEncoding::Standard),
        _ => None,
    }
}

fn is_bold(doc: &Document, base_font: &str, descriptor: Option<&Dictionary>) -> bool {
    let name = base_font.to_ascii_lowercase();
    if ["bold", "black", "heavy", "semibold", "demi"]
        .iter()
        .any(|k| name.contains(k))
    {
        return true;
    }
    let Some(d) = descriptor else { return false };
    if get_num(doc, d, b"FontWeight").is_some_and(|w| w >= 600.0) {
        return true;
    }
    get_num(doc, d, b"Flags").is_some_and(|f| (f as i64) & (1 << 18) != 0)
}

/// 预定义 CMap 名字到字符码来源。
fn predefined_source(name: &[u8]) -> CodeSource {
    let n = String::from_utf8_lossy(name);
    if n.starts_with("Identity") {
        return CodeSource::Identity;
    }
    if n.starts_with("Uni") {
        if n.contains("UCS2") {
            return CodeSource::Ucs2;
        }
        if n.contains("UTF16") {
            return CodeSource::Utf16;
        }
        return CodeSource::Identity;
    }
    let legacy = if n.contains("RKSJ") {
        Legacy::Sjis
    } else if n.starts_with("GB") {
        Legacy::Gbk
    } else if n.contains("B5") || n.starts_with("CNS") {
        Legacy::Big5
    } else if n.starts_with("KSC") {
        Legacy::EucKr
    } else if n.starts_with("EUC") {
        Legacy::EucJp
    } else if n == "H" || n == "V" {
        Legacy::Jis
    } else {
        return CodeSource::Identity;
    };
    CodeSource::Legacy(legacy)
}

fn code_len(source: &CodeSource, bytes: &[u8]) -> usize {
    let b0 = bytes.first().copied().unwrap_or(0);
    match source {
        CodeSource::Identity | CodeSource::Ucs2 => 2,
        CodeSource::Utf16 => {
            if bytes.len() >= 4 && (0xd8..=0xdb).contains(&b0) {
                4
            } else {
                2
            }
        }
        CodeSource::Embedded(cm) => cm.code_len(bytes),
        CodeSource::Legacy(l) => match l {
            Legacy::Gbk => {
                if b0 < 0x80 {
                    1
                } else if bytes.get(1).is_some_and(|b| b.is_ascii_digit()) {
                    4
                } else {
                    2
                }
            }
            Legacy::Big5 | Legacy::EucKr => {
                if b0 < 0x81 {
                    1
                } else {
                    2
                }
            }
            Legacy::Sjis => {
                if matches!(b0, 0x81..=0x9f | 0xe0..=0xfc) {
                    2
                } else {
                    1
                }
            }
            Legacy::EucJp => match b0 {
                0x8f => 3,
                0x8e | 0xa1..=0xfe => 2,
                _ => 1,
            },
            Legacy::Jis => 2,
        },
    }
}

fn fallback_text(source: &CodeSource, raw: &[u8], code: u32) -> Option<String> {
    match source {
        CodeSource::Ucs2 => char::from_u32(code).map(String::from),
        CodeSource::Utf16 => {
            let units: Vec<u16> = raw
                .chunks(2)
                .map(|c| u16::from_be_bytes([c[0], c.get(1).copied().unwrap_or(0)]))
                .collect();
            char::decode_utf16(units)
                .collect::<Result<String, _>>()
                .ok()
        }
        CodeSource::Legacy(l) => {
            let (enc, bytes): (&'static encoding_rs::Encoding, Vec<u8>) = match l {
                Legacy::Gbk => (encoding_rs::GB18030, raw.to_vec()),
                Legacy::Big5 => (encoding_rs::BIG5, raw.to_vec()),
                Legacy::Sjis => (encoding_rs::SHIFT_JIS, raw.to_vec()),
                Legacy::EucJp => (encoding_rs::EUC_JP, raw.to_vec()),
                Legacy::EucKr => (encoding_rs::EUC_KR, raw.to_vec()),
                Legacy::Jis => (encoding_rs::EUC_JP, raw.iter().map(|b| b | 0x80).collect()),
            };
            let (s, bad) = enc.decode_without_bom_handling(&bytes);
            (!bad).then(|| s.into_owned())
        }
        CodeSource::Identity | CodeSource::Embedded(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_cjk() {
        let src = predefined_source(b"GBK-EUC-H");
        let mut bytes: &[u8] = b"\xd6\xd0A";
        let mut out = Vec::new();
        while !bytes.is_empty() {
            let n = code_len(&src, bytes).min(bytes.len());
            let code = bytes[..n].iter().fold(0u32, |v, &b| v << 8 | u32::from(b));
            out.push(fallback_text(&src, &bytes[..n], code).unwrap_or_default());
            bytes = &bytes[n..];
        }
        assert_eq!(out, vec!["中".to_string(), "A".to_string()]);

        let sjis = predefined_source(b"90ms-RKSJ-H");
        assert_eq!(
            fallback_text(&sjis, b"\x93\xfa", 0x93fa).as_deref(),
            Some("日")
        );
    }
}
