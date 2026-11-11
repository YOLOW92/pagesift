//! PDF 转干净的 Markdown（直接重导出 [`yolowpdf`]）。
//!
//! ```no_run
//! let bytes = std::fs::read("paper.pdf").unwrap();
//! let doc = pagesift_pdf::extract(&bytes).unwrap();
//! println!("{}", doc.markdown);
//! ```

#![forbid(unsafe_code)]

pub use yolowpdf::*;
