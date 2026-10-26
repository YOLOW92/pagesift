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
    /// 解码后文本里的 U+FFFD 数量；`had_errors == false` 时为 0。
    pub replacement_chars: usize,
    /// 编码来自 HTTP 头或 `<meta>` 时，记录原始声明。
    pub declared: Option<Declared>,
    /// 头/meta 的声明被正文证据推翻时，记录原因。
    pub overridden: Option<DecodeOverride>,
}

/// HTTP 头或 `<meta>` 声明的编码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Declared {
    pub encoding_name: &'static str,
    pub source: CharsetSource,
}

/// 声明不可信、改用其他编码的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeOverride {
    /// 按声明编码解码出现替换字符，换用正文探测结果。
    InvalidUnderDeclared { replacements: usize },
    /// 声明的是单字节编码，但正文整体是合法 UTF-8。
    BodyIsUtf8,
}

impl Decoded {
    fn new(
        text: String,
        encoding: &'static Encoding,
        encoding_name: &'static str,
        source: CharsetSource,
        had_errors: bool,
    ) -> Self {
        // 只有解码报错时才可能有替换字符；避免对正常页面做一次全文扫描。
        let replacement_chars = if had_errors {
            text.matches('\u{FFFD}').count()
        } else {
            0
        };
        Self {
            text,
            encoding,
            encoding_name,
            source,
            had_errors,
            replacement_chars,
            declared: None,
            overridden: None,
        }
    }

    /// 面向 `metadata.pagesift.warnings` 的解码提示。没有异常时返回 `None`。
    pub fn warning(&self) -> Option<String> {
        if let Some(overridden) = self.overridden {
            let declared = self.declared?;
            let label = source_label(declared.source);
            return Some(match overridden {
                DecodeOverride::InvalidUnderDeclared { replacements } => format!(
                    "decode: {label} declared {} but decoding produced {replacements} replacement characters; decoded as {} instead",
                    declared.encoding_name, self.encoding_name
                ),
                DecodeOverride::BodyIsUtf8 => format!(
                    "decode: {label} declared {} but the body is valid UTF-8; decoded as UTF-8 instead",
                    declared.encoding_name
                ),
            });
        }
        if self.replacement_chars == 0 {
            return None;
        }
        Some(match self.declared {
            Some(declared) => format!(
                "decode: {} replacement characters under charset={} (declared by {})",
                self.replacement_chars,
                self.encoding_name,
                source_label(declared.source)
            ),
            None => format!(
                "decode: {} replacement characters under charset={}",
                self.replacement_chars, self.encoding_name
            ),
        })
    }
}

fn source_label(source: CharsetSource) -> &'static str {
    match source {
        CharsetSource::ContentType => "Content-Type",
        CharsetSource::Meta => "meta",
        CharsetSource::Bom => "BOM",
        CharsetSource::Detected => "detection",
    }
}

/// 把响应体解码成 UTF-8。`tld` 是主机的顶级域（如 `b"jp"`），给启发式猜测做提示。
pub fn decode_owned(mut bytes: Vec<u8>, content_type: Option<&str>, tld: Option<&[u8]>) -> Decoded {
    let (encoding, encoding_name, source, skip) = detect(&bytes, content_type, tld);

    // UTF-8 极速无分配通道。合法 UTF-8 不会被“单字节声明”误判，
    // 因此这里可以直接返回，不进入正文复核。
    if encoding == UTF_8 {
        bytes.drain(..skip);
        match String::from_utf8(bytes) {
            Ok(text) => {
                let mut decoded = Decoded::new(text, encoding, encoding_name, source, false);
                if matches!(source, CharsetSource::ContentType | CharsetSource::Meta) {
                    decoded.declared = Some(Declared {
                        encoding_name,
                        source,
                    });
                }
                return decoded;
            }
            Err(e) => bytes = e.into_bytes(),
        }
        let (text, had_errors) = encoding.decode_without_bom_handling(&bytes);
        let mut decoded = Decoded::new(
            text.into_owned(),
            encoding,
            encoding_name,
            source,
            had_errors,
        );
        refine_against_body(&mut decoded, &bytes, tld);
        return decoded;
    }

    let mut decoded = decode_slice(&bytes, encoding, encoding_name, source, skip);
    refine_against_body(&mut decoded, &bytes, tld);
    decoded
}

pub fn decode(bytes: &[u8], content_type: Option<&str>, tld: Option<&[u8]>) -> Decoded {
    let (encoding, encoding_name, source, skip) = detect(bytes, content_type, tld);
    let mut decoded = decode_slice(bytes, encoding, encoding_name, source, skip);
    refine_against_body(&mut decoded, bytes, tld);
    decoded
}

