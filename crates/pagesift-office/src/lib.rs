#![forbid(unsafe_code)]

//! Thin workspace facade re-exporting [`yolowdoc`] with pagesift's HTTP fetchability policy
//! and unwind-safe media inspection wrappers.

use std::panic::{AssertUnwindSafe, catch_unwind};

pub use yolowdoc::{
    Converted, Error, Format, HtmlToMarkdown, Limits, Media, MediaLocation, MemberToMarkdown,
    Options, convert, convert_csv_text, convert_text,
};

/// 列出文档内嵌媒体部件与引用位置，与 [`convert`] 对齐 `catch_unwind` 边界保护。
pub fn media(bytes: &[u8], format: Format, opts: &Options<'_>) -> Result<Vec<Media>, Error> {
    catch_unwind(AssertUnwindSafe(|| yolowdoc::media(bytes, format, opts)))
        .unwrap_or_else(|_| Err(Error::Malformed("media reader panicked".into())))
}

/// 取出单个内嵌媒体部件原始字节，与 [`convert`] 对齐 `catch_unwind` 边界保护。
pub fn media_bytes(bytes: &[u8], part: &str, opts: &Options<'_>) -> Result<Option<Vec<u8>>, Error> {
    catch_unwind(AssertUnwindSafe(|| {
        yolowdoc::media_bytes(bytes, part, opts)
    }))
    .unwrap_or_else(|_| Err(Error::Malformed("media reader panicked".into())))
}

/// HTTP 抓取路径的文档格式策略扩展；与本地显式解析区分，保留在 `pagesift-office` 兼容层。
pub trait FormatExt {
    /// Whether this format should be parsed when encountered over HTTP.
    fn fetchable(self) -> bool;
}

impl FormatExt for Format {
    fn fetchable(self) -> bool {
        is_fetchable(self)
    }
}

/// Whether `format` should be parsed on HTTP crawl / fetch paths.
pub fn is_fetchable(format: Format) -> bool {
    matches!(
        format,
        Format::Docx
            | Format::Xlsx
            | Format::Pptx
            | Format::Epub
            | Format::Csv
            | Format::Rtf
            | Format::Odt
            | Format::Ods
            | Format::Odp
            | Format::Eml
            | Format::Mbox
            | Format::Msg
            | Format::Archive
            | Format::Doc
            | Format::Xls
            | Format::Ppt
            | Format::Xlsb
            | Format::Hwp
            | Format::Hwpx
            | Format::Sqlite
            | Format::Mobi
    )
}
