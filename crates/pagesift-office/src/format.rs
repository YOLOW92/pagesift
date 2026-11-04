//! 支持的格式和格式识别。内容优先，扩展名和 Content-Type 只在内容说明不了时才用。

use crate::{Limits, is_ole_encrypted, opc, tar, zip};

/// 支持的格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Docx,
    Xlsx,
    Pptx,
    Epub,
    Csv,
    /// 纯文本和轻量标记（txt、log、rst、org、adoc、tex、bib、ris）：原样输出。
    Text,
    Markdown,
    /// 结构化文本（YAML、TOML、INI、XML、JSON、JSONL）：放进代码块，参数是语言标记。
    Fenced(&'static str),
    /// 字幕（SRT、WebVTT）。
    Subtitle,
    /// Jupyter 笔记本。
    Notebook,
    /// OPML 大纲。
    Outline,
    Rtf,
    Odt,
    Ods,
    Odp,
    Fb2,
    Eml,
    Mbox,
    Msg,
    /// 压缩包（ZIP、tar、7z，以及 gz、bz2、xz）。
    Archive,
    /// DocBook 4/5。
    Docbook,
    /// JATS 期刊文章。
    Jats,
    /// SVG（只取文字）。
    Svg,
    /// WordPerfect。
    WordPerfect,
    /// Word 97–2003（OLE）。
    Doc,
    /// Excel 97–2003（BIFF8）。
    Xls,
    /// PowerPoint 97–2003。
    Ppt,
    /// Excel 二进制工作簿。
    Xlsb,
    /// 韩文 HWP 5。
    Hwp,
    /// 韩文 HWPX。
    Hwpx,
    /// 单文件 XML 的 OpenDocument（fodt、fods、fodp、fodg）。
    FlatOdf,
    /// dBASE / FoxPro。
    Dbf,
    /// SQLite 3（含 GeoPackage）。
    Sqlite,
    /// Kindle / Palm 电子书。
    Mobi,
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
            Format::Text => "text/plain",
            Format::Markdown => "text/markdown",
            Format::Fenced("yaml") => "application/yaml",
            Format::Fenced("toml") => "application/toml",
            Format::Fenced("xml") => "application/xml",
            Format::Fenced("json") => "application/json",
            Format::Fenced("jsonl") => "application/x-ndjson",
            Format::Fenced(_) => "text/plain",
            Format::Subtitle => "text/vtt",
            Format::Notebook => "application/x-ipynb+json",
            Format::Outline => "text/x-opml",
            Format::Rtf => "application/rtf",
            Format::Odt => "application/vnd.oasis.opendocument.text",
            Format::Ods => "application/vnd.oasis.opendocument.spreadsheet",
            Format::Odp => "application/vnd.oasis.opendocument.presentation",
            Format::Fb2 => "application/x-fictionbook+xml",
            Format::Eml => "message/rfc822",
            Format::Mbox => "application/mbox",
            Format::Msg => "application/vnd.ms-outlook",
            Format::Archive => "application/zip",
            Format::Docbook => "application/docbook+xml",
            Format::Jats => "application/jats+xml",
            Format::Svg => "image/svg+xml",
            Format::WordPerfect => "application/vnd.wordperfect",
            Format::Doc => "application/msword",
            Format::Xls => "application/vnd.ms-excel",
            Format::Ppt => "application/vnd.ms-powerpoint",
            Format::Xlsb => "application/vnd.ms-excel.sheet.binary.macroenabled.12",
            Format::Hwp => "application/x-hwp",
            Format::Hwpx => "application/hwp+zip",
            Format::FlatOdf => "application/vnd.oasis.opendocument.text-flat-xml",
            Format::Dbf => "application/x-dbf",
            Format::Sqlite => "application/vnd.sqlite3",
            Format::Mobi => "application/x-mobipocket-ebook",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Docx => "docx",
            Format::Xlsx => "xlsx",
            Format::Pptx => "pptx",
            Format::Epub => "epub",
            Format::Csv => "csv",
            Format::Text => "txt",
            Format::Markdown => "md",
            Format::Fenced(lang) => lang,
            Format::Subtitle => "vtt",
            Format::Notebook => "ipynb",
            Format::Outline => "opml",
            Format::Rtf => "rtf",
            Format::Odt => "odt",
            Format::Ods => "ods",
            Format::Odp => "odp",
            Format::Fb2 => "fb2",
            Format::Eml => "eml",
            Format::Mbox => "mbox",
            Format::Msg => "msg",
            Format::Archive => "zip",
            Format::Docbook => "dbk",
            Format::Jats => "jats",
            Format::Svg => "svg",
            Format::WordPerfect => "wpd",
            Format::Doc => "doc",
            Format::Xls => "xls",
            Format::Ppt => "ppt",
            Format::Xlsb => "xlsb",
            Format::Hwp => "hwp",
            Format::Hwpx => "hwpx",
            Format::FlatOdf => "fodt",
            Format::Dbf => "dbf",
            Format::Sqlite => "sqlite",
            Format::Mobi => "mobi",
        }
    }

    /// 按扩展名（不分大小写，可带点）。带宏的 docm/xlsm/pptm 结构相同，宏本身不执行也不输出。
    pub fn from_extension(ext: &str) -> Option<Format> {
        match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "docx" | "docm" | "dotx" | "dotm" => Some(Format::Docx),
            "xlsx" | "xlsm" | "xltx" | "xltm" | "xlam" => Some(Format::Xlsx),
            "pptx" | "pptm" | "ppsx" | "ppsm" | "potx" | "potm" => Some(Format::Pptx),
            "epub" => Some(Format::Epub),
            "csv" | "tsv" => Some(Format::Csv),
            "txt" | "text" | "log" | "rst" | "org" | "adoc" | "asciidoc" | "tex" | "latex"
            | "bib" | "ris" | "nbib" | "enw" | "typ" | "typst" | "dj" | "djot" | "nfo"
            | "textile" | "dot" | "gv" | "puml" | "mmd" => Some(Format::Text),
            "md" | "markdown" | "commonmark" | "mdx" | "qmd" | "rmd" => Some(Format::Markdown),
            "yaml" | "yml" => Some(Format::Fenced("yaml")),
            "toml" => Some(Format::Fenced("toml")),
            "ini" | "cfg" | "conf" => Some(Format::Fenced("ini")),
            "xml" | "kml" => Some(Format::Fenced("xml")),
            "json" | "geojson" => Some(Format::Fenced("json")),
            "jsonl" | "ndjson" => Some(Format::Fenced("jsonl")),
            "srt" | "vtt" => Some(Format::Subtitle),
            "ipynb" => Some(Format::Notebook),
            "opml" => Some(Format::Outline),
            "rtf" => Some(Format::Rtf),
            "odt" | "ott" => Some(Format::Odt),
            "ods" | "ots" => Some(Format::Ods),
            "odp" | "otp" => Some(Format::Odp),
            "fb2" => Some(Format::Fb2),
            "dbk" | "docbook" | "docbook4" | "docbook5" => Some(Format::Docbook),
            "jats" | "nxml" => Some(Format::Jats),
            "svg" => Some(Format::Svg),
            "wp" | "wpd" | "wp5" | "wp6" => Some(Format::WordPerfect),
            "eml" => Some(Format::Eml),
            "mbox" => Some(Format::Mbox),
            "msg" => Some(Format::Msg),
            "zip" | "tar" | "tgz" | "gz" | "bz2" | "tbz2" | "xz" | "txz" | "7z" => {
                Some(Format::Archive)
            }
            "doc" | "wbk" => Some(Format::Doc),
            "xls" | "xlt" | "xla" => Some(Format::Xls),
            "ppt" | "pps" | "pot" => Some(Format::Ppt),
            "xlsb" => Some(Format::Xlsb),
            "hwp" => Some(Format::Hwp),
            "hwpx" => Some(Format::Hwpx),
            "fodt" | "fods" | "fodp" | "fodg" => Some(Format::FlatOdf),
            "dbf" => Some(Format::Dbf),
            "sqlite" | "sqlite3" | "db" | "db3" | "gpkg" | "gpkx" => Some(Format::Sqlite),
            "mobi" | "azw" | "azw3" | "prc" => Some(Format::Mobi),
            other => crate::code::language(other).map(Format::Fenced),
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
            "text/plain" => Some(Format::Text),
            "text/markdown" | "text/x-markdown" => Some(Format::Markdown),
            "application/yaml" | "application/x-yaml" | "text/yaml" | "text/x-yaml" => {
                Some(Format::Fenced("yaml"))
            }
            "application/toml" => Some(Format::Fenced("toml")),
            "application/xml" | "text/xml" => Some(Format::Fenced("xml")),
            "application/json" | "text/json" => Some(Format::Fenced("json")),
            "application/x-ndjson" | "application/jsonl" => Some(Format::Fenced("jsonl")),
            "text/vtt" | "application/x-subrip" => Some(Format::Subtitle),
            "application/x-ipynb+json" => Some(Format::Notebook),
            "text/x-opml" => Some(Format::Outline),
            "application/rtf" | "text/rtf" => Some(Format::Rtf),
            "application/vnd.oasis.opendocument.text" => Some(Format::Odt),
            "application/vnd.oasis.opendocument.spreadsheet" => Some(Format::Ods),
            "application/vnd.oasis.opendocument.presentation" => Some(Format::Odp),
            "application/x-fictionbook+xml" => Some(Format::Fb2),
            "application/docbook+xml" => Some(Format::Docbook),
            "application/jats+xml" => Some(Format::Jats),
            "image/svg+xml" => Some(Format::Svg),
            "application/vnd.wordperfect" | "application/wordperfect" => Some(Format::WordPerfect),
            "message/rfc822" => Some(Format::Eml),
            "application/mbox" => Some(Format::Mbox),
            "application/vnd.ms-outlook" => Some(Format::Msg),
            "application/msword" => Some(Format::Doc),
            "application/vnd.ms-excel" => Some(Format::Xls),
            "application/vnd.ms-powerpoint" => Some(Format::Ppt),
            "application/vnd.ms-excel.sheet.binary.macroenabled.12" => Some(Format::Xlsb),
            "application/x-hwp" | "application/haansofthwp" | "application/vnd.hancom.hwp" => {
                Some(Format::Hwp)
            }
            "application/hwp+zip" | "application/vnd.hancom.hwpx" => Some(Format::Hwpx),
            "application/vnd.oasis.opendocument.text-flat-xml"
            | "application/vnd.oasis.opendocument.spreadsheet-flat-xml"
            | "application/vnd.oasis.opendocument.presentation-flat-xml"
            | "application/vnd.oasis.opendocument.graphics-flat-xml" => Some(Format::FlatOdf),
            "application/x-dbf"
            | "application/dbf"
            | "application/vnd.dbf"
            | "application/x-dbase" => Some(Format::Dbf),
            "application/vnd.sqlite3"
            | "application/x-sqlite3"
            | "application/geopackage+sqlite3" => Some(Format::Sqlite),
            "application/x-mobipocket-ebook" | "application/vnd.amazon.ebook" => Some(Format::Mobi),
            "application/zip"
            | "application/x-zip-compressed"
            | "application/x-tar"
            | "application/gzip"
            | "application/x-gzip"
            | "application/x-bzip2"
            | "application/x-xz"
            | "application/x-7z-compressed" => Some(Format::Archive),
            _ => None,
        }
    }

    /// 识别格式：ZIP 容器看里面的部件（内容优先，文件名和类型可能写错），RTF 看签名，
    /// 文本类没有签名，只能靠扩展名或 Content-Type。认不出返回 `None`。
    pub fn detect(
        bytes: &[u8],
        filename: Option<&str>,
        content_type: Option<&str>,
    ) -> Option<Format> {
        let raw_ext = filename.and_then(|n| n.rsplit_once('.')).map(|(_, e)| e);
        // 特定文件名优先：requirements.txt、CMakeLists.txt 虽然有扩展名，但语言是确定的
        let ext = filename
            .and_then(crate::code::language_of_name)
            .map(Format::Fenced)
            .or_else(|| raw_ext.and_then(Format::from_extension));
        let mime = content_type.and_then(Format::from_mime);
        if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
            if let Some(f) = sniff_zip(bytes) {
                return Some(f);
            }
            // 认不出部件的 ZIP：扩展名说是 Office 就按它报错，其余都是普通压缩包
            return Some(
                ext.or(mime)
                    .filter(|f| f.is_zip_based())
                    .unwrap_or(Format::Archive),
            );
        }
        if bytes.starts_with(b"{\\rtf") {
            return Some(Format::Rtf);
        }
        if bytes.starts_with(crate::sqlite::MAGIC) {
            return Some(Format::Sqlite);
        }
        if crate::mobi::is_pdb_book(bytes) {
            return Some(Format::Mobi);
        }
        if is_ole_encrypted(bytes)
            && let Some(f) = crate::legacy::sniff(bytes)
        {
            return Some(f);
        }
        if crate::wordperfect::is_wpc(bytes) {
            return Some(Format::WordPerfect);
        }
        if crate::archive::has_signature(bytes) || tar::looks_like_tar(bytes) {
            return Some(Format::Archive);
        }
        let hinted = match (mime, ext) {
            // text/plain、application/xml 这类类型太笼统：扩展名说得更清楚。
            // 扩展名是别的东西（比如 a.html 以 text/plain 上传）时不抢，交给调用方判断。
            (Some(m), Some(e)) if m.is_generic_text() => e,
            (Some(m), None) if m.is_generic_text() && raw_ext.is_some() => return None,
            (m, e) => m.or(e)?,
        };
        // 加了口令的 docx/xlsx/pptx 是 OLE 复合文件：照样认成该格式，转换时报“已加密”
        if is_ole_encrypted(bytes) {
            return matches!(
                hinted,
                Format::Docx
                    | Format::Xlsx
                    | Format::Pptx
                    | Format::Msg
                    | Format::Doc
                    | Format::Xls
                    | Format::Ppt
                    | Format::Hwp
                    | Format::Xlsb
            )
            .then_some(hinted);
        }
        match hinted {
            Format::Csv => Some(Format::Csv),
            // 老式 WordPerfect 没有文件头，只能信扩展名
            Format::WordPerfect => Some(Format::WordPerfect),
            // dBASE 也没有签名，转换时校验文件头
            Format::Dbf => Some(Format::Dbf),
            Format::Fenced("xml") if looks_textual(bytes) => Some(sniff_xml(bytes)),
            f if f.is_text_based() && looks_textual(bytes) => Some(f),
            _ => None,
        }
    }

    /// 文本内容要先解码成 UTF-8：调用方知道字符集时走 [`crate::convert_text`]。
    pub fn decodes_text(self) -> bool {
        matches!(
            self,
            Format::Csv
                | Format::Text
                | Format::Markdown
                | Format::Fenced(_)
                | Format::Subtitle
                | Format::Notebook
        )
    }

    /// 抓取网页时也按这个格式处理吗？文本家族只用于上传和本地文件：网上的 text/plain、XML、JSON
    /// 仍然走原来的网页路径，行为不变。
    pub fn fetchable(self) -> bool {
        matches!(
            self,
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

    /// 没有签名、内容就是文字的格式。服务器明说是 HTML 时不能信扩展名。
    pub fn is_text_based(self) -> bool {
        matches!(
            self,
            Format::Csv
                | Format::Text
                | Format::Markdown
                | Format::Fenced(_)
                | Format::Subtitle
                | Format::Notebook
                | Format::Outline
                | Format::Fb2
                | Format::Docbook
                | Format::Jats
                | Format::Svg
                | Format::FlatOdf
                | Format::Eml
                | Format::Mbox
        )
    }

    fn is_zip_based(self) -> bool {
        matches!(
            self,
            Format::Docx
                | Format::Xlsx
                | Format::Pptx
                | Format::Epub
                | Format::Odt
                | Format::Ods
                | Format::Odp
                | Format::Xlsb
                | Format::Hwpx
        )
    }

    fn is_generic_text(self) -> bool {
        matches!(self, Format::Text | Format::Fenced(_))
    }
}

/// 开头没有 NUL 就算文字（UTF-16 带 BOM 的除外）。二进制文件改名成 .txt 时不当文字输出。
fn looks_textual(bytes: &[u8]) -> bool {
    const SAMPLE: usize = 8 << 10;
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return true;
    }
    !bytes[..bytes.len().min(SAMPLE)].contains(&0)
}

