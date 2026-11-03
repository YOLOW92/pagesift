//! 标准输出：每页一行 JSON。

use std::io::Write;

use pagesift_api::types::Document;

use super::{PageOut, page_url};

pub struct LinesOut;

impl PageOut for LinesOut {
    fn page(&self, doc: &Document) -> std::io::Result<()> {
        let mut line = serde_json::json!({
            "url": page_url(doc),
            "title": doc.metadata.title,
            "markdown": doc.markdown,
        });
        if let Some(w) = &doc.warning {
            line["warning"] = serde_json::json!(w);
        }
        writeln!(std::io::stdout().lock(), "{line}")
    }
}
