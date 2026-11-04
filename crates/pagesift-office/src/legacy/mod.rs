//! 旧版二进制 Office（OLE 复合文件）：Word、Excel、PowerPoint 97–2003 和 HWP 5。

pub(crate) mod doc;
mod doc_lists;
mod doc_styles;
pub(crate) mod hwp;
pub(crate) mod ppt;
mod props;
pub(crate) mod xls;
pub(crate) mod xlsb;

use crate::Format;
use crate::ole::Ole;

/// 按复合文件里的流名认格式。
pub(crate) fn sniff(bytes: &[u8]) -> Option<Format> {
    let ole = Ole::open(bytes).ok()?;
    let root = ole.children(None);
    let has = |name: &str| root.iter().any(|e| e.name.eq_ignore_ascii_case(name));
    if has("WordDocument") {
        Some(Format::Doc)
    } else if has("Workbook") || has("Book") {
        Some(Format::Xls)
    } else if has("PowerPoint Document") {
        Some(Format::Ppt)
    } else if has("FileHeader") && has("BodyText") {
        Some(Format::Hwp)
    } else if root.iter().any(|e| e.name.starts_with("__substg1.0_")) {
        Some(Format::Msg)
    } else {
        None
    }
}
