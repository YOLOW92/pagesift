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
mod interp;
mod layout;
mod lexer;
mod math;
mod obj;
mod orient;
mod structure;
mod table;

use std::collections::HashMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use crate::file::{Document, Object, OpenError};

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
    /// 只处理前 N 页（Firecrawl 的 `maxPages`）；后面的页不解释、不排版。
    /// `metadata.page_count` 仍是整个文件的页数；页数上限按实际处理的页数算。
    pub page_limit: Option<usize>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            timeout: Some(Duration::from_secs(30)),
            max_input_bytes: 256 << 20,
            max_pages: 5000,
            max_stream_bytes: 64 << 20,
            page_limit: None,
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
    /// 从版面推断的文档标题：第一页的第一个一级标题。`metadata.title`（/Info 的 /Title）
    /// 缺失时调用方可以用它；两者都给，由调用方决定。
    pub inferred_title: Option<String>,
    /// 文件损坏但部分恢复时的提示（`标识: 说明`）：`pdf_repaired`（xref 不可用或偏移指错，
    /// 对象是扫描全文找到的）、`pages_missing`（页树引用的页面对象丢了，结果少页）。
    pub warnings: Vec<String>,
}

/// 用默认选项提取。
pub fn extract(bytes: &[u8]) -> Result<PdfDocument, Error> {
    extract_with(bytes, &Options::default())
}

pub fn extract_with(bytes: &[u8], opts: &Options) -> Result<PdfDocument, Error> {
    // 兜底：依赖库在畸形输入上万一 panic，也转成错误返回
    catch_unwind(AssertUnwindSafe(|| run(bytes, opts, true)))
        .unwrap_or_else(|_| Err(Error::Malformed("parser panicked".into())))
}

const SCANNED_MAX_CHARS: usize = 32;
const SCANNED_MIN_COVERAGE: f64 = 0.3;

