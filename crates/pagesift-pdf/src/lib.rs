//! PDF 转干净的 Markdown：文本提取、版面重组（分栏、标题、页眉页脚、断词），
//! 扫描件只做标记不做 OCR。PDF 对象解析和解压交给 lopdf，其余自研。
//!
//! ```no_run
//! let bytes = std::fs::read("paper.pdf").unwrap();
//! let doc = pagesift_pdf::extract(&bytes).unwrap();
//! println!("{}", doc.markdown);
//! ```

#![forbid(unsafe_code)]

mod cmap;
mod encoding;
mod font;
mod interp;
mod layout;
mod lexer;
mod obj;

use std::collections::HashMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use lopdf::{Document, LoadOptions, Object};

use crate::interp::Interp;
use crate::obj::{deref, text_string};

#[derive(Debug, Clone)]
pub struct Options {
    /// 单个文件的处理时间上限（文件解析之后的阶段按这个时间检查）。
    pub timeout: Option<Duration>,
    /// 输入大小上限。
    pub max_input_bytes: usize,
    /// 页数上限，超过直接报错。
    pub max_pages: usize,
    /// 单个流解压后的大小上限，防解压炸弹。
    pub max_stream_bytes: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            timeout: Some(Duration::from_secs(30)),
            max_input_bytes: 256 << 20,
            max_pages: 5000,
            max_stream_bytes: 64 << 20,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// 不是 PDF，或者损坏到无法解析。
    Malformed(String),
    /// 加密且需要密码。
    Encrypted,
    /// 超过大小或页数上限。
    TooLarge(&'static str),
    /// 超过处理时间上限。
    Timeout,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Malformed(m) => write!(f, "malformed PDF: {m}"),
            Error::Encrypted => f.write_str("PDF is encrypted and requires a password"),
            Error::TooLarge(what) => write!(f, "PDF exceeds the {what} limit"),
            Error::Timeout => f.write_str("PDF processing timed out"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    /// 尽量转成 RFC 3339；格式不认识时原样保留。
    pub creation_date: Option<String>,
    pub modification_date: Option<String>,
    pub page_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PdfPage {
    /// 从 1 开始的页码。
    pub number: usize,
    pub markdown: String,
    /// 非空白字符数。
    pub text_chars: usize,
    pub image_count: usize,
    /// 页面有大面积图像而几乎没有文字，像是扫描件。
    pub scanned: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PdfDocument {
    /// 全文 Markdown，跨页的段落已接好。
    pub markdown: String,
    pub pages: Vec<PdfPage>,
    pub metadata: Metadata,
    /// 至少一半的页面是扫描件（没有文字层）。
    pub scanned: bool,
}

/// 用默认选项提取。
pub fn extract(bytes: &[u8]) -> Result<PdfDocument, Error> {
    extract_with(bytes, &Options::default())
}

pub fn extract_with(bytes: &[u8], opts: &Options) -> Result<PdfDocument, Error> {
    // 兜底：依赖库在畸形输入上万一 panic，也转成错误返回
    catch_unwind(AssertUnwindSafe(|| run(bytes, opts)))
        .unwrap_or_else(|_| Err(Error::Malformed("parser panicked".into())))
}

const SCANNED_MAX_CHARS: usize = 32;
const SCANNED_MIN_COVERAGE: f64 = 0.3;

fn run(bytes: &[u8], opts: &Options) -> Result<PdfDocument, Error> {
    let start = Instant::now();
    let deadline = opts.timeout.map(|t| start + t);
    if bytes.len() > opts.max_input_bytes {
        return Err(Error::TooLarge("input size"));
    }
    let head = &bytes[..bytes.len().min(1024)];
    if !head.windows(5).any(|w| w == b"%PDF-") {
        return Err(Error::Malformed("missing %PDF header".into()));
    }
    let load = LoadOptions {
        max_decompressed_size: Some(opts.max_stream_bytes),
        ..LoadOptions::default()
    };
    let doc = Document::load_mem_with_options(bytes, load).map_err(|e| match e {
        lopdf::Error::InvalidPassword | lopdf::Error::Decryption(_) => Error::Encrypted,
        lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded { .. }) => {
            Error::TooLarge("decompressed stream size")
        }
        other => Error::Malformed(other.to_string()),
    })?;
    // 空口令加密的文件 lopdf 会自动解开；还带着 /Encrypt 说明需要密码
    if doc.is_encrypted() {
        return Err(Error::Encrypted);
    }
    check_deadline(deadline)?;

    let page_ids = doc.get_pages();
    if page_ids.len() > opts.max_pages {
        return Err(Error::TooLarge("page count"));
    }
    if page_ids.is_empty() {
        return Err(Error::Malformed("document has no pages".into()));
    }

    let mut fonts = HashMap::new();
    let mut frags = Vec::with_capacity(page_ids.len());
    let mut heights = Vec::with_capacity(page_ids.len());
    let mut stats = Vec::with_capacity(page_ids.len());
    for (_, id) in page_ids {
        check_deadline(deadline)?;
        let page = match doc.get_object(id).map(|o| deref(&doc, o)) {
            Ok(Object::Dictionary(d)) => d,
            _ => {
                frags.push(Vec::new());
                heights.push(792.0);
                stats.push((0, 0, false));
                continue;
            }
        };
        let ink = Interp::new(&doc, &mut fonts, deadline, opts.max_stream_bytes)
            .run_page(page)
            .map_err(|_| Error::Timeout)?;
        let text_chars: usize = ink
            .glyphs
            .iter()
            .map(|g| g.text.chars().filter(|c| !c.is_whitespace()).count())
            .sum();
        let coverage = ink.image_area / (ink.width * ink.height);
        let scanned = ink.image_count > 0
            && coverage >= SCANNED_MIN_COVERAGE
            && text_chars < SCANNED_MAX_CHARS;
        stats.push((text_chars, ink.image_count, scanned));
        heights.push(ink.height);
        frags.push(layout::fragments(&ink.glyphs));
    }
    check_deadline(deadline)?;

    layout::remove_furniture(&mut frags, &heights);
    let mut paras = Vec::with_capacity(frags.len());
    for f in frags {
        check_deadline(deadline)?;
        paras.push(layout::paragraphs(layout::blocks(f)));
    }
    let (page_md, markdown) = layout::render(&paras);

    let pages: Vec<PdfPage> = page_md
        .into_iter()
        .zip(stats)
        .enumerate()
        .map(|(i, (md, (text_chars, image_count, scanned)))| PdfPage {
            number: i + 1,
            markdown: md,
            text_chars,
            image_count,
            scanned,
        })
        .collect();
    let scanned_pages = pages.iter().filter(|p| p.scanned).count();
    let scanned = scanned_pages > 0 && scanned_pages * 2 >= pages.len();
    let mut metadata = metadata(&doc);
    metadata.page_count = pages.len();
    Ok(PdfDocument {
        markdown,
        pages,
        metadata,
        scanned,
    })
}

fn check_deadline(deadline: Option<Instant>) -> Result<(), Error> {
    match deadline {
        Some(d) if Instant::now() >= d => Err(Error::Timeout),
        _ => Ok(()),
    }
}

fn metadata(doc: &Document) -> Metadata {
    let info = doc
        .trailer
        .get(b"Info")
        .ok()
        .map(|o| deref(doc, o))
        .and_then(|o| o.as_dict().ok());
    let field = |key: &[u8]| -> Option<String> {
        let v = match deref(doc, info?.get(key).ok()?) {
            Object::String(s, _) => text_string(s),
            _ => return None,
        };
        let v = v.trim().to_string();
        (!v.is_empty()).then_some(v)
    };
    Metadata {
        title: field(b"Title"),
        author: field(b"Author"),
        subject: field(b"Subject"),
        keywords: field(b"Keywords"),
        creator: field(b"Creator"),
        producer: field(b"Producer"),
        creation_date: field(b"CreationDate").map(|d| pdf_date(&d)),
        modification_date: field(b"ModDate").map(|d| pdf_date(&d)),
        page_count: 0,
    }
}

/// `D:YYYYMMDDHHmmSSOHH'mm'` 转 RFC 3339；不认识的格式原样返回。
fn pdf_date(raw: &str) -> String {
    let s = raw.trim().strip_prefix("D:").unwrap_or(raw.trim());
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() < 4 {
        return raw.to_string();
    }
    let part = |a: usize, b: usize, d: &str| digits.get(a..b).unwrap_or(d).to_string();
    let date = format!(
        "{}-{}-{}T{}:{}:{}",
        &digits[..4],
        part(4, 6, "01"),
        part(6, 8, "01"),
        part(8, 10, "00"),
        part(10, 12, "00"),
        part(12, 14, "00"),
    );
    let rest = &s[digits.len()..];
    let tz = match rest.chars().next() {
        Some(sign @ ('+' | '-')) => {
            let tzd: String = rest[1..].chars().filter(char::is_ascii_digit).collect();
            let hh = tzd.get(..2).unwrap_or("00");
            let mm = tzd.get(2..4).unwrap_or("00");
            format!("{sign}{hh}:{mm}")
        }
        Some('Z') => "Z".to_string(),
        _ => String::new(),
    };
    format!("{date}{tz}")
}

/// 模糊测试入口：不经过 `catch_unwind`，直接调用自研的解析器，好让 panic 暴露出来。
#[doc(hidden)]
pub mod fuzzing {
    use super::*;

    pub fn extract_unguarded(bytes: &[u8], opts: &Options) -> Result<PdfDocument, Error> {
        run(bytes, opts)
    }

    /// 返回操作符个数。
    pub fn lex(data: &[u8]) -> usize {
        let mut lx = lexer::Lexer::new(data);
        let mut n = 0;
        while lx.next_op().is_some() {
            n += 1;
        }
        n
    }

    pub fn cmap(data: &[u8], probe: &[u8]) -> Option<String> {
        let cm = cmap::CMap::parse(data);
        let len = cm.code_len(probe).min(probe.len()).max(1);
        let code = probe
            .iter()
            .take(len)
            .fold(0u32, |v, &b| v << 8 | u32::from(b));
        cm.cid(code);
        cm.unicode(code)
    }

    /// 字形：(文本, x, y, 宽, 字号, 粗体)。
    pub type Glyph = (String, f64, f64, f64, f64, bool);

    /// 按页排版成 Markdown。
    pub fn layout(pages: &[Vec<Glyph>]) -> String {
        let mut frags: Vec<Vec<layout::Line>> = pages
            .iter()
            .map(|gs| {
                let glyphs: Vec<interp::Glyph> = gs
                    .iter()
                    .map(|(text, x, y, w, size, bold)| interp::Glyph {
                        text: text.clone(),
                        x: *x,
                        y: *y,
                        x_end: x + w,
                        size: *size,
                        bold: *bold,
                    })
                    .collect();
                layout::fragments(&glyphs)
            })
            .collect();
        let heights = vec![792.0; frags.len()];
        layout::remove_furniture(&mut frags, &heights);
        let paras: Vec<_> = frags
            .into_iter()
            .map(|f| layout::paragraphs(layout::blocks(f)))
            .collect();
        layout::render(&paras).1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(
            pdf_date("D:20240315083000+08'00'"),
            "2024-03-15T08:30:00+08:00"
        );
        assert_eq!(pdf_date("D:2024"), "2024-01-01T00:00:00");
        assert_eq!(pdf_date("D:20240315083000Z"), "2024-03-15T08:30:00Z");
        assert_eq!(pdf_date("yesterday"), "yesterday");
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(extract(b"hello"), Err(Error::Malformed(_))));
        assert!(matches!(extract(b""), Err(Error::Malformed(_))));
        assert!(extract(b"%PDF-1.7\n%%EOF").is_err());
    }
}
