//! 简单字体的编码：标准编码表、Differences 用到的字形名，以及标准 14 字体的字宽。

pub(crate) mod tables;

use std::collections::HashMap;
use std::sync::OnceLock;
use tables::{
    ASCII_NAMES, GREEK_LOWER, GREEK_UPPER, HELVETICA, KANGXI, LATIN1_NAMES, OTHER_NAMES,
    RADICAL_SUPPLEMENT, SIZE_SUFFIXES, STANDARD_HIGH, TEX_NAMES, TIMES,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BaseEncoding {
    Standard,
    WinAnsi,
    MacRoman,
}

/// 基础编码下单字节码对应的字符。
pub(crate) fn base_char(enc: BaseEncoding, code: u8) -> Option<char> {
    match enc {
        BaseEncoding::WinAnsi => match code {
            0x20..=0x7e => Some(code as char),
            0xa0 => Some(' '),
            0xad => Some('-'),
            0x80..=0xff => single_byte(encoding_rs::WINDOWS_1252, code),
            _ => None,
        },
        BaseEncoding::MacRoman => match code {
            0x20..=0x7e => Some(code as char),
            0x80..=0xff => single_byte(encoding_rs::MACINTOSH, code),
            _ => None,
        },
        BaseEncoding::Standard => match code {
            0x27 => Some('\u{2019}'),
            0x60 => Some('\u{2018}'),
            0x20..=0x7e => Some(code as char),
            0xa1..=0xff => {
                let c = STANDARD_HIGH[usize::from(code - 0xa1)];
                (c != '\0').then_some(c)
            }
            _ => None,
        },
    }
}

fn single_byte(enc: &'static encoding_rs::Encoding, code: u8) -> Option<char> {
    let buf = [code];
    let (s, bad) = enc.decode_without_bom_handling(&buf);
    if bad {
        return None;
    }
    s.chars().next().filter(|c| !c.is_control())
}

fn glyph_table() -> &'static HashMap<&'static str, String> {
    static TABLE: OnceLock<HashMap<&'static str, String>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut m = HashMap::new();
        for (i, n) in ASCII_NAMES.iter().enumerate() {
            m.insert(*n, char::from(0x20 + i as u8).to_string());
        }
        for (i, n) in LATIN1_NAMES.iter().enumerate() {
            m.insert(*n, char::from(0xa0 + i as u8).to_string());
        }
        for (i, n) in GREEK_LOWER.iter().enumerate() {
            if let Some(c) = char::from_u32(0x3b1 + i as u32) {
                m.insert(*n, c.to_string());
            }
        }
        for (i, n) in GREEK_UPPER.iter().enumerate() {
            if let Some(c) = char::from_u32(0x391 + i as u32)
                && !n.is_empty()
            {
                m.insert(*n, c.to_string());
            }
        }
        m.insert("mu", "\u{00B5}".to_string());
        for (n, s) in OTHER_NAMES.iter().chain(TEX_NAMES) {
            m.insert(*n, (*s).to_string());
        }
        m
    })
}

/// 字形名转 Unicode：查表，再按 AGL 规则处理 uniXXXX、uXXXX、连字和后缀。
pub(crate) fn glyph_to_unicode(name: &[u8]) -> Option<String> {
    let name = std::str::from_utf8(name).ok()?;
    let base = name.split('.').next().unwrap_or(name);
    if base.is_empty() {
        return None;
    }
    if base.contains('_') {
        let parts: Option<String> = base.split('_').map(glyph_component).collect();
        return parts;
    }
    glyph_component(base)
}