/// 按已选定的编码解码。非 WHATWG 标准的多字节/遗留编码走各自的解码器。
fn decode_slice(
    bytes: &[u8],
    encoding: &'static Encoding,
    encoding_name: &'static str,
    source: CharsetSource,
    skip: usize,
) -> Decoded {
    let (text, had_errors) = if encoding_name == "utf-32" || encoding_name == "utf-32le" {
        decode_utf32_le(&bytes[skip..])
    } else if encoding_name == "utf-32be" {
        decode_utf32_be(&bytes[skip..])
    } else if encoding_name == "utf-7" {
        decode_utf7(&bytes[skip..])
    } else if encoding_name == "cp500" {
        decode_ebcdic_cp500(&bytes[skip..])
    } else {
        let (text, had_errors) = encoding.decode_without_bom_handling(&bytes[skip..]);
        (text.into_owned(), had_errors)
    };
    Decoded::new(text, encoding, encoding_name, source, had_errors)
}

/// 头/meta 的声明只是“候选”，正文有更硬的证据时纠正它。
///
/// 两个真实场景：
/// 1. `Content-Type: charset=utf-8`，正文是 GBK —— 解码会出现大量 U+FFFD；
/// 2. `Content-Type: charset=iso-8859-1`，正文是 UTF-8 —— 解码不会报错，但整段乱码。
fn refine_against_body(decoded: &mut Decoded, bytes: &[u8], tld: Option<&[u8]>) {
    if !matches!(
        decoded.source,
        CharsetSource::ContentType | CharsetSource::Meta
    ) {
        return;
    }
    let declared = Declared {
        encoding_name: decoded.encoding_name,
        source: decoded.source,
    };
    decoded.declared = Some(declared);

    // 情况 1：按声明解码出现明显替换字符 → 跳过声明重新探测正文。
    // 阈值保持保守：1–2 个坏字节可能只是页面局部损坏，不值得整页换编码。
    // 声明 UTF-8 且正文绝大多数非 ASCII 字节都组成合法 UTF-8 序列时，坏字节只是
    // 局部混入（评论里夹一段 Latin-1 之类），坚持 UTF-8 有损解码，不整页换编码。
    if decoded.replacement_chars >= 3 && !(decoded.encoding == UTF_8 && mostly_valid_utf8(bytes)) {
        let (encoding, name, source, skip) = detect_without_declarations(bytes, tld);
        if source == CharsetSource::Detected {
            let candidate = decode_slice(bytes, encoding, name, source, skip);
            if candidate.replacement_chars < decoded.replacement_chars
                && candidate.replacement_chars * 4 <= decoded.replacement_chars
            {
                let replacements = decoded.replacement_chars;
                *decoded = candidate;
                decoded.declared = Some(declared);
                decoded.overridden = Some(DecodeOverride::InvalidUnderDeclared { replacements });
                return;
            }
        }
    }

    // 情况 2：声明的是单字节编码，但整个正文是合法 UTF-8。
    // 真正单字节文本（é = 0xE9）不是合法 UTF-8；能通过校验且含多个非 ASCII
    // 字符的，几乎只可能是 UTF-8 被误当成 Latin-1/Windows-1252。
    if decoded.encoding.is_single_byte()
        && decoded.encoding != UTF_8
        && let Ok(text) = std::str::from_utf8(bytes)
        && text.chars().filter(|c| !c.is_ascii()).count() >= 2
    {
        *decoded = Decoded::new(
            text.to_owned(),
            UTF_8,
            "UTF-8",
            CharsetSource::Detected,
            false,
        );
        decoded.declared = Some(declared);
        decoded.overridden = Some(DecodeOverride::BodyIsUtf8);
    }
}

/// 合法多字节 UTF-8 序列数至少是非法片段数的这么多倍，才认定正文“本来就是 UTF-8，
/// 只是混进了少量坏字节”。
///
/// 依据：把旧编码的文本当 UTF-8 读，能碰巧组成合法多字节序列的比例很低——
/// GBK 双字节（首字节 0x81–0xFE、尾字节 0x40–0xFE）约 24% 的首字节落在 0xC2–0xDF、
/// 约 34% 的尾字节落在 0x80–0xBF，合法 2 字节序列约占 8%，3 字节更少，
/// 合计约 1 个合法序列对 9 个非法片段（比值 ≈ 0.1）；Shift_JIS 假名首字节 0x82/0x83
/// 本身就是续字节，比值更低；Latin-1/windows-1252 的孤立高位字节几乎从不合法（≈ 0）。
/// 反过来，真实 UTF-8 页面里混入几个坏字节时比值是几十到上千。取 2 倍：比旧编码
/// 误读的最坏情况高一个数量级以上，又远低于真实 UTF-8 页面的典型值。
const UTF8_VALID_TO_INVALID_RATIO: usize = 2;

