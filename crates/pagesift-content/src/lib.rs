//! pagesift 的内容处理核心：把 HTML 变成干净的 Markdown 和元数据（由 [`yolowhtml`] 驱动）。
//!
//! ```
//! let html = "<html><body><nav>Home</nav><article><h1>Hi</h1><p>Hello, world.</p></article></body></html>";
//! let doc = pagesift_content::process(html, &pagesift_content::Options::default());
//! assert_eq!(doc.markdown, "# Hi\n\nHello, world.\n");
//! ```

#![forbid(unsafe_code)]

pub use yolowhtml::*;
