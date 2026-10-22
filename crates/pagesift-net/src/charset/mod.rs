//! 字符集识别与解码管道：BOM > Content-Type > <meta> > UTF-7 > 无 BOM 多字节 > UTF-8 > EBCDIC > DOS CP437 > chardetng > N-Gram 统计特征。

mod legacy;
mod meta;
mod multi_byte;
mod ngram;

use encoding_rs::{Encoding, IBM866, UTF_8, WINDOWS_1252, WINDOWS_1254};

pub use legacy::{
    decode_ebcdic_cp500, decode_utf7, decode_utf32_be, decode_utf32_le, detect_cp437_box,
    detect_ebcdic, detect_utf7,
};
pub use meta::{PRESCAN_BYTES, charset_param, prescan_meta};
pub use multi_byte::{detect_bom, detect_bomless_multibyte};
pub use ngram::{analyze_statistical, score_turkish};

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
    pub encoding_name: &'static str,
    pub source: CharsetSource,
    /// 输入里有无法解码、被替换成 U+FFFD 的字节。
    pub had_errors: bool,
}

/// 把响应体解码成 UTF-8。`tld` 是主机的顶级域（如 `b"jp"`），给启发式猜测做提示。
pub fn decode_owned(mut bytes: Vec<u8>, content_type: Option<&str>, tld: Option<&[u8]>) -> Decoded {
    let (encoding, encoding_name, source, skip) = detect(&bytes, content_type, tld);

    // 1. 处理非 WHATWG 标准的多字节/遗留编码解码
    if encoding_name == "utf-32" || encoding_name == "utf-32le" {
        let (text, had_errors) = decode_utf32_le(&bytes[skip..]);
        return Decoded {
            text,
            encoding,
            encoding_name,
            source,
            had_errors,
        };
    }
    if encoding_name == "utf-32be" {
        let (text, had_errors) = decode_utf32_be(&bytes[skip..]);
        return Decoded {
            text,
            encoding,
            encoding_name,
            source,
            had_errors,
        };
    }
    if encoding_name == "utf-7" {
        let (text, had_errors) = decode_utf7(&bytes[skip..]);
        return Decoded {
            text,
            encoding,
            encoding_name,
            source,
            had_errors,
        };
    }
    if encoding_name == "cp500" {
        let (text, had_errors) = decode_ebcdic_cp500(&bytes[skip..]);
        return Decoded {
            text,
            encoding,
            encoding_name,
            source,
            had_errors,
        };
    }

    // 2. UTF-8 极速无分配通道
    if encoding == UTF_8 {
        bytes.drain(..skip);
        match String::from_utf8(bytes) {
            Ok(text) => {
                return Decoded {
                    text,
                    encoding,
                    encoding_name,
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
            encoding_name,
            source,
            had_errors,
        };
    }

    // 3. 其他 WHATWG 编码通过 encoding_rs 解码
    let (text, had_errors) = encoding.decode_without_bom_handling(&bytes[skip..]);
    Decoded {
        text: text.into_owned(),
        encoding,
        encoding_name,
        source,
        had_errors,
    }
}

pub fn decode(bytes: &[u8], content_type: Option<&str>, tld: Option<&[u8]>) -> Decoded {
    let (encoding, encoding_name, source, skip) = detect(bytes, content_type, tld);

    if encoding_name == "utf-32" || encoding_name == "utf-32le" {
        let (text, had_errors) = decode_utf32_le(&bytes[skip..]);
        return Decoded {
            text,
            encoding,
            encoding_name,
            source,
            had_errors,
        };
    }
    if encoding_name == "utf-32be" {
        let (text, had_errors) = decode_utf32_be(&bytes[skip..]);
        return Decoded {
            text,
            encoding,
            encoding_name,
            source,
            had_errors,
        };
    }
    if encoding_name == "utf-7" {
        let (text, had_errors) = decode_utf7(&bytes[skip..]);
        return Decoded {
            text,
            encoding,
            encoding_name,
            source,
            had_errors,
        };
    }
    if encoding_name == "cp500" {
        let (text, had_errors) = decode_ebcdic_cp500(&bytes[skip..]);
        return Decoded {
            text,
            encoding,
            encoding_name,
            source,
            had_errors,
        };
    }

    let (text, had_errors) = encoding.decode_without_bom_handling(&bytes[skip..]);
    Decoded {
        text: text.into_owned(),
        encoding,
        encoding_name,
        source,
        had_errors,
    }
}

/// 核心探测入口，返回 (WHATWG 兼容 Encoding, 规范名称, 来源, BOM 长度)。
pub fn detect(
    bytes: &[u8],
    content_type: Option<&str>,
    tld: Option<&[u8]>,
) -> (&'static Encoding, &'static str, CharsetSource, usize) {
    // 1. BOM 优先（支持 UTF-32, UTF-16, UTF-8）
    if let Some((enc, len)) = detect_bom(bytes) {
        return (enc.whatwg_encoding(), enc.name(), CharsetSource::Bom, len);
    }

    // 2. HTTP Content-Type 参数
    if let Some(enc) = content_type
        .and_then(charset_param)
        .and_then(|l| Encoding::for_label(l.as_bytes()))
    {
        return (enc, enc.name(), CharsetSource::ContentType, 0);
    }

    // 3. HTML <meta> 预检
    if let Some(enc) = prescan_meta(&bytes[..bytes.len().min(PRESCAN_BYTES)]) {
        return (enc, enc.name(), CharsetSource::Meta, 0);
    }

    // 4. UTF-7 探测（纯 ASCII + Base64 逃逸）
    if detect_utf7(bytes) {
        return (UTF_8, "utf-7", CharsetSource::Detected, 0);
    }

    // 5. 无 BOM 的多字节探测 (UTF-32LE, UTF-32BE, UTF-16LE, UTF-16BE)
    if let Some(enc) = detect_bomless_multibyte(bytes) {
        return (
            enc.whatwg_encoding(),
            enc.name(),
            CharsetSource::Detected,
            0,
        );
    }

    // 6. 合法 UTF-8 校验 (极速直接返回)
    if std::str::from_utf8(bytes).is_ok() {
        return (UTF_8, "UTF-8", CharsetSource::Detected, 0);
    }

    // 7. EBCDIC 模式快速识别 (0x40 空格高密度)
    if detect_ebcdic(bytes) {
        return (WINDOWS_1252, "cp500", CharsetSource::Detected, 0);
    }

    // 8. chardetng 高置信度识别
    let mut det = chardetng::EncodingDetector::new();
    det.feed(bytes, true);
    let (guessed, high_conf) = det.guess_assess(tld, true);
    if high_conf {
        let name = if guessed == WINDOWS_1254 {
            score_turkish(bytes).map(|s| s.1).unwrap_or("windows-1254")
        } else if guessed == IBM866 {
            "cp866"
        } else {
            guessed.name()
        };
        return (guessed, name, CharsetSource::Detected, 0);
    }

    // 9. 自研统计与长尾 N-Gram 频次特征识别 (CJK、中欧、希腊、西里尔、土耳其)
    if let Some((enc, name)) = analyze_statistical(bytes, tld) {
        return (enc, name, CharsetSource::Detected, 0);
    }

    // 10. DOS CP437 制表符特征模式 (兜底)
    if detect_cp437_box(bytes) {
        return (IBM866, "cp437", CharsetSource::Detected, 0);
    }

    let name = if guessed == WINDOWS_1254 {
        score_turkish(bytes).map(|s| s.1).unwrap_or("windows-1254")
    } else if guessed == IBM866 {
        "cp866"
    } else {
        guessed.name()
    };
    (guessed, name, CharsetSource::Detected, 0)
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
    fn utf32_and_utf7_support() {
        // UTF-32LE with BOM
        let utf32_le = [0xFF, 0xFE, 0x00, 0x00, b'A', 0, 0, 0, b'B', 0, 0, 0];
        let d = decode(&utf32_le, None, None);
        assert_eq!(d.text, "AB");
        assert_eq!(d.encoding_name, "utf-32le");

        // UTF-7
        let utf7_bytes = b"+/v8-Hello world";
        let d = decode(utf7_bytes, None, None);
        assert_eq!(d.text, "Hello world");
        assert_eq!(d.encoding_name, "utf-7");
    }
}
