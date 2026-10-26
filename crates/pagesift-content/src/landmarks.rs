//! 每个元素在不在正文区（祖先里有 `<main>`、`<article>` 或 `role=main`）、子树里有没有 `<nav>`。
//! 按文档顺序一遍算好。逐个元素沿祖先、后代走一遍时，深层嵌套（解析器的栈深上限是 512）
//! 让每个元素的代价乘上几百（模糊测试找到：180 万字节的嵌套 `<header>` 要 4 秒）。

use pagesift_html::{Html, NodeId, NodeRef, Tag};

const IN_AREA: u8 = 1;
const NAV_BELOW: u8 = 2;

pub(crate) struct Landmarks {
    bits: Vec<u8>,
}

fn is_area(node: NodeRef<'_>) -> bool {
    node.as_element().is_some_and(|e| {
        matches!(e.tag(), Tag::Main | Tag::Article) || e.attr("role") == Some("main")
    })
}

impl Landmarks {
    /// `order` 是所有元素的先序（父在子前）。
    pub(crate) fn compute(html: &Html<'_>, order: &[NodeId]) -> Landmarks {
        let mut bits = vec![0u8; html.len()];
        for &id in order {
            let node = html.get(id);
            if let Some(p) = node.parent()
                && p.is_element()
                && (bits[p.id().index()] & IN_AREA != 0 || is_area(p))
            {
                bits[id.index()] |= IN_AREA;
            }
        }
        for &id in order.iter().rev() {
            let node = html.get(id);
            let below = node
                .children()
                .any(|c| c.tag() == Some(Tag::Nav) || bits[c.id().index()] & NAV_BELOW != 0);
            if below {
                bits[id.index()] |= NAV_BELOW;
            }
        }
        Landmarks { bits }
    }

    /// 祖先（不含自身）里有 `<main>`、`<article>` 或 `role=main`。
    pub(crate) fn in_content_area(&self, node: &NodeRef<'_>) -> bool {
        self.bits
            .get(node.id().index())
            .is_some_and(|b| b & IN_AREA != 0)
    }

    /// 子树里（不含自身）有 `<nav>`。
    pub(crate) fn has_nav_below(&self, node: &NodeRef<'_>) -> bool {
        self.bits
            .get(node.id().index())
            .is_some_and(|b| b & NAV_BELOW != 0)
    }
}
