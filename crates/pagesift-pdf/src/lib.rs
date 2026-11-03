//! PDF 转干净的 Markdown：文本提取、版面重组（分栏、标题、页眉页脚、断词），
//! 扫描件只做标记不做 OCR。从文件结构（xref、对象、解压、解密）到版面全部自研。
//!
//! ```no_run
//! let bytes = std::fs::read("paper.pdf").unwrap();
//! let doc = pagesift_pdf::extract(&bytes).unwrap();
//! println!("{}", doc.markdown);
//! ```

#![forbid(unsafe_code)]

mod budget;
mod cmap;
mod encoding;
mod figure;
mod file;
mod font;
mod fontfile;
mod geom;
mod image;
mod interp;
mod layout;
mod lexer;
mod math;
mod obj;
mod orient;
mod page;
mod quality;
mod run;
mod short;
mod structure;
mod table;
mod timeout;

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use crate::file::{Document, Object};
use crate::obj::{deref, text_string};
use crate::run::run;

pub use crate::image::{ImageEncoding, ImagePlacement, PdfImage};
pub use crate::quality::PageQuality;
pub use crate::timeout::{observe_parse, parse_timeout};

#[derive(Debug, Clone)]
pub struct Options {
    /// 单个文件的处理时间的硬上限（用户给的时限）。实际时限还要和按字节数估的时限取小
    /// （见 [`Options::adaptive_timeout`]、[`parse_timeout`]）。
    pub timeout: Option<Duration>,
    /// 按“字节数 → 耗时”的回归给时限（默认开）：小文件不会卡上几十秒，大文件也不会被
    /// 固定时限截断。关掉时只用 `timeout`。
    pub adaptive_timeout: bool,
    /// 输入大小上限。
    pub max_input_bytes: usize,
    /// 页数上限，超过直接报错。
    pub max_pages: usize,
    /// 单个流解压后的大小上限，防解压炸弹。
    pub max_stream_bytes: usize,
    /// 只处理前 N 页（Firecrawl 的 `maxPages`）；后面的页不解释、不排版。
    /// `metadata.page_count` 仍是整个文件的页数；页数上限按实际处理的页数算。
    pub page_limit: Option<usize>,
    /// 图片识别与导出（默认只标出扫描页）。
    pub images: ImageOptions,
}

/// 图片识别与导出。
///
/// 扫描页（整页是图、没有文字层）总会标出：Markdown 里写一行说明并引用那张图，那张图进
/// [`PdfDocument::images`]。其余图片要打开 `list` 才识别。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageOptions {
    /// 列出所有图片（编码、尺寸、色彩空间、字节数、哈希、位置）。调用方要 `images` 格式时打开。
    pub list: bool,
    /// Markdown 里按阅读顺序放图片引用 `![](images/<文件名>)`，同一张图只引用第一次出现。
    /// 打开它也就打开了 `list`。
    pub markdown: bool,
    /// 按原编码导出图片字节（[`PdfImage::data`]）。打开它也就打开了 `list`。
    pub export: bool,
}

impl ImageOptions {
    pub(crate) fn any(&self) -> bool {
        self.list || self.markdown || self.export
    }
}

