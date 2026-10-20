//! 字符集识别：BOM > Content-Type > `<meta>` > 启发式。

use encoding_rs::{Encoding, UTF_8, WINDOWS_1252};

/// 编码是从哪里得出的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharsetSource {
    Bom,
    ContentType,
    Meta,
    /// 合法 UTF-8 或 chardetng 猜测。
    Detected,
}

#[derive(Debug, Clone)]
pub struct Decoded {
    pub text: String,
    pub encoding: &'static Encoding,
    pub source: CharsetSource,
    /// 输入里有无法解码、被替换成 U+FFFD 的字节。
    pub had_errors: bool,
}

/// 只在文档开头这么多字节里找 `<meta>`（HTML 规范是 1024，放宽一些）。
const PRESCAN_BYTES: usize = 4096;

/// 把响应体解码成 UTF-8。`tld` 是主机的顶级域（如 `b"jp"`），给启发式猜测做提示。
pub fn decode(bytes: &[u8], content_type: Option<&str>, tld: Option<&[u8]>) -> Decoded {
    let (encoding, source, skip) = detect(bytes, content_type, tld);
    let (text, had_errors) = encoding.decode_without_bom_handling(&bytes[skip..]);
    Decoded {
        text: text.into_owned(),
        encoding,
        source,
        had_errors,
    }
}

fn detect(
    bytes: &[u8],
    content_type: Option<&str>,
    tld: Option<&[u8]>,
) -> (&'static Encoding, CharsetSource, usize) {
    if let Some((enc, len)) = Encoding::for_bom(bytes) {
        return (enc, CharsetSource::Bom, len);
    }
    if let Some(enc) = content_type
        .and_then(charset_param)
        .and_then(|l| Encoding::for_label(l.as_bytes()))
    {
        return (enc, CharsetSource::ContentType, 0);
    }
    if let Some(enc) = prescan_meta(&bytes[..bytes.len().min(PRESCAN_BYTES)]) {
        return (enc, CharsetSource::Meta, 0);
    }
    if std::str::from_utf8(bytes).is_ok() {
        return (UTF_8, CharsetSource::Detected, 0);
    }
    let mut det = chardetng::EncodingDetector::new();
    det.feed(bytes, true);
    (det.guess(tld, true), CharsetSource::Detected, 0)
}

fn charset_param(content_type: &str) -> Option<&str> {
    content_type.split(';').skip(1).find_map(|part| {
        let (k, v) = part.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| v.trim().trim_matches(['"', '\'']))
    })
}

/// 按 HTML 规范的思路扫描 `<meta charset>` 和 `<meta http-equiv="content-type" content="...">`。
fn prescan_meta(head: &[u8]) -> Option<&'static Encoding> {
    let mut i = 0;
    while i < head.len() {
        if head[i..].starts_with(b"<!--") {
            i = find(head, i + 4, b"-->").map_or(head.len(), |e| e + 3);
            continue;
        }
        if head[i] == b'<'
            && head.len() >= i + 6
            && head[i + 1..i + 5].eq_ignore_ascii_case(b"meta")
            && matches!(head[i + 5], b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'/')
        {
            let (attrs, end) = parse_attrs(head, i + 5);
            i = end;
            if let Some(enc) = meta_encoding(&attrs) {
                // 规范：meta 声明 UTF-16 时按 UTF-8 处理，x-user-defined 按 windows-1252。
                return Some(
                    if enc == encoding_rs::UTF_16LE || enc == encoding_rs::UTF_16BE {
                        UTF_8
                    } else if enc == encoding_rs::X_USER_DEFINED {
                        WINDOWS_1252
                    } else {
                        enc
                    },
                );
            }
            continue;
        }
        i += 1;
    }
    None
}

fn meta_encoding(attrs: &[(String, String)]) -> Option<&'static Encoding> {
    let get = |name: &str| {
        attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    if let Some(cs) = get("charset") {
        return Encoding::for_label(cs.trim().as_bytes());
    }
    if get("http-equiv").is_some_and(|v| v.trim().eq_ignore_ascii_case("content-type")) {
        let content = get("content")?.to_ascii_lowercase();
        let rest = &content[content.find("charset")? + 7..];
        let rest = rest.trim_start().strip_prefix('=')?.trim_start();
        let label: String = rest
            .trim_start_matches(['"', '\''])
            .chars()
            .take_while(|c| !matches!(c, '"' | '\'' | ';' | ' ' | '\t'))
            .collect();
        return Encoding::for_label(label.as_bytes());
    }
    None
}

