//! DOM 元素拓扑指纹提取器：提取标签、文本、祖先拓扑路径、兄弟上下文与语义属性。

use pagesift_html::{Html, NodeRef, Selector};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const MAX_TOKENS: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ElementFingerprint {
    pub tag: String,
    pub text: String,
    pub tokens: BTreeSet<String>,
    pub ancestor_tags: Vec<String>,
    pub context_tokens: BTreeSet<String>,
    pub semantic_classes: BTreeSet<String>,
}

impl ElementFingerprint {
    /// 从原始文档的 CSS 选择器捕获目标元素的拓扑指纹。
    pub fn capture(doc: &Html<'_>, selector: &str) -> Result<Self, String> {
        let sel = Selector::parse(selector).map_err(|e| e.to_string())?;
        let mut nodes = doc.select(&sel);
        let node = nodes.next().ok_or("selector must match one element")?;
        if nodes.next().is_some() || !node.is_element() {
            return Err("selector must match exactly one element".into());
        }
        Ok(Self::from_node(node))
    }

    /// 从任意节点直接生成指纹。
    pub fn from_node(node: NodeRef<'_>) -> Self {
        let el = node.as_element().expect("must be element");
        let tag = el.name().to_string();
        let text = node.text().trim().to_string();
        let tokens = tokenize(&text);

        // 提取祖先标签链（由根向下）
        let mut ancestor_tags: Vec<String> = node
            .ancestors()
            .filter_map(|a| a.as_element().map(|e| e.name().to_string()))
            .collect();
        ancestor_tags.reverse();

        // 提取周围上下文词素（父容器与兄弟节点）
        let mut context_tokens = BTreeSet::new();
        if let Some(parent) = node.parent() {
            for sibling in parent.children() {
                if sibling != node {
                    for t in tokenize(&sibling.text()) {
                        context_tokens.insert(t);
                        if context_tokens.len() >= MAX_TOKENS {
                            break;
                        }
                    }
                }
            }
        }

        // 提取语义类名（过滤动态生成哈希类如 css-1x8dfa）
        let mut semantic_classes = BTreeSet::new();
        if let Some(cls_str) = el.attr("class") {
            for word in cls_str.split_whitespace() {
                if is_semantic_class(word) {
                    semantic_classes.insert(word.to_lowercase());
                }
            }
        }

        Self {
            tag,
            text,
            tokens,
            ancestor_tags,
            context_tokens,
            semantic_classes,
        }
    }
}

/// 判定类名是否为有意义的语义类名，而非随机编译哈希
fn is_semantic_class(name: &str) -> bool {
    if name.len() < 2 || name.len() > 40 {
        return false;
    }
    // 过滤 styled-components / CSS Modules / Tailwind 生成哈希 (如 css-1a2b3c, c-val-x98f, _3nt9)
    if name.starts_with("css-")
        || name.starts_with("c-")
        || name.starts_with('_')
        || (name.contains('-')
            && name
                .split('-')
                .next_back()
                .is_some_and(|s| s.len() >= 6 && s.chars().all(|c| c.is_ascii_hexdigit())))
    {
        return false;
    }
    true
}

pub fn tokenize(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() >= 2)
        .take(MAX_TOKENS)
        .map(|s| s.to_lowercase())
        .collect()
}