/// `use_structure`：用不用写入方声明的结构（结构树、书签）。关掉只用于评测推断本身的水平。
fn run(bytes: &[u8], opts: &Options, use_structure: bool) -> Result<PdfDocument, Error> {
    let start = Instant::now();
    let deadline = opts.timeout.map(|t| start + t);
    let _budget = budget::enter(deadline);
    if bytes.len() > opts.max_input_bytes {
        return Err(Error::TooLarge("input size"));
    }
    let head = &bytes[..bytes.len().min(1024)];
    if !head.windows(5).any(|w| w == b"%PDF-") {
        return Err(Error::Malformed("missing %PDF header".into()));
    }
    let doc = Document::open(bytes, opts.max_stream_bytes).map_err(|e| match e {
        OpenError::Encrypted => Error::Encrypted,
        OpenError::Malformed(m) => Error::Malformed(m.into()),
        OpenError::Timeout => Error::Timeout,
    })?;
    check_deadline(deadline)?;

    let tree = doc.page_tree();
    let mut page_ids = tree.ids.clone();
    let total_pages = page_ids.len();
    if let Some(limit) = opts.page_limit {
        page_ids.truncate(limit.max(1));
    }
    if page_ids.len() > opts.max_pages {
        return Err(Error::TooLarge("page count"));
    }
    if page_ids.is_empty() {
        return Err(Error::Malformed("document has no pages".into()));
    }

    let structure = if use_structure {
        structure::read(&doc, &page_ids)
    } else {
        structure::Structure::default()
    };
    let mut tagged_lines = 0;
    let mut bases = Vec::with_capacity(page_ids.len());

    // 书签条目按页分好组，标题匹配的长度上限也只算一次
    let outline_by_page = structure::outline_by_page(&structure.outline, page_ids.len());
    let outline_limit = structure::title_limit(&structure.outline);
    // 调试输出开关只读一次
    let dump_tables = std::env::var_os("PAGESIFT_PDF_TABLES").is_some();
    let mut fonts = HashMap::new();
    let mut frags = Vec::with_capacity(page_ids.len());
    let mut heights = Vec::with_capacity(page_ids.len());
    let mut stats = Vec::with_capacity(page_ids.len());
    for (index, &id) in page_ids.iter().enumerate() {
        check_deadline(deadline)?;
        let page = match doc.get_object(id).map(|o| deref(&doc, o)) {
            Some(Object::Dictionary(d)) => d,
            _ => {
                frags.push(Vec::new());
                bases.push(None);
                heights.push(792.0);
                stats.push((0, 0, false));
                continue;
            }
        };
        let mut ink = Interp::new(&doc, &mut fonts, deadline, opts.max_stream_bytes)
            .run_page(page)
            .map_err(|_| Error::Timeout)?;
        // 旋转的字：整页横排的转正，少数竖写的拼成行，后面和别的行一起排版
        let turned_lines = orient::orient(&mut ink);
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
        bases.push(ink.base);
        let mut page_frags =
            layout::fragments(&ink.glyphs, structure.tags.as_ref().map(|t| &t[index]));
        page_frags.extend(turned_lines);
        if let Some(tags) = &structure.tags {
            tagged_lines += structure::tag_lines(&mut page_frags, &tags[index]);
        }
        // 表格：格子里的片段换成一个原样输出的 Markdown 表格，放在表格顶部的位置
        // 书签声明的标题行（这一页和前一页的条目）标成 keep：不归图，也不当页眉页脚删
        structure::keep_outline_lines(&mut page_frags, &outline_by_page[index], outline_limit);
        // 矢量图：图内文字（刻度、图例、标注）不进表格检测、不当标题，合成一个原样输出的块
        if std::env::var_os("PAGESIFT_DBG").is_some() {
            let bent: Vec<_> = ink
                .shapes
                .iter()
                .filter(|s| s.visible && (s.slanted + s.curves) > 0)
                .collect();
            let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for s in &bent {
                x0 = x0.min(s.x0);
                y0 = y0.min(s.y0);
                x1 = x1.max(s.x1);
                y1 = y1.max(s.y1);
            }
            eprintln!(
                "dbg {} w {} h {} shapes {} bent {} box {:.0} {:.0} {:.0} {:.0} rules {}",
                index + 1,
                ink.width,
                ink.height,
                ink.shapes.len(),
                bent.len(),
                x0,
                y0,
                x1,
                y1,
                ink.rules.len()
            );
        }
        let figures = figure::detect(&ink.shapes, &ink.rules, &page_frags, ink.width, ink.height);
        ink.shapes = Vec::new();
        if dump_tables {
            for f in &figures {
                let head: String = figure::text_of(&page_frags, &f.lines)
                    .chars()
                    .take(100)
                    .collect();
                eprintln!(
                    "figure\t{}\t{:.0}\t{:.0}\t{:.0}\t{:.0}\t{}\t{}",
                    index + 1,
                    f.x0,
                    f.top,
                    f.x1,
                    f.bottom,
                    f.lines.len(),
                    head.replace('\n', " / ")
                );
            }
        }
        let (figure_blocks, table_rules) =
            figure::take(&figures, &mut page_frags, std::mem::take(&mut ink.rules));
        // 没被图区认领的旋转行不参加表格识别，识别完再放回去
        let (turned_left, mut rest): (Vec<_>, Vec<_>) = std::mem::take(&mut page_frags)
            .into_iter()
            .partition(|l| l.turned);
        let tables = table::extract(&table_rules, &mut rest, &figures, ink.width, ink.height);
        page_frags = rest;
        page_frags.extend(turned_left);
        page_frags.extend(figure_blocks);
        if dump_tables {
            for t in &tables {
                let cols = t.rows.iter().map(Vec::len).max().unwrap_or(0);
                let head: Vec<String> = t.rows.iter().take(3).map(|r| r.join(" | ")).collect();
                eprintln!(
                    "table\t{}\t{:.0}\t{:.0}\t{:.0}\t{:.0}\t{}x{}\t{}",
                    index + 1,
                    t.x0,
                    t.top,
                    t.x1,
                    t.bottom,
                    t.rows.len(),
                    cols,
                    head.join(" // ")
                );
            }
        }
        if !tables.is_empty() {
            let mut sizes: Vec<f64> = page_frags.iter().map(|f| f.size).collect();
            sizes.sort_by(f64::total_cmp);
            let size = sizes.get(sizes.len() / 2).copied().unwrap_or(10.0);
            for t in tables {
                page_frags.push(layout::Line {
                    text: t.markdown(),
                    x0: t.x0,
                    x1: t.x1,
                    y: t.top,
                    size,
                    bold: false,
                    raw: true,
                    mcid: None,
                    artifact: false,
                    hint: None,
                    group: None,
                    keep: false,
                    turned: false,
                });
            }
        }
        frags.push(page_frags);
    }
    check_deadline(deadline)?;

    // 书签标题行的 keep 已在逐页循环里标过（图区识别之前），这里不再重复
    layout::remove_furniture(&mut frags, &heights);
    let mut paras = Vec::with_capacity(frags.len());
    for f in frags {
        check_deadline(deadline)?;
        paras.push(layout::paragraphs(layout::blocks(f)));
    }
    // 写入方给了标题就照着用：结构树优先，其次书签（对上一半以上的条目才算数）
    // 结构树里只有一两个标题（常见的是只标了文档题名）说明写入方没给章节打标签，不能拿它定标题
    let mode = if tagged_lines >= 2 {
        // 书签里有、结构树没标成标题的（常见：粗体段落当小节标题），也算标题
        structure::mark_outline(&mut paras, &structure.outline, &bases);
        layout::Mode::Hinted
    } else {
        let located = structure
            .outline
            .iter()
            .filter(|e| e.page.is_some())
            .count();
        // 结构树没起作用时，行上的少量标签提示不能留着（否则会把段落切碎）
        for p in paras.iter_mut().flatten() {
            p.hint = None;
        }
        let matched = structure::mark_outline(&mut paras, &structure.outline, &bases);
        if matched >= 2 && matched * 2 >= located {
            layout::Mode::Hinted
        } else {
            layout::Mode::Infer
        }
    };
    let (page_md, markdown) = layout::render(&paras, mode);

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
    metadata.page_count = total_pages;
    let inferred_title = pages
        .first()
        .and_then(|p| structure::inferred_title(&p.markdown));
    // 对象都读完了，这时才知道有没有靠扫描纠正过偏移
    let mut damage = doc.damage(&tree);
    // 修复过的文件再查一遍：页面字典在、页数对，内容流或字体却可能丢了（截断时常见）
    if damage.repaired || damage.rescued {
        (damage.incomplete_pages, damage.fontless_pages) =
            doc.incomplete_pages(&page_ids, opts.max_stream_bytes);
    }
    let warnings = damage.warnings(total_pages);
    Ok(PdfDocument {
        markdown,
        pages,
        metadata,
        scanned,
        inferred_title,
        warnings,
    })
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
        run(bytes, opts, true)
    }

    /// 评测用：不读结构树和书签，只看版面推断出的标题。
    pub fn extract_inferred(bytes: &[u8], opts: &Options) -> Result<PdfDocument, Error> {
        run(bytes, opts, false)
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
