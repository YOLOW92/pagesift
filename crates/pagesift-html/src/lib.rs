//! 面向正文提取的 HTML 解析器（由 [`yolowhtml_dom`] 驱动）。
//!
//! ```
//! let doc = pagesift_html::Html::parse("<p>Fish &amp; chips<p>Peas");
//! let paragraphs: Vec<String> = doc
//!     .root()
//!     .descendants()
//!     .filter(|n| n.tag() == Some(pagesift_html::Tag::P))
//!     .map(|n| n.text())
//!     .collect();
//! assert_eq!(paragraphs, ["Fish & chips", "Peas"]);
//! ```

#![forbid(unsafe_code)]

pub use yolowhtml_dom::*;
