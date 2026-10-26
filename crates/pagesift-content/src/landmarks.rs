//! 每个元素在不在正文区（祖先里有 `<main>`、`<article>` 或 `role=main`）、子树里有没有 `<nav>`。
//! 逐个元素沿祖先、后代走一遍时，深层嵌套（解析器的栈深上限是 512）让每个元素的代价乘上几百
//! （模糊测试找到：180 万字节的嵌套 `<header>` 要 4 秒）。这里按需计算并记住每个走过的节点：
//! 正常页面只查几个 header/aside，开销和原来一样小；嵌套再深，每个节点也只算一次。

use std::cell::Cell;

use pagesift_html::{NodeRef, Tag};

const AREA_KNOWN: u8 = 1;
const IN_AREA: u8 = 2;
const NAV_KNOWN: u8 = 4;
const NAV_BELOW: u8 = 8;

pub(crate) struct Landmarks {
    bits: Vec<Cell<u8>>,
}

fn is_area(node: NodeRef<'_>) -> bool {
    node.as_element().is_some_and(|e| {
        matches!(e.tag(), Tag::Main | Tag::Article) || e.attr("role") == Some("main")
    })
}

impl Landmarks {
    pub(crate) fn new(nodes: usize) -> Landmarks {
        Landmarks {
            bits: (0..nodes).map(|_| Cell::new(0)).collect(),
        }
    }

    fn get(&self, node: NodeRef<'_>) -> u8 {
        self.bits.get(node.id().index()).map_or(0, Cell::get)
    }

    fn set(&self, node: NodeRef<'_>, bit: u8) {
        if let Some(c) = self.bits.get(node.id().index()) {
            c.set(c.get() | bit);
        }
    }

    /// 祖先（不含自身）里有 `<main>`、`<article>` 或 `role=main`。
    pub(crate) fn in_content_area(&self, node: &NodeRef<'_>) -> bool {
        // 往上走到第一个已知结果（或根），再沿原路把结果记下来
        let mut path = Vec::new();
        let mut cur = *node;
        let mut found = false;
        loop {
            let b = self.get(cur);
            if b & AREA_KNOWN != 0 {
                found = b & IN_AREA != 0;
                break;
            }
            path.push(cur);
            match cur.parent() {
                Some(p) if is_area(p) => {
                    found = true;
                    break;
                }
                Some(p) => cur = p,
                None => break,
            }
        }
        // path 里每个节点的祖先都包含 `cur` 往上的部分，结果相同
        for n in path {
            self.set(n, AREA_KNOWN | if found { IN_AREA } else { 0 });
        }
        found
    }

    /// 子树里（不含自身）有 `<nav>`。
    pub(crate) fn has_nav_below(&self, node: &NodeRef<'_>) -> bool {
        let b = self.get(*node);
        if b & NAV_KNOWN != 0 {
            return b & NAV_BELOW != 0;
        }
        // 后序遍历这棵子树，跳过已知结果的节点
        let mut stack = vec![(*node, false)];
        while let Some((n, children_done)) = stack.pop() {
            if children_done {
                let below = n
                    .children()
                    .any(|c| c.tag() == Some(Tag::Nav) || self.get(c) & NAV_BELOW != 0);
                self.set(n, NAV_KNOWN | if below { NAV_BELOW } else { 0 });
                continue;
            }
            stack.push((n, true));
            for c in n.children() {
                if c.is_element() && self.get(c) & NAV_KNOWN == 0 {
                    stack.push((c, false));
                }
            }
        }
        self.get(*node) & NAV_BELOW != 0
    }
}
