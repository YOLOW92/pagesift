//! OpenGraph、Twitter Cards 与 DublinCore 元标签提取器。

use pagesift_html::{Html, Tag};
use serde_json::{Map, Value};

/// 提取 HTML 中所有 OpenGraph 与 Twitter 元标签
pub fn extract_opengraph(html: &Html<'_>) -> Map<String, Value> {
    let mut map = Map::new();

    for node in html.root().descendants() {
        if node.tag() == Some(Tag::Meta) {
            let Some(el) = node.as_element() else {
                continue;
            };
            let key = el
                .attr("property")
                .or_else(|| el.attr("name"))
                .map(|k| k.trim());

            let content = el.attr("content").map(|c| c.trim());

            if let (Some(k), Some(v)) = (key, content) {
                if k.starts_with("og:") || k.starts_with("twitter:") || k.starts_with("dc.") {
                    map.insert(k.to_string(), Value::String(v.to_string()));
                }
            }
        }
    }

    map
}