/// 正文按 UTF-8 逐段校验：合法多字节序列数 ≥ 非法片段数 × [`UTF8_VALID_TO_INVALID_RATIO`]。
/// 非法片段按 WHATWG“最大子序列”计数，和解码器产生的 U+FFFD 一一对应。
fn mostly_valid_utf8(bytes: &[u8]) -> bool {
    let mut valid = 0usize;
    let mut invalid = 0usize;
    let mut rest = bytes;
    loop {
        let (good, err) = match std::str::from_utf8(rest) {
            Ok(text) => (text, None),
            Err(e) => {
                // valid_up_to 之前一定是合法 UTF-8，这里不会落到默认值。
                let good = std::str::from_utf8(&rest[..e.valid_up_to()]).unwrap_or_default();
                (good, Some((e.valid_up_to(), e.error_len())))
            }
        };
        valid += good.chars().filter(|c| !c.is_ascii()).count();
        let Some((up_to, len)) = err else { break };
        invalid += 1;
        match len {
            Some(len) => rest = &rest[up_to + len..],
            // 末尾被截断的多字节序列
            None => break,
        }
    }
    invalid == 0 || valid >= invalid.saturating_mul(UTF8_VALID_TO_INVALID_RATIO)
}

/// 核心探测入口，返回 (WHATWG 兼容 Encoding, 规范名称, 来源, BOM 长度)。
pub fn detect(
    bytes: &[u8],
    content_type: Option<&str>,
    tld: Option<&[u8]>,
) -> (&'static Encoding, &'static str, CharsetSource, usize) {
    detect_inner(bytes, content_type, tld, true)
}

/// 忽略 HTTP 头和 `<meta>` 的声明，只看正文。用于验证“声明是否说谎”。
fn detect_without_declarations(
    bytes: &[u8],
    tld: Option<&[u8]>,
) -> (&'static Encoding, &'static str, CharsetSource, usize) {
    detect_inner(bytes, None, tld, false)
}