/// 解析标签属性，返回属性列表和标签结束后的位置。属性名小写。
fn parse_attrs(s: &[u8], mut i: usize) -> (Vec<(String, String)>, usize) {
    let mut attrs = Vec::new();
    loop {
        while i < s.len() && (s[i].is_ascii_whitespace() || s[i] == b'/') {
            i += 1;
        }
        if i >= s.len() {
            return (attrs, i);
        }
        if s[i] == b'>' {
            return (attrs, i + 1);
        }
        let start = i;
        while i < s.len() && !matches!(s[i], b'=' | b'>' | b'/') && !s[i].is_ascii_whitespace() {
            i += 1;
        }
        let name = String::from_utf8_lossy(&s[start..i]).to_ascii_lowercase();
        while i < s.len() && s[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < s.len() && s[i] == b'=' {
            i += 1;
            while i < s.len() && s[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < s.len() && matches!(s[i], b'"' | b'\'') {
                let q = s[i];
                let vs = i + 1;
                i = vs;
                while i < s.len() && s[i] != q {
                    i += 1;
                }
                value = String::from_utf8_lossy(&s[vs..i]).into_owned();
                i = (i + 1).min(s.len());
            } else {
                let vs = i;
                while i < s.len() && s[i] != b'>' && !s[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = String::from_utf8_lossy(&s[vs..i]).into_owned();
            }
        }
        if name.is_empty() {
            i += 1;
        } else if !attrs.iter().any(|(k, _)| *k == name) {
            attrs.push((name, value));
        }
    }
}

fn find(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gbk(s: &str) -> Vec<u8> {
        encoding_rs::GBK.encode(s).0.into_owned()
    }

    #[test]
    fn bom_wins_over_header() {
        let mut b = vec![0xEF, 0xBB, 0xBF];
        b.extend_from_slice("中文".as_bytes());
        let d = decode(&b, Some("text/html; charset=gbk"), None);
        assert_eq!(d.text, "中文");
        assert_eq!(d.source, CharsetSource::Bom);

        let utf16: Vec<u8> = [0xFF, 0xFE, b'h', 0, b'i', 0].to_vec();
        assert_eq!(decode(&utf16, None, None).text, "hi");
    }

    #[test]
    fn header_then_meta() {
        let body = gbk("中文");
        let d = decode(&body, Some("text/html; Charset=\"GBK\""), None);
        assert_eq!(
            (d.text.as_str(), d.source),
            ("中文", CharsetSource::ContentType)
        );

        let mut doc = b"<html><head><META Charset=gb2312>".to_vec();
        doc.extend(gbk("中文"));
        let d = decode(&doc, Some("text/html"), None);
        assert!(d.text.ends_with("中文"));
        assert_eq!(d.source, CharsetSource::Meta);

        let mut doc =
            b"<meta http-equiv='Content-Type' content='text/html; charset=shift_jis'>".to_vec();
        doc.extend(encoding_rs::SHIFT_JIS.encode("日本語").0.iter());
        let d = decode(&doc, None, None);
        assert_eq!(d.encoding, encoding_rs::SHIFT_JIS);
        assert!(d.text.ends_with("日本語"));
    }

    #[test]
    fn meta_in_comment_is_ignored_and_utf16_meta_means_utf8() {
        let doc = b"<!-- <meta charset=gbk> --><meta charset=utf-16>ok";
        let d = decode(doc, None, None);
        assert_eq!(d.encoding, UTF_8);
        assert_eq!(d.source, CharsetSource::Meta);
    }

    #[test]
    fn heuristic_fallback() {
        assert_eq!(decode("plain ü".as_bytes(), None, None).encoding, UTF_8);
        let body = gbk("这是一段比较长的中文文本，用来让启发式判断有足够的依据。");
        let d = decode(&body, Some("text/html"), Some(b"cn"));
        assert_eq!(d.source, CharsetSource::Detected);
        assert!(d.text.contains("中文文本"), "{}", d.text);
    }

    #[test]
    fn unknown_label_falls_through() {
        let d = decode(
            b"<meta charset=bogus>abc",
            Some("text/html; charset=nope"),
            None,
        );
        assert_eq!(d.encoding, UTF_8);
        assert_eq!(d.source, CharsetSource::Detected);
    }
}
