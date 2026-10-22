//! 网页品类形态毫秒级轻量分类器（对标 WCXB 7 大类别）。

mod features;
mod page_type;

pub use features::classify;
pub use page_type::PageType;
