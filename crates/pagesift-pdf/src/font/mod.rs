//! 字体解码：把显示串里的字节切成字符码，转成 Unicode 并给出字宽。

pub(crate) mod cjk;
use cjk::{CodeSource, code_len, fallback_text, predefined_source};

use std::collections::HashMap;

use lopdf::{Dictionary, Document, Object};

use crate::cmap::CMap;
use crate::encoding::{
    BaseEncoding, Standard14, base_char, glyph_to_unicode, standard_width, standard14,
};
use crate::fontfile::builtin_encoding;
use crate::obj::{deref, get, get_dict, get_name, get_num, num, num_array, stream_bytes};

/// 一个字符码解码后的结果。`width` 是字号为 1 时在文本空间里的前进宽度。
#[derive(Debug, Clone)]
pub(crate) struct DecodedChar {
    pub(crate) text: String,
    pub(crate) width: f64,
    pub(crate) is_space: bool,
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
    pub(crate) math: MathKind,
}

/// 数学字体的类别，按字体名判断（TeX 的 CM/AMS/LM/TX/PX 系列、Cambria Math、STIX 等）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum MathKind {
    #[default]
    Text,
    /// 数学斜体字母（CMMI）
    Italic,
    /// 符号（CMSY、MSAM、Symbol）：其中的大写字母是花体
    Symbol,
    /// 大型符号和定界符（CMEX）：基线位置和正文不同，不参与上下标判断
    Extension,
    /// 黑板粗体（MSBM、bbold）
    Blackboard,
    /// 手写体（rsfs、EUSM）
    Script,
    /// 哥特体（EUFM）
    Fraktur,
}

pub(crate) fn math_kind(base_font: &str) -> MathKind {
    // 去掉子集前缀 ABCDEF+
    let name = base_font
        .rsplit('+')
        .next()
        .unwrap_or(base_font)
        .to_ascii_lowercase();
    let has = |keys: &[&str]| keys.iter().any(|k| name.contains(k));
    if has(&["msbm", "bbold", "dsrom", "bbm", "doublestroke"]) {
        MathKind::Blackboard
    } else if has(&["eufm", "eufb", "fraktur"]) {
        MathKind::Fraktur
    } else if has(&["rsfs", "eusm", "eusb", "mathscr"]) {
        MathKind::Script
    } else if has(&[
        "cmex",
        "mathextension",
        "txex",
        "pxex",
        "esint",
        "mathx",
        "largesymbols",
    ]) {
        MathKind::Extension
    } else if has(&[
        "cmmi",
        "mathitalic",
        "txmi",
        "pxmi",
        "ntxmi",
        "newtxmi",
        "mtmi",
        "zxmi",
    ]) {
        MathKind::Italic
    } else if has(&[
        "cmsy",
        "cmbsy",
        "mathsymbols",
        "txsy",
        "pxsy",
        "msam",
        "stmary",
        "mnsymbol",
        "matha",
        "math",
    ]) || name == "symbol"
        || name.starts_with("symbolmt")
    {
        MathKind::Symbol
    } else {
        MathKind::Text
    }
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
            math: MathKind::Text,
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
        font.math = math_kind(&base_font);
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
        let mut named = false;
        let mut differences: Option<&Vec<Object>> = None;
        match get(doc, dict, b"Encoding") {
            Some(Object::Name(n)) => {
                base = named_base(n).unwrap_or(default_base);
                named = true;
            }
            Some(Object::Dictionary(d)) => {
                if let Some(n) = get_name(doc, d, b"BaseEncoding") {
                    base = named_base(n).unwrap_or(default_base);
                    named = true;
                }
                if let Some(Object::Array(a)) = get(doc, d, b"Differences") {
                    differences = Some(a);
                }
            }
            _ => {}
        }
        // 没指定基础编码、或者是符号字体时，以字体程序自带的编码为准（TeX 字体的 OT1、CMMI、CMSY、
        // CMEX 编码）：Ghostscript 输出的 TeX 字体常写着 WinAnsi，没列进 Differences 的码实际按内建编码画
        let symbolic = descriptor
            .and_then(|d| get_num(doc, d, b"Flags"))
            .is_some_and(|f| (f as i64) & 4 != 0);
        let builtin = ((!named || symbolic) && subtype != b"TrueType")
            .then(|| descriptor.and_then(|d| builtin_encoding(doc, d, stream_limit)))
            .flatten();
        font.simple_map = match builtin {
            Some(names) => names
                .iter()
                .map(|n| n.as_ref().and_then(|n| glyph_to_unicode(n.as_bytes())))
                .collect(),
            None => (0..=255u8)
                .map(|c| base_char(base, c).map(String::from))
                .collect(),
        };
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
        font.math = math_kind(base_font);
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
