#![forbid(unsafe_code)]

//! Thin workspace facade re-exporting [`yolowdoc`] with pagesift's HTTP fetchability policy.

pub use yolowdoc::*;

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