fn glyph_component(name: &str) -> Option<String> {
    if let Some(s) = glyph_table().get(name) {
        return Some(s.clone());
    }
    for suffix in SIZE_SUFFIXES {
        if let Some(stem) = name.strip_suffix(suffix)
            && stem.len() > 1
            && let Some(s) = glyph_table().get(stem)
        {
            return Some(s.clone());
        }
    }
    if let Some(hex) = name.strip_prefix("uni")
        && hex.len() >= 4
        && hex.len() % 4 == 0
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        let units: Vec<u16> = (0..hex.len() / 4)
            .filter_map(|i| u16::from_str_radix(&hex[i * 4..i * 4 + 4], 16).ok())
            .collect();
        let s: String = char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
        return Some(s);
    }
    if let Some(hex) = name.strip_prefix('u')
        && (4..=6).contains(&hex.len())
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        let v = u32::from_str_radix(hex, 16).ok()?;
        return char::from_u32(v).map(|c| c.to_string());
    }
    if !name.is_empty()
        && name.chars().all(|c| c.is_ascii_digit())
        && let Ok(v) = name.parse::<u32>()
        && let Some(c) = char::from_u32(v)
        && !c.is_control()
    {
        return Some(c.to_string());
    }
    None
}

/// 有些字体的 ToUnicode 把汉字映射到康熙部首或部首补充区，归一到统一汉字。
pub(crate) fn unify_radical(c: char) -> char {
    let v = c as u32;
    let mapped = match v {
        0x2F00..=0x2FD5 => Some(u32::from(KANGXI[(v - 0x2F00) as usize])),
        0x2E80..=0x2EF3 => RADICAL_SUPPLEMENT
            .binary_search_by_key(&v, |&(r, _)| u32::from(r))
            .ok()
            .map(|i| u32::from(RADICAL_SUPPLEMENT[i].1)),
        _ => None,
    };
    mapped.and_then(char::from_u32).unwrap_or(c)
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Standard14 {
    Helvetica,
    Times,
    Courier,
}

/// 标准 14 字体的字宽（千分之一 em），只覆盖 ASCII，其余按平均宽度估算。
pub(crate) fn standard_width(font: Standard14, ch: char) -> f64 {
    let code = ch as u32;
    match font {
        Standard14::Courier => 600.0,
        Standard14::Helvetica => match code {
            0x20..=0x7e => f64::from(HELVETICA[(code - 0x20) as usize]),
            _ => 556.0,
        },
        Standard14::Times => match code {
            0x20..=0x7e => f64::from(TIMES[(code - 0x20) as usize]),
            _ => 500.0,
        },
    }
}

pub(crate) fn standard14(base_font: &str) -> Option<Standard14> {
    let n = base_font.to_ascii_lowercase();
    if n.contains("courier") {
        Some(Standard14::Courier)
    } else if n.contains("helvetica") || n.contains("arial") {
        Some(Standard14::Helvetica)
    } else if n.contains("times") {
        Some(Standard14::Times)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_names() {
        assert_eq!(glyph_to_unicode(b"A").as_deref(), Some("A"));
        assert_eq!(glyph_to_unicode(b"eacute").as_deref(), Some("é"));
        assert_eq!(glyph_to_unicode(b"uni4E2D").as_deref(), Some("中"));
        assert_eq!(glyph_to_unicode(b"u1F600").as_deref(), Some("😀"));
        assert_eq!(glyph_to_unicode(b"f_f_i").as_deref(), Some("ffi"));
        assert_eq!(glyph_to_unicode(b"a.sc").as_deref(), Some("a"));
        assert_eq!(glyph_to_unicode(b"g123"), None);
        assert_eq!(unify_radical('\u{2F08}'), '人');
        assert_eq!(unify_radical('\u{2ECB}'), '车');
        assert_eq!(unify_radical('a'), 'a');
    }

    #[test]
    fn base_encodings() {
        assert_eq!(base_char(BaseEncoding::WinAnsi, 0x93), Some('“'));
        assert_eq!(base_char(BaseEncoding::MacRoman, 0x8e), Some('é'));
        assert_eq!(base_char(BaseEncoding::Standard, 0x27), Some('’'));
        assert_eq!(base_char(BaseEncoding::Standard, 0xae), Some('ﬁ'));
    }
}
