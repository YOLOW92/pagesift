//! lopdf 对象的便捷访问：解引用、取数字和名字、解压流。

use lopdf::{Dictionary, Document, Object};

/// 跟随间接引用，最多跳 16 次，防止引用成环。
pub(crate) fn deref<'a>(doc: &'a Document, mut obj: &'a Object) -> &'a Object {
    for _ in 0..16 {
        match obj {
            Object::Reference(id) => match doc.get_object(*id) {
                Ok(o) => obj = o,
                Err(_) => return &Object::Null,
            },
            _ => return obj,
        }
    }
    &Object::Null
}

pub(crate) fn get<'a>(doc: &'a Document, dict: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
    dict.get(key).ok().map(|o| deref(doc, o))
}

pub(crate) fn get_dict<'a>(
    doc: &'a Document,
    dict: &'a Dictionary,
    key: &[u8],
) -> Option<&'a Dictionary> {
    match get(doc, dict, key)? {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    }
}

pub(crate) fn get_name<'a>(
    doc: &'a Document,
    dict: &'a Dictionary,
    key: &[u8],
) -> Option<&'a [u8]> {
    match get(doc, dict, key)? {
        Object::Name(n) => Some(n),
        _ => None,
    }
}

pub(crate) fn get_num(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<f64> {
    num(get(doc, dict, key)?)
}

pub(crate) fn num(obj: &Object) -> Option<f64> {
    let v = match obj {
        Object::Integer(i) => *i as f64,
        Object::Real(r) => f64::from(*r),
        _ => return None,
    };
    v.is_finite().then_some(v)
}

pub(crate) fn num_array(doc: &Document, obj: &Object) -> Vec<f64> {
    match deref(doc, obj) {
        Object::Array(a) => a
            .iter()
            .map(|o| num(deref(doc, o)).unwrap_or(0.0))
            .collect(),
        _ => Vec::new(),
    }
}

/// 解压流内容，单个流的输出有上限。
pub(crate) fn stream_bytes(doc: &Document, obj: &Object, limit: usize) -> Option<Vec<u8>> {
    match deref(doc, obj) {
        Object::Stream(s) => s.decompressed_content_with_limit(limit).ok(),
        _ => None,
    }
}

/// PDF 文本串：UTF-16BE（带 BOM）、UTF-8（带 BOM）或 PDFDocEncoding。
pub(crate) fn text_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        let units = rest
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], c.get(1).copied().unwrap_or(0)]));
        return char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
    }
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    bytes.iter().map(|&b| pdfdoc_char(b)).collect()
}

fn pdfdoc_char(b: u8) -> char {
    const HIGH: [char; 32] = [
        '•', '†', '‡', '…', '—', '–', 'ƒ', '⁄', '‹', '›', '−', '‰', '„', '“', '”', '‘', '’', '‚',
        '™', 'ﬁ', 'ﬂ', 'Ł', 'Œ', 'Š', 'Ÿ', 'Ž', 'ı', 'ł', 'œ', 'š', 'ž', '\u{FFFD}',
    ];
    match b {
        0x80..=0x9f => HIGH[usize::from(b - 0x80)],
        0xa0 => '€',
        _ => char::from(b),
    }
}