fn detect_inner(
    bytes: &[u8],
    content_type: Option<&str>,
    tld: Option<&[u8]>,
    trust_declarations: bool,
) -> (&'static Encoding, &'static str, CharsetSource, usize) {
    // 1. BOM 优先（支持 UTF-32, UTF-16, UTF-8）
    if let Some((enc, len)) = detect_bom(bytes) {
        return (enc.whatwg_encoding(), enc.name(), CharsetSource::Bom, len);
    }

    if trust_declarations {
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

    const SAMPLE: &str = "这是一个中文测试页面，用来验证字符集识别。这句话应该有足够的长度，\
让抽取器保留正文内容，而不是把它当成导航或空壳页面。";

    #[test]
    fn lying_utf8_header_falls_back_to_gbk() {
        let body = gbk(SAMPLE);
        let d = decode_owned(body, Some("text/html; charset=utf-8"), Some(b"cn"));
        assert_eq!(d.text, SAMPLE);
        assert!(d.encoding_name.eq_ignore_ascii_case("gbk"));
        assert!(matches!(
            d.overridden,
            Some(DecodeOverride::InvalidUnderDeclared { replacements }) if replacements > 0
        ));
        let warning = d.warning().expect("warning");
        assert!(warning.contains("Content-Type declared UTF-8"), "{warning}");
        assert!(warning.contains("decoded as"), "{warning}");
    }

    #[test]
    fn lying_latin1_header_detects_utf8_body() {
        let body = SAMPLE.as_bytes().to_vec();
        let d = decode_owned(body, Some("text/html; charset=iso-8859-1"), Some(b"cn"));
        assert_eq!(d.text, SAMPLE);
        assert_eq!(d.overridden, Some(DecodeOverride::BodyIsUtf8));
        assert_eq!(d.encoding, UTF_8);
        let warning = d.warning().expect("warning");
        assert!(warning.contains("valid UTF-8"), "{warning}");
    }

    #[test]
    fn correct_legacy_headers_are_unchanged() {
        let gbk_body = gbk(SAMPLE);
        let d = decode(&gbk_body, Some("text/html; charset=gbk"), Some(b"cn"));
        assert_eq!(d.text, SAMPLE);
        assert_eq!(d.overridden, None);
        assert_eq!(d.source, CharsetSource::ContentType);

        let sjis = encoding_rs::SHIFT_JIS
            .encode("日本語のテストページです。")
            .0;
        let d = decode(&sjis, Some("text/html; charset=shift_jis"), None);
        assert_eq!(d.text, "日本語のテストページです。");
        assert_eq!(d.overridden, None);

        let big5 = encoding_rs::BIG5
            .encode("繁體中文測試頁面，內容長度足夠。")
            .0;
        let d = decode(&big5, Some("text/html; charset=big5"), None);
        assert_eq!(d.text, "繁體中文測試頁面，內容長度足夠。");
        assert_eq!(d.overridden, None);

        let euckr = encoding_rs::EUC_KR.encode("한국어 테스트 페이지입니다.").0;
        let d = decode(&euckr, Some("text/html; charset=euc-kr"), None);
        assert_eq!(d.text, "한국어 테스트 페이지입니다.");
        assert_eq!(d.overridden, None);

        // 真正的 Windows-1252：é = 0xE9，不是合法 UTF-8，不能被误判成 UTF-8。
        let french = encoding_rs::WINDOWS_1252
            .encode("Café, élève, français — déjà vu.")
            .0;
        let d = decode(&french, Some("text/html; charset=windows-1252"), None);
        assert!(d.text.starts_with("Café"));
        assert_eq!(d.overridden, None);
        assert_eq!(d.encoding, WINDOWS_1252);
    }

    /// 声明 UTF-8 的中文页面末尾混入几个 Latin-1 字节（`café naïve résumé`）。
    fn utf8_page_with_latin1_tail() -> Vec<u8> {
        let mut body = b"<html><head><meta charset=\"utf-8\"></head><body>".to_vec();
        for i in 0..40 {
            body.extend(format!("<p>第{i}段：{SAMPLE}</p>").as_bytes());
        }
        body.extend_from_slice(b"<p>caf\xe9 na\xefve r\xe9sum\xe9</p></body></html>");
        body
    }

    #[test]
    fn mostly_utf8_page_with_stray_bytes_keeps_declared_utf8() {
        for d in [
            decode_owned(utf8_page_with_latin1_tail(), None, Some(b"cn")),
            decode(&utf8_page_with_latin1_tail(), None, Some(b"cn")),
        ] {
            assert_eq!(d.encoding, UTF_8);
            assert_eq!(d.source, CharsetSource::Meta);
            assert_eq!(d.overridden, None);
            assert_eq!(d.replacement_chars, 4);
            assert!(d.text.contains(SAMPLE));
            assert!(d.text.contains("caf\u{FFFD} na\u{FFFD}ve"));
            let warning = d.warning().expect("warning");
            assert!(
                warning.contains("4 replacement characters under charset=UTF-8"),
                "{warning}"
            );
            assert!(!warning.contains("decoded as"), "{warning}");
        }
    }

    #[test]
    fn lying_utf8_header_still_detects_legacy_bodies() {
        // 反例：正文整体是旧编码时照样推翻 UTF-8 声明。
        let french = "Le café de la gare était fermé, mais l'élève a acheté un gâteau \
à la crème. Déjà vu : où est la bibliothèque ? Très bien, merci beaucoup.";
        let body = WINDOWS_1252.encode(french).0.into_owned();
        assert!(!mostly_valid_utf8(&body));
        let d = decode(&body, Some("text/html; charset=utf-8"), Some(b"fr"));
        assert_eq!(d.text, french);
        assert!(matches!(
            d.overridden,
            Some(DecodeOverride::InvalidUnderDeclared { .. })
        ));

        let sjis = encoding_rs::SHIFT_JIS
            .encode("これは日本語のテストページです。文字コードの判定を確認します。")
            .0
            .into_owned();
        assert!(!mostly_valid_utf8(&sjis));
        assert!(!mostly_valid_utf8(&gbk(SAMPLE)));
        assert!(mostly_valid_utf8(&utf8_page_with_latin1_tail()));
    }

    #[test]
    fn a_single_bad_byte_warns_without_switching_the_page() {
        let mut body = "hello 中文".as_bytes().to_vec();
        body.push(0xFF);
        let d = decode_owned(body, Some("text/html; charset=utf-8"), None);
        assert_eq!(d.overridden, None);
        assert_eq!(d.replacement_chars, 1);
        let warning = d.warning().expect("warning");
        assert!(warning.contains("1 replacement characters"), "{warning}");
        assert!(warning.contains("Content-Type"), "{warning}");
    }
}
