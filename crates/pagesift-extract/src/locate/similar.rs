//! `find_similar`：给一个例子，找出结构相同的兄弟节点——只指一个商品就能抽整个列表。
//!
//! “结构相同”：标签相同、语义类名集合相同、子元素标签集合相同（`Fingerprint` 里的 `tag`、`classes`、
//! `shape`）。从例子本身开始往上找：哪一层的父元素下有和它结构相同的兄弟，那一层就是列表项。
//! 例子是商品卡片里的价格时，价格自己没有兄弟，往上一层的卡片才有。

use pagesift_html::NodeRef;

use super::fingerprint::Fingerprint;
use super::robust::robust_selector;

/// 找到的列表：列表项（含例子那一项，文档顺序）、能取出这一组的选择器、例子在第几层祖先。
#[derive(Debug, Clone)]
pub struct Similar<'a> {
    pub items: Vec<NodeRef<'a>>,
    /// 在作用域内恰好取出这些列表项的选择器；拼不出时为 `None`（列表项仍然可用）。
    pub selector: Option<String>,
    /// 列表项是例子的第几层祖先（0 = 例子本身）。
    pub depth: usize,
}

fn signature(n: NodeRef<'_>) -> Option<(String, Vec<String>, Vec<String>)> {
    let f = Fingerprint::of(n)?;
    Some((
        f.tag,
        f.classes.into_iter().collect(),
        f.shape.into_iter().collect(),
    ))
}

/// 在 `scope` 内找和 `example` 结构相同的兄弟；找不到时返回 `None`。
pub fn find_similar<'a>(example: NodeRef<'a>, scope: NodeRef<'a>) -> Option<Similar<'a>> {
    let mut node = example;
    let mut depth = 0;
    loop {
        if node == scope {
            return None;
        }
        let parent = node.parent()?;
        let sig = signature(node)?;
        let items: Vec<NodeRef<'a>> = parent
            .children()
            .filter(|c| c.is_element() && signature(*c).as_ref() == Some(&sig))
            .collect();
        if items.len() >= 2 {
            return Some(Similar {
                selector: group_selector(&items, parent, scope),
                items,
                depth,
            });
        }
        node = parent;
        depth += 1;
    }
}

/// 父元素的稳健选择器加列表项的标签和语义类名，验证恰好取出这一组。
fn group_selector(
    items: &[NodeRef<'_>],
    parent: NodeRef<'_>,
    scope: NodeRef<'_>,
) -> Option<String> {
    let first = items[0].as_element()?;
    let classes: Vec<String> = first
        .attr("class")
        .map(|c| super::fingerprint::semantic_classes(c).collect())
        .unwrap_or_default();
    let item = if classes.is_empty() {
        first.name().to_string()
    } else {
        format!("{}.{}", first.name(), classes.join("."))
    };
    let parent_sel = if parent == scope || parent.is_document() {
        None
    } else {
        robust_selector(parent, scope)
    };
    let sel = match parent_sel {
        Some(p) => format!("{p} > {item}"),
        None if scope.is_document() => item,
        None => format!(":scope > {item}"),
    };
    let parsed = pagesift_html::Selector::parse(&sel).ok()?;
    let hits: Vec<NodeRef<'_>> = scope.select(&parsed).collect();
    (hits == items).then_some(sel)
}
