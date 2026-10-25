//! 面向正文提取的 HTML 解析器。
//!
//! - 零拷贝：文本和属性值直接引用源字符串，只有含字符引用的部分才解码到一块共享缓冲区；
//! - 紧凑的树：节点放在一个 `Vec` 里，用 u32 下标互相引用，标签是枚举；
//! - HTML5 树构建规则里影响文本结构的部分都实现了（隐式标签、自动闭合、作用域、表格、
//!   外部内容、raw text），见 `builder` 模块的说明；
//! - CSS 选择器（[`Selector`]）和 HTML 序列化（[`NodeRef::html`]）。
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

mod builder;
mod dom;
mod entities;
mod entity;
mod select;
mod serialize;
mod tag;

pub use dom::{Ancestors, Children, Descendants, Element, Html, NodeId, NodeRef, ParseScratch};
pub use select::{Select, Selector, SelectorError};
pub use tag::Tag;