impl Default for Options {
    fn default() -> Self {
        Options {
            timeout: None,
            adaptive_timeout: true,
            max_input_bytes: 256 << 20,
            max_pages: 5000,
            max_stream_bytes: 64 << 20,
            page_limit: None,
            images: ImageOptions::default(),
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
    /// 按页质量分（乱码、字体映射缺失、阅读顺序跳变）和复核情况。
    pub quality: PageQuality,
    /// 这页画的图在 [`PdfDocument::images`] 里的下标，按阅读顺序，同一张图可能出现多次。
    pub images: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PdfDocument {
    /// 全文 Markdown，跨页的段落已接好。
    pub markdown: String,
    pub pages: Vec<PdfPage>,
    pub metadata: Metadata,
    /// 至少一半的页面是扫描件（没有文字层）。
    pub scanned: bool,
    /// 从版面推断的文档标题：第一页的第一个一级标题。`metadata.title`（/Info 的 /Title）
    /// 缺失时调用方可以用它；两者都给，由调用方决定。
    pub inferred_title: Option<String>,
    /// 文件损坏但部分恢复时的提示（`标识: 说明`）：`pdf_repaired`（xref 不可用或偏移指错，
    /// 对象是扫描全文找到的）、`pages_missing`（页树引用的页面对象丢了，结果少页）。
    pub warnings: Vec<String>,
    /// 识别出的图片（同一张图只有一条）。没打开 [`ImageOptions::list`] 时只有扫描页的图。
    pub images: Vec<PdfImage>,
}

/// 用默认选项提取。
pub fn extract(bytes: &[u8]) -> Result<PdfDocument, Error> {
    extract_with(bytes, &Options::default())
}

pub fn extract_with(bytes: &[u8], opts: &Options) -> Result<PdfDocument, Error> {
    let start = Instant::now();
    let mut opts = opts.clone();
    opts.timeout = effective_timeout(bytes.len(), &opts);
    // 兜底：依赖库在畸形输入上万一 panic，也转成错误返回
    let out = catch_unwind(AssertUnwindSafe(|| run(bytes, &opts, true)))
        .unwrap_or_else(|_| Err(Error::Malformed("parser panicked".into())));
    if out.is_ok() {
        observe_parse(bytes.len(), start.elapsed());
    }
    out
}

/// 实际用的时限：用户上限和按字节数估的时限取小。
fn effective_timeout(bytes: usize, opts: &Options) -> Option<Duration> {
    if opts.adaptive_timeout {
        Some(parse_timeout(bytes, opts.timeout))
    } else {
        opts.timeout
    }
}

fn check_deadline(deadline: Option<Instant>) -> Result<(), Error> {
    match deadline {
        Some(d) if Instant::now() >= d => Err(Error::Timeout),
        _ => Ok(()),
    }
}

fn metadata(doc: &Document) -> Metadata {
    let info = doc.trailer.get(b"Info").and_then(|o| match deref(doc, o) {
        Object::Dictionary(d) => Some(d),
        _ => None,
    });
    let field = |key: &[u8]| -> Option<String> {
        let v = match deref(doc, info?.get(key)?) {
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
        let mut opts = opts.clone();
        opts.timeout = effective_timeout(bytes.len(), &opts);
        run(bytes, &opts, true)
    }

    /// 评测用：不读结构树和书签，只看版面推断出的标题。
    pub fn extract_inferred(bytes: &[u8], opts: &Options) -> Result<PdfDocument, Error> {
        let mut opts = opts.clone();
        opts.timeout = effective_timeout(bytes.len(), &opts);
        run(bytes, &opts, false)
    }

    /// 解一段 JPEG（DCTDecode 的图像编码层数据）：(宽, 高, 分量数, 像素)。
    /// `color_transform` 同 DCTDecode 的 /ColorTransform；`gray` 为真时输出灰度。
    /// 内存上限取 [`Options::max_stream_bytes`] 的默认值。
    pub fn decode_jpeg(
        data: &[u8],
        color_transform: Option<bool>,
        gray: bool,
    ) -> Result<(u32, u32, u8, Vec<u8>), String> {
        let max = Options::default().max_stream_bytes;
        let r = if gray {
            image::decode::jpeg::decode_gray(data, color_transform, max)?
        } else {
            image::decode::jpeg::decode(data, color_transform, max)?
        };
        Ok((r.width, r.height, r.channels, r.data))
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
        cm.unicode(code).map(|s| s.to_string())
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
                        text: text.as_str().into(),
                        x: *x,
                        y: *y,
                        x_end: x + w,
                        size: *size,
                        bold: *bold,
                        math: crate::font::MathKind::Text,
                        mcid: None,
                        artifact: false,
                    })
                    .collect();
                layout::fragments(&glyphs, None)
            })
            .collect();
        let heights = vec![792.0; frags.len()];
        layout::remove_furniture(&mut frags, &heights);
        let paras: Vec<_> = frags
            .into_iter()
            .map(|f| layout::paragraphs(layout::blocks(f)))
            .collect();
        layout::render(&paras, layout::Mode::Infer).1
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
