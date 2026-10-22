//! 预定义 CJK 编码与回退文本映射。

use crate::cmap::CMap;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Legacy {
    Gbk,
    Big5,
    Sjis,
    EucJp,
    EucKr,
    Jis,
}

#[derive(Debug)]
pub(crate) enum CodeSource {
    Identity,
    Embedded(CMap),
    Ucs2,
    Utf16,
    Legacy(Legacy),
}

/// 预定义 CMap 名字到字符码来源。
pub(crate) fn predefined_source(name: &[u8]) -> CodeSource {
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

pub(crate) fn code_len(source: &CodeSource, bytes: &[u8]) -> usize {
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

pub(crate) fn fallback_text(source: &CodeSource, raw: &[u8], code: u32) -> Option<String> {
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