/// `.xml` 按根元素认：认得出的是文档，其他的放进代码块。
fn sniff_xml(bytes: &[u8]) -> Format {
    let head = &bytes[..bytes.len().min(16 << 10)];
    let has = |needle: &[u8]| head.windows(needle.len()).any(|w| w == needle);
    match crate::xmldoc::root_name(bytes) {
        Some("opml") => Format::Outline,
        Some("FictionBook") => Format::Fb2,
        Some("svg") => Format::Svg,
        Some("document") if has(b"opendocument") => Format::FlatOdf,
        Some("book" | "chapter" | "article" | "section") if has(b"docbook") => Format::Docbook,
        Some("article") if has(b"JATS") || has(b"<front") => Format::Jats,
        _ => Format::Fenced("xml"),
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
    if zip.contains("xl/workbook.bin") {
        return Some(Format::Xlsb);
    }
    if zip.contains("Contents/section0.xml") && zip.contains("Contents/content.hpf") {
        return Some(Format::Hwpx);
    }
    if zip.contains("ppt/presentation.xml") {
        return Some(Format::Pptx);
    }
    if zip.contains("META-INF/container.xml") {
        return Some(Format::Epub);
    }
    // ODF 第一个条目 mimetype 写明类型（ISO 26300 §3）
    if let Ok(Some(mimetype)) = zip.read_text("mimetype") {
        match mimetype.trim() {
            m if m.starts_with("application/vnd.oasis.opendocument.text") => {
                return Some(Format::Odt);
            }
            m if m.starts_with("application/vnd.oasis.opendocument.spreadsheet") => {
                return Some(Format::Ods);
            }
            m if m.starts_with("application/vnd.oasis.opendocument.presentation") => {
                return Some(Format::Odp);
            }
            "application/hwp+zip" => return Some(Format::Hwpx),
            _ => {}
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_family_needs_a_name_or_type_and_textual_bytes() {
        let d = Format::detect;
        assert_eq!(d(b"hi", Some("a.txt"), None), Some(Format::Text));
        assert_eq!(d(b"# hi", Some("a.MD"), None), Some(Format::Markdown));
        assert_eq!(
            d(b"a: 1", Some("a.yml"), None),
            Some(Format::Fenced("yaml"))
        );
        assert_eq!(d(b"hi", None, Some("text/plain")), Some(Format::Text));
        assert_eq!(d(b"hi", None, None), None);
        // 二进制改名成 .txt 不当文字
        assert_eq!(d(b"\x89PNG\0\0", Some("a.txt"), None), None);
        // 带 BOM 的 UTF-16 里有 NUL，照样是文字
        assert_eq!(
            d(&[0xFF, 0xFE, b'h', 0], Some("a.txt"), None),
            Some(Format::Text)
        );
    }

    #[test]
    fn extension_beats_a_generic_type_but_never_steals_other_files() {
        let d = Format::detect;
        assert_eq!(
            d(b"# x", Some("a.md"), Some("text/plain")),
            Some(Format::Markdown)
        );
        assert_eq!(
            d(b"a,b", Some("a.csv"), Some("text/plain")),
            Some(Format::Csv)
        );
        // a.html 以 text/plain 上传：不抢，由调用方按 HTML 处理
        assert_eq!(d(b"<p>", Some("a.html"), Some("text/plain")), None);
        assert_eq!(d(b"{}", Some("a.bin"), Some("application/json")), None);
    }

    #[test]
    fn signatures_and_containers_win() {
        let d = Format::detect;
        assert_eq!(d(b"{\\rtf1 x}", Some("a.txt"), None), Some(Format::Rtf));
        assert_eq!(d(b"plain", Some("a.rtf"), None), None);
        let odt = zip::tests::build(&[(
            "mimetype",
            b"application/vnd.oasis.opendocument.text",
            false,
        )]);
        assert_eq!(d(&odt, Some("a.zip"), None), Some(Format::Odt));
        let ole = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0];
        assert_eq!(d(&ole, Some("a.msg"), None), Some(Format::Msg));
        assert_eq!(d(b"text", Some("a.msg"), None), None);
    }

    #[test]
    fn xml_documents_are_recognized_by_their_root() {
        let d = |b: &[u8]| Format::detect(b, Some("a.xml"), None);
        assert_eq!(d(b"<opml/>"), Some(Format::Outline));
        assert_eq!(d(b"<?xml?><svg/>"), Some(Format::Svg));
        assert_eq!(
            d(b"<article xmlns='http://docbook.org/ns/docbook'/>"),
            Some(Format::Docbook)
        );
        assert_eq!(d(b"<article><front/></article>"), Some(Format::Jats));
        assert_eq!(d(b"<config/>"), Some(Format::Fenced("xml")));
    }

    #[test]
    fn web_paths_only_take_formats_with_clear_content() {
        assert!(Format::Rtf.fetchable());
        assert!(Format::Eml.fetchable());
        assert!(!Format::Text.fetchable());
        assert!(!Format::Fenced("json").fetchable());
        assert!(!Format::Markdown.fetchable());
    }
}
