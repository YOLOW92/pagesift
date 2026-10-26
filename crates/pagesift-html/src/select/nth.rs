//! `:nth-child` 系列。直接数兄弟是 O(兄弟数)，`select()` 按文档顺序逐个匹配一组兄弟时
//! 整体变成平方（7.6 万个 `<li>` 配 `li:nth-child(2n+1)` 要 6 秒，模糊测试找到的）。
//! 和 Servo 的 nth-index cache 一样，在一次 `select()` 的上下文里记住最近算过的位置：
//! 下一个兄弟只要从上一个算过的元素接着数。

use std::cell::RefCell;

use crate::{Element, NodeId, NodeRef, Tag};

use super::Nth;
use super::matcher::Ctx;

/// 同时记住的（选择器，父节点，类型）组合数；超过后覆盖最旧的。
const SLOTS: usize = 8;

#[derive(Default)]
pub(crate) struct NthCache {
    entries: Vec<Entry>,
    next: usize,
}

struct Entry {
    /// 哪个 `:nth-*`（选择器在一次匹配期间不会移动，用地址区分）。
    nth: usize,
    parent: NodeId,
    /// `of_type` 时只数同类型的兄弟，类型也是键的一部分。
    ty: Option<(Tag, String)>,
    /// 最近算过的元素和它从前往后数的位置（从 1 开始）。
    node: NodeId,
    pos: i64,
    /// 参与计数的兄弟总数（`:nth-last-*` 用，第一次需要时才算）。
    total: Option<i64>,
}

pub(super) fn nth_matches(n: &Nth, el: NodeRef<'_>, e: &Element<'_>, cx: &Ctx) -> bool {
    if let Some(of) = &n.of
        && !of.matches_in(el, cx)
    {
        return false;
    }
    let tag = e.tag();
    let counts = |s: NodeRef<'_>| {
        let Some(st) = s.tag() else {
            return false;
        };
        if n.of_type {
            st == tag
                && (tag != Tag::Other || s.as_element().is_some_and(|se| se.name() == e.name()))
        } else if let Some(of) = &n.of {
            of.matches_in(s, cx)
        } else {
            true
        }
    };
    let pos = match el.parent() {
        Some(parent) => cached_position(n, el, e, parent.id(), &cx.nth, counts),
        None => {
            let p = forward(el, None, &counts).0;
            if n.last { 1 } else { p }
        }
    };
    let (a, b) = (i64::from(n.a), i64::from(n.b));
    if a == 0 {
        pos == b
    } else {
        let d = pos - b;
        d % a == 0 && d / a >= 0
    }
}

/// 从前往后数的位置：往前走，数参与计数的兄弟；碰到 `known`（已知位置的兄弟）就接上。
fn forward(
    el: NodeRef<'_>,
    known: Option<(NodeId, i64)>,
    counts: &impl Fn(NodeRef<'_>) -> bool,
) -> (i64, bool) {
    let mut between = 0;
    let mut cur = el.prev_sibling();
    while let Some(s) = cur {
        if let Some((id, p)) = known
            && s.id() == id
        {
            return (p + 1 + between, true);
        }
        if counts(s) {
            between += 1;
        }
        cur = s.prev_sibling();
    }
    (1 + between, false)
}

fn cached_position(
    n: &Nth,
    el: NodeRef<'_>,
    e: &Element<'_>,
    parent: NodeId,
    cache: &RefCell<NthCache>,
    counts: impl Fn(NodeRef<'_>) -> bool,
) -> i64 {
    let key = n as *const Nth as usize;
    let ty = n.of_type.then(|| (e.tag(), e.name().to_owned()));
    // 计数函数可能递归匹配（`of S`），借用不能跨过它
    let (slot, known, total) = {
        let c = cache.borrow();
        match c
            .entries
            .iter()
            .position(|x| x.nth == key && x.parent == parent && x.ty == ty)
        {
            Some(i) => (
                Some(i),
                Some((c.entries[i].node, c.entries[i].pos)),
                c.entries[i].total,
            ),
            None => (None, None, None),
        }
    };
    let pos = forward(el, known, &counts).0;
    let total = if n.last {
        Some(total.unwrap_or_else(|| {
            el.parent()
                .map_or(1, |p| p.children().filter(|&s| counts(s)).count() as i64)
        }))
    } else {
        total
    };
    let mut c = cache.borrow_mut();
    let entry = Entry {
        nth: key,
        parent,
        ty,
        node: el.id(),
        pos,
        total,
    };
    match slot {
        Some(i) if i < c.entries.len() => c.entries[i] = entry,
        _ if c.entries.len() < SLOTS => c.entries.push(entry),
        _ => {
            let i = c.next % SLOTS;
            c.entries[i] = entry;
            c.next += 1;
        }
    }
    if n.last {
        total.unwrap_or(1) - pos + 1
    } else {
        pos
    }
}
