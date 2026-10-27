//! Office 文档转 Markdown：Word（.docx）、Excel（.xlsx）、PowerPoint（.pptx）、EPUB 和 CSV。
//!
//! 照 ECMA-376（OOXML）、EPUB 3（OCF/OPF）、RFC 4180 自己实现，ZIP 目录和 XML 解析也是自己写的
//! （见 [`zip`]、[`xml`]），只有 deflate 解压用 miniz_oxide。所有输入都按不可信处理：
//! 解压量、单元格数、嵌套层数都有上限（[`Limits`]），XML 不展开任何声明的实体。
//!
//! EPUB 的正文是 XHTML，转换交给调用方（[`Options::html`]），这样和网页走同一个 HTML 提取器。

#![forbid(unsafe_code)]

use std::fmt;
use std::time::Instant;

mod csv;
mod docx;
mod epub;
mod md;
mod opc;
mod pptx;
mod xlsx;
mod xml;
mod zip;

/// 支持的格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Docx,
    Xlsx,
    Pptx,
    Epub,
    Csv,
}

impl Format {
    pub fn mime(self) -> &'static str {
        match self {
            Format::Docx => {
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            }
            Format::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Format::Pptx => {
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            }
            Format::Epub => "application/epub+zip",
            Format::Csv => "text/csv",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Docx => "docx",
            Format::Xlsx => "xlsx",
            Format::Pptx => "pptx",
            Format::Epub => "epub",
            Format::Csv => "csv",
        }
    }

    /// 按扩展名（不分大小写，可带点）。带宏的 docm/xlsm/pptm 结构相同，宏本身不执行也不输出。
    pub fn from_extension(ext: &str) -> Option<Format> {
        match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "docx" | "docm" | "dotx" | "dotm" => Some(Format::Docx),
            "xlsx" | "xlsm" | "xltx" | "xltm" => Some(Format::Xlsx),
            "pptx" | "pptm" | "ppsx" | "ppsm" | "potx" | "potm" => Some(Format::Pptx),
            "epub" => Some(Format::Epub),
            "csv" | "tsv" => Some(Format::Csv),
            _ => None,
        }
    }

    /// 按 Content-Type（忽略参数）。
    pub fn from_mime(mime: &str) -> Option<Format> {
        let m = mime
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match m.as_str() {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            | "application/vnd.ms-word.document.macroenabled.12"
            | "application/vnd.openxmlformats-officedocument.wordprocessingml.template" => {
                Some(Format::Docx)
            }
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
            | "application/vnd.ms-excel.sheet.macroenabled.12"
            | "application/vnd.openxmlformats-officedocument.spreadsheetml.template" => {
                Some(Format::Xlsx)
            }
            "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            | "application/vnd.ms-powerpoint.presentation.macroenabled.12"
            | "application/vnd.openxmlformats-officedocument.presentationml.slideshow" => {
                Some(Format::Pptx)
            }
            "application/epub+zip" => Some(Format::Epub),
            "text/csv" | "application/csv" | "text/tab-separated-values" => Some(Format::Csv),
            _ => None,
        }
    }

    /// 识别格式：ZIP 容器看里面的部件（内容优先，文件名和类型可能写错），
    /// CSV 没有签名，只能靠扩展名或 Content-Type。认不出返回 `None`。
    pub fn detect(
        bytes: &[u8],
        filename: Option<&str>,
        content_type: Option<&str>,
    ) -> Option<Format> {
        let ext = filename
            .and_then(|n| n.rsplit_once('.'))
            .and_then(|(_, e)| Format::from_extension(e));
        let mime = content_type.and_then(Format::from_mime);
        if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
            if let Some(f) = sniff_zip(bytes) {
                return Some(f);
            }
            return ext.or(mime).filter(|f| *f != Format::Csv);
        }
        let hinted = mime.or(ext)?;
        // 加了口令的 docx/xlsx/pptx 是 OLE 复合文件：照样认成该格式，转换时报“已加密”
        if is_ole_encrypted(bytes) && matches!(hinted, Format::Docx | Format::Xlsx | Format::Pptx) {
            return Some(hinted);
        }
        (hinted == Format::Csv).then_some(Format::Csv)
    }
}

fn sniff_zip(bytes: &[u8]) -> Option<Format> {
    let zip = zip::Zip::open(bytes, &Limits::default()).ok()?;
    if zip.contains("word/document.xml") {
        return Some(Format::Docx);
    }
    if zip.contains("xl/workbook.xml") {
        return Some(Format::Xlsx);
    }
    if zip.contains("ppt/presentation.xml") {
        return Some(Format::Pptx);
    }
    if zip.contains("META-INF/container.xml") {
        return Some(Format::Epub);
    }
    // 主部件不在默认位置时按 _rels/.rels 指向的部件判断
    let rels = zip.read_text("_rels/.rels").ok()??;
    let target = opc::Rels::parse(&rels, "").ok()?.office_document()?;
    let t = target.to_ascii_lowercase();
    [
        ("word/", Format::Docx),
        ("xl/", Format::Xlsx),
        ("ppt/", Format::Pptx),
    ]
    .into_iter()
    .find(|(p, _)| t.contains(p))
    .map(|(_, f)| f)
}

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
        }
    }
}

/// EPUB 正文（XHTML）转 Markdown 的函数：参数是 XHTML 和它在包里的路径。
pub type HtmlToMarkdown<'a> = &'a (dyn Fn(&str, &str) -> String + Sync);

#[derive(Clone, Default)]
pub struct Options<'a> {
    pub limits: Limits,
    /// 超过这个时间点就停下报 [`Error::Timeout`]（按块检查：段落、行、幻灯片、章节）。
    pub deadline: Option<Instant>,
    /// EPUB 必须给；没给时 EPUB 报 [`Error::Unsupported`]。
    pub html: Option<HtmlToMarkdown<'a>>,
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
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        csv::convert_text(text, opts)
    }))
    .unwrap_or_else(|_| Err(Error::Malformed("converter panicked".into())))
}

/// 转换一份文档。
pub fn convert(bytes: &[u8], format: Format, opts: &Options<'_>) -> Result<Converted, Error> {
    // 依赖代码在畸形输入上万一 panic，也转成错误返回
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match format {
        Format::Csv => csv::convert(bytes, opts),
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
                Format::Csv => unreachable!(),
            }
        }
    }))
    .unwrap_or_else(|_| Err(Error::Malformed("converter panicked".into())))
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
        assert_eq!(Format::detect(&other, Some("a.zip"), None), None);
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
        assert_eq!(Format::detect(&ole, Some("old.doc"), None), None);
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
