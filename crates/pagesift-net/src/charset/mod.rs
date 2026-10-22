//! 字符集识别管道：BOM > Content-Type > `<meta>` > 无 BOM 多字节探测 > N-Gram 统计特征 > chardetng 启发式。

mod meta;
mod multi_byte;
mod ngram;

use encoding_rs::{Encoding, UTF_8};

pub use meta::{PRESCAN_BYTES, charset_param, prescan_meta};
pub use multi_byte::{detect_bom, detect_bomless_multibyte};
pub use ngram::analyze_statistical;

/// 编码来源判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharsetSource {
    Bom,
    ContentType,
    Meta,
    /// 合法 UTF-8、多字节探测、统计特征或 chardetng 猜测。
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

/// 把响应体解码成 UTF-8。`tld` 是主机的顶级域（如 `b"jp"`），给启发式猜测做提示。
pub fn decode_owned(mut bytes: Vec<u8>, content_type: Option<&str>, tld: Option<&[u8]>) -> Decoded {
    let (encoding, source, skip) = detect(&bytes, content_type, tld);
    if encoding == UTF_8 {
        bytes.drain(..skip);
        match String::from_utf8(bytes) {
            Ok(text) => {
                return Decoded {
                    text,
                    encoding,
                    source,
                    had_errors: false,
                };
            }
            Err(e) => bytes = e.into_bytes(),
        }
        let (text, had_errors) = encoding.decode_without_bom_handling(&bytes);
        return Decoded {
            text: text.into_owned(),
            encoding,
            source,
            had_errors,
        };
    }
    let (text, had_errors) = encoding.decode_without_bom_handling(&bytes[skip..]);
    Decoded {
        text: text.into_owned(),
        encoding,
        source,
        had_errors,
    }
}

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

/// 核心探测入口。
pub fn detect(
    bytes: &[u8],
    content_type: Option<&str>,
    tld: Option<&[u8]>,
) -> (&'static Encoding, CharsetSource, usize) {
    // 1. BOM 优先
    if let Some((enc, len)) = detect_bom(bytes) {
        return (enc, CharsetSource::Bom, len);
    }

    // 2. HTTP Content-Type 参数
    if let Some(enc) = content_type
        .and_then(charset_param)
        .and_then(|l| Encoding::for_label(l.as_bytes()))
    {
        return (enc, CharsetSource::ContentType, 0);
    }

    // 3. HTML <meta> 预检
    if let Some(enc) = prescan_meta(&bytes[..bytes.len().min(PRESCAN_BYTES)]) {
        return (enc, CharsetSource::Meta, 0);
    }

    // 4. 无 BOM 的多字节探测 (UTF-16LE, UTF-16BE, UTF-32)
    if let Some(enc) = detect_bomless_multibyte(bytes) {
        return (enc, CharsetSource::Detected, 0);
    }

    // 5. 合法 UTF-8 校验 (极速直接返回)
    if std::str::from_utf8(bytes).is_ok() {
        return (UTF_8, CharsetSource::Detected, 0);
    }

    // 6. chardetng 高置信度识别（充分利用 Mozilla 深度训练的欧里尔/西里尔状态转移）
    let mut det = chardetng::EncodingDetector::new();
    det.feed(bytes, true);
    let (guessed, high_conf) = det.guess_assess(tld, true);
    if high_conf {
        return (guessed, CharsetSource::Detected, 0);
    }

    // 7. 低置信度时，运行自研统计与长尾 N-Gram 频次特征识别 (CJK、中欧、希腊、多字节扩展)
    if let Some(enc) = analyze_statistical(bytes, tld) {
        return (enc, CharsetSource::Detected, 0);
    }

    (guessed, CharsetSource::Detected, 0)
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
