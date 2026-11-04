//! 文档转 Markdown：Word、Excel、PowerPoint、EPUB、CSV、OpenDocument、RTF、FB2、邮件、压缩包和文本类格式。
//!
//! 照 ECMA-376（OOXML）、EPUB 3（OCF/OPF）、RFC 4180 自己实现，ZIP 目录和 XML 解析也是自己写的
//! （见 [`zip`]、[`xml`]），只有 deflate 解压用 miniz_oxide。所有输入都按不可信处理：
//! 解压量、单元格数、嵌套层数都有上限（[`Limits`]），XML 不展开任何声明的实体。
//!
//! EPUB 的正文是 XHTML，转换交给调用方（[`Options::html`]），这样和网页走同一个 HTML 提取器。

#![forbid(unsafe_code)]

use std::fmt;
use std::time::Instant;

mod archive;
mod budget;
mod code;
mod compress;
mod csv;
mod dbf;
mod docx;
mod email;
mod epub;
mod fb2;
mod format;
mod hwpx;
mod ipynb;
mod legacy;
mod md;
mod media;
mod mobi;
mod odf;
mod ole;
mod opc;
mod opml;
mod pptx;
mod rtf;
mod sqlite;
mod subtitle;
mod tar;
mod text;
mod wordperfect;
mod xlsx;
mod xml;
mod xmldoc;
mod zip;

pub use format::Format;
pub use media::{Media, MediaLocation, media, media_bytes};

/// 资源上限。默认值足够正常文档用，又能挡住炸弹。
#[derive(Debug, Clone)]
pub struct Limits {
    /// ZIP 里的条目数。
    pub max_zip_entries: usize,
    /// 单个条目解压后的大小。
    pub max_entry_bytes: u64,
    /// 整个文件累计解压的大小。
    pub max_total_bytes: u64,
    /// 大条目（4 MB 以上）的压缩比上限。
    pub max_ratio: u64,
    /// 输出的表格单元格总数（Excel、CSV）；超过的行截掉并警告。
    pub max_cells: usize,
    /// Excel 的列数上限（规范上限是 16384 列）；超过的列截掉。
    pub max_columns: usize,
    /// 转出的文字累计超过这么多字节就停止转换：已经转出的部分照常返回，加 `output_truncated` 警告
    /// （和 HTML 的响应体上限同一个量级；见 `budget` 模块）。
    pub max_output_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_zip_entries: 20_000,
            max_entry_bytes: 256 << 20,
            max_total_bytes: 512 << 20,
            max_ratio: 400,
            max_cells: 2_000_000,
            max_columns: 1_024,
            max_output_bytes: 16 << 20,
        }
    }
}

/// EPUB 正文（XHTML）转 Markdown 的函数：参数是 XHTML 和它在包里的路径。
pub type HtmlToMarkdown<'a> = &'a (dyn Fn(&str, &str) -> String + Sync);

/// 转换压缩包或邮件里的一个文件：参数是字节、文件名和还能输出多少字节。
/// 类型不支持时返回 `Ok(None)`，只列在清单里。
pub type MemberToMarkdown<'a> =
    &'a (dyn Fn(&[u8], &str, usize) -> Result<Option<Converted>, Error> + Sync);

#[derive(Clone, Default)]
pub struct Options<'a> {
    pub limits: Limits,
    /// 超过这个时间点就停下报 [`Error::Timeout`]（按块检查：段落、行、幻灯片、章节）。
    pub deadline: Option<Instant>,
    /// EPUB 必须给；没给时 EPUB 报 [`Error::Unsupported`]。
    pub html: Option<HtmlToMarkdown<'a>>,
    /// 压缩包成员和邮件附件的转换函数。没给时只列清单。
    pub member: Option<MemberToMarkdown<'a>>,
}

