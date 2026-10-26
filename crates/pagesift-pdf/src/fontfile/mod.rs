//! 嵌入字体程序内建编码解析器（支持 Type 1 与 CFF/Type1C）。

pub(crate) mod cff;
pub(crate) mod strings;
pub(crate) mod type1;

use crate::file::{Dictionary, Document, Object};
use crate::obj::{get, get_num, stream_bytes};
use cff::cff;
use type1::type1;

/// 字符码 0..=255 对应的字形名。字体程序用标准编码、或者读不出来时返回 None。
pub(crate) fn builtin_encoding(
    doc: &Document,
    descriptor: &Dictionary,
    limit: usize,
) -> Option<Vec<Option<String>>> {
    if let Some(obj @ Object::Reference(_) | obj @ Object::Stream(_)) = descriptor.get(b"FontFile")
    {
        let length1 = match crate::obj::deref(doc, obj) {
            Object::Stream(s) => get_num(doc, &s.dict, b"Length1"),
            _ => None,
        };
        let bytes = stream_bytes(doc, obj, limit)?;
        let clear = match length1 {
            Some(n) if n > 0.0 && (n as usize) <= bytes.len() => &bytes[..n as usize],
            _ => &bytes[..],
        };
        let clear = if clear.len() > 6 && clear[0] == 0x80 && clear[1] == 1 {
            &clear[6..]
        } else {
            clear
        };
        return type1(clear);
    }
    let obj = get(doc, descriptor, b"FontFile3")?;
    let bytes = stream_bytes(doc, obj, limit)?;
    cff(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type1_builtin_encoding() {
        let clear = b"%!PS-AdobeFont-1.0: CMSY10\n/Encoding 256 array\n0 1 255 {1 index exch /.notdef put} for\n\
dup 0 /minus put\ndup 20 /lessequal put\ndup 50 /element put\nreadonly def\ncurrentfile eexec";
        let enc = type1::type1(clear).unwrap();
        assert_eq!(enc[0].as_deref(), Some("minus"));
        assert_eq!(enc[20].as_deref(), Some("lessequal"));
        assert_eq!(enc[50].as_deref(), Some("element"));
        assert_eq!(enc[1], None);
        assert!(type1::type1(b"/Encoding StandardEncoding def").is_none());
    }

    #[test]
    fn cff_builtin_encoding() {
        fn index(items: &[&[u8]]) -> Vec<u8> {
            if items.is_empty() {
                return vec![0, 0];
            }
            let mut out = vec![0, items.len() as u8, 1];
            let mut off = 1u8;
            out.push(off);
            for it in items {
                off += it.len() as u8;
                out.push(off);
            }
            for it in items {
                out.extend_from_slice(it);
            }
            out
        }
        let enc_int = |v: i32| -> Vec<u8> {
            let b = v.to_be_bytes();
            vec![29, b[0], b[1], b[2], b[3]]
        };
        let header = [1u8, 0, 4, 1];
        let names = index(&[b"F"]);
        let strings = index(&[b"summationtext"]);
        let gsubrs = index(&[]);
        let top_len = 18;
        let top_index_len = 2 + 1 + 2 + top_len;
        let base = header.len() + names.len() + top_index_len + strings.len() + gsubrs.len();
        let charset_off = base;
        let charset = [0u8, 1, 135];
        let enc_off = charset_off + charset.len();
        let encoding = [0u8, 1, 0x58];
        let cs_off = enc_off + encoding.len();
        let charstrings = index(&[&[14], &[14]]);
        let mut top = Vec::new();
        top.extend(enc_int(charset_off as i32));
        top.push(15);
        top.extend(enc_int(enc_off as i32));
        top.push(16);
        top.extend(enc_int(cs_off as i32));
        top.push(17);
        assert_eq!(top.len(), top_len);
        let mut font = header.to_vec();
        font.extend(names);
        font.extend(index(&[&top]));
        font.extend(strings);
        font.extend(gsubrs);
        font.extend(charset);
        font.extend(encoding);
        font.extend(charstrings);
        let enc = cff::cff(&font).unwrap();
        assert_eq!(enc[0x58].as_deref(), Some("summationtext"));
        assert_eq!(
            crate::encoding::glyph_to_unicode(b"summationtext").as_deref(),
            Some("\u{2211}")
        );
        assert_eq!(
            crate::encoding::glyph_to_unicode(b"parenleftBigg").as_deref(),
            Some("(")
        );
    }
}