/// 转换结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Converted {
    pub markdown: String,
    /// 文档属性里的标题（docProps/core.xml 或 OPF 的 dc:title）。
    pub title: Option<String>,
    /// 作者、语言等其他元数据（键是 Dublin Core 的名字）。
    pub metadata: Vec<(String, String)>,
    /// 截断、跳过的内容等提示。
    pub warnings: Vec<String>,
    /// 页、工作表或幻灯片的个数（EPUB 是 spine 里的文档数，CSV 是行数）。
    pub parts: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// 不是这种格式，或者坏得读不了。
    Malformed(String),
    /// 加密（需要口令或有 DRM）。
    Encrypted,
    /// 超过资源上限。
    TooLarge(&'static str),
    /// 超时。
    Timeout,
    /// 格式里用到了不支持的东西。
    Unsupported(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Malformed(m) => write!(f, "malformed document: {m}"),
            Error::Encrypted => f.write_str("document is encrypted"),
            Error::TooLarge(what) => write!(f, "document exceeds the {what} limit"),
            Error::Timeout => f.write_str("document conversion timed out"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// 转换一份已经解码成 UTF-8 的 CSV。[`convert`] 自己只认 BOM、UTF-8 和 Windows-1252；
/// 知道字符集（HTTP 头的 `charset`、探测器）的调用方先解码再用这个入口。
pub fn convert_csv_text(text: &str, opts: &Options<'_>) -> Result<Converted, Error> {
    budgeted(opts, || csv::convert_text(text, opts))
}

/// 转换一份已经解码成 UTF-8 的文本类文档（[`Format::decodes_text`] 为真的格式）。
pub fn convert_text(text: &str, format: Format, opts: &Options<'_>) -> Result<Converted, Error> {
    budgeted(opts, || text_format(text, format, opts))
}

fn text_format(text: &str, format: Format, opts: &Options<'_>) -> Result<Converted, Error> {
    match format {
        Format::Csv => csv::convert_text(text, opts),
        Format::Subtitle => subtitle::convert(text, opts),
        Format::Notebook => ipynb::convert(text, opts),
        Format::Text | Format::Markdown | Format::Fenced(_) => text::convert(text, format, opts),
        other => Err(Error::Unsupported(format!(
            "{} is not a text format",
            other.extension()
        ))),
    }
}

/// 在输出预算下运行一次转换；预算用完提前停下时加 `output_truncated` 警告。
/// 依赖代码在畸形输入上万一 panic，也转成错误返回。
fn budgeted(
    opts: &Options<'_>,
    run: impl FnOnce() -> Result<Converted, Error>,
) -> Result<Converted, Error> {
    let guard = budget::enter(opts.limits.max_output_bytes);
    let mut out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
        .unwrap_or_else(|_| Err(Error::Malformed("converter panicked".into())))?;
    // 各格式按产出的文字记账，表格的分隔符、转义、批量记账的尾数不在里面：
    // 最后按成品的字节数再卡一次，上限是硬的
    let limit = opts.limits.max_output_bytes;
    let over = out.markdown.len() > limit;
    budget::truncate(&mut out.markdown, limit);
    if guard.truncated() || over {
        out.warnings.push(format!(
            "output_truncated: the document holds more than {} MB of text; conversion stopped there and the rest was left out",
            opts.limits.max_output_bytes >> 20
        ));
    }
    Ok(out)
}

/// 转换一份文档。
pub fn convert(bytes: &[u8], format: Format, opts: &Options<'_>) -> Result<Converted, Error> {
    budgeted(opts, || match format {
        Format::Csv => csv::convert(bytes, opts),
        Format::Text
        | Format::Markdown
        | Format::Fenced(_)
        | Format::Subtitle
        | Format::Notebook => text_format(&String::from_utf8_lossy(bytes), format, opts),
        Format::Outline => opml::convert(bytes, opts),
        Format::Fb2 => fb2::convert(bytes, opts),
        Format::Docbook | Format::Jats => xmldoc::convert_article(bytes, opts),
        Format::Svg => xmldoc::convert_svg(bytes, opts),
        Format::WordPerfect => wordperfect::convert(bytes, opts),
        Format::Rtf => rtf::convert(bytes, opts),
        Format::Eml => email::convert_eml(bytes, opts),
        Format::Mbox => email::convert_mbox(bytes, opts),
        Format::Msg => email::convert_msg(bytes, opts),
        Format::Archive => archive::convert(bytes, opts),
        Format::Doc => legacy::doc::convert(bytes, opts),
        Format::Xls => legacy::xls::convert(bytes, opts),
        Format::Ppt => legacy::ppt::convert(bytes, opts),
        Format::Hwp => legacy::hwp::convert(bytes, opts),
        Format::FlatOdf => odf::convert_flat(bytes, opts),
        Format::Dbf => dbf::convert(bytes, opts),
        Format::Sqlite => sqlite::convert(bytes, opts),
        Format::Mobi => mobi::convert(bytes, opts),
        _ => {
            if is_ole_encrypted(bytes) {
                return Err(Error::Encrypted);
            }
            let zip = zip::Zip::open(bytes, &opts.limits)?;
            match format {
                Format::Docx => docx::convert(&zip, opts),
                Format::Xlsx => xlsx::convert(&zip, opts),
                Format::Pptx => pptx::convert(&zip, opts),
                Format::Epub => epub::convert(&zip, opts),
                Format::Odt | Format::Ods | Format::Odp => odf::convert(&zip, format, opts),
                Format::Xlsb => legacy::xlsb::convert(&zip, opts),
                Format::Hwpx => hwpx::convert(&zip, opts),
                _ => unreachable!("handled above"),
            }
        }
    })
}

/// 加了口令的 OOXML 不是 ZIP，而是 OLE 复合文件（MS-OFFCRYPTO），开头是这个签名。
fn is_ole_encrypted(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
}

/// 每处理一块检查一次截止时间。
pub(crate) fn check_deadline(opts: &Options<'_>) -> Result<(), Error> {
    match opts.deadline {
        Some(d) if Instant::now() >= d => Err(Error::Timeout),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_content_then_name_then_type() {
        let docx = zip::tests::build(&[("word/document.xml", b"<w:document/>", false)]);
        assert_eq!(
            Format::detect(&docx, Some("x.xlsx"), None),
            Some(Format::Docx)
        );
        let other = zip::tests::build(&[("x.txt", b"x", false)]);
        assert_eq!(
            Format::detect(&other, Some("a.PPTX"), None),
            Some(Format::Pptx)
        );
        assert_eq!(
            Format::detect(&other, Some("a.zip"), None),
            Some(Format::Archive)
        );
        assert_eq!(
            Format::detect(b"a,b\n1,2", Some("data.csv"), None),
            Some(Format::Csv)
        );
        assert_eq!(
            Format::detect(b"a,b", None, Some("text/csv; charset=utf-8")),
            Some(Format::Csv)
        );
        assert_eq!(Format::detect(b"a,b", Some("a.docx"), None), None);
        assert_eq!(Format::detect(b"<html>", Some("a.html"), None), None);
        let ole = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0];
        assert_eq!(
            Format::detect(&ole, Some("locked.xlsx"), None),
            Some(Format::Xlsx)
        );
        assert_eq!(
            Format::detect(&ole, Some("old.doc"), None),
            Some(Format::Doc)
        );
        assert_eq!(Format::detect(&ole, Some("old.xyz"), None), None);
    }

    #[test]
    fn password_protected_office_files_are_reported_as_encrypted() {
        let mut ole = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
        ole.resize(512, 0);
        assert_eq!(
            convert(&ole, Format::Docx, &Options::default()),
            Err(Error::Encrypted)
        );
    }
}
