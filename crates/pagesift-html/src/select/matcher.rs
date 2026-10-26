use std::cell::RefCell;

use crate::{Element, NodeId, NodeRef, Tag};

use super::nth::{NthCache, nth_matches};
use super::{AttrOp, AttrSel, Comb, Compound, Selector, Simple};

// ---------------------------------------------------------------------------
// 匹配
// ---------------------------------------------------------------------------

pub(crate) struct Ctx {
    /// `:scope` 指向的元素；`None` 时是根元素。
    pub(super) scope: Option<NodeId>,
    /// `:has()` 正在判断的元素。
    pub(super) anchor: Option<NodeId>,
    /// `:nth-*` 的位置缓存，见 `nth` 模块。
    pub(super) nth: RefCell<NthCache>,
}

impl Ctx {
    pub(super) fn new(scope: Option<NodeId>, anchor: Option<NodeId>) -> Ctx {
        Ctx {
            scope,
            anchor,
            nth: RefCell::default(),
        }
    }
}

/// 复杂选择器的匹配结果。失败时告诉上层从哪里重新开始，避免后代组合器的指数回溯
/// （和 Servo selectors 的 `SelectorMatchingResult` 同一套规则）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Res {
    Matched,
    RestartFromLaterSibling,
    RestartFromDescendant,
    NotMatchedGlobally,
}

pub(super) fn match_complex(parts: &[Compound], i: usize, el: NodeRef<'_>, cx: &Ctx) -> Res {
    let part = &parts[i];
    if !part.tag_may_match(el) || !match_compound(&part.simples, el, cx) {
        return Res::RestartFromLaterSibling;
    }
    if i + 1 == parts.len() {
        return Res::Matched;
    }
    let comb = part.comb;
    let not_found = match comb {
        Comb::Next | Comb::Later => Res::RestartFromDescendant,
        Comb::Child | Comb::Descendant => Res::NotMatchedGlobally,
    };
    let mut cur = el;
    loop {
        let next = match comb {
            Comb::Child | Comb::Descendant => cur.parent().filter(NodeRef::is_element),
            Comb::Next | Comb::Later => prev_element(cur),
        };
        let Some(next) = next else {
            return not_found;
        };
        // 标签对不上时不必递归，结果就是复合选择器不匹配
        let r = if parts[i + 1].tag_may_match(next) {
            match_complex(parts, i + 1, next, cx)
        } else {
            Res::RestartFromLaterSibling
        };
        match (r, comb) {
            (Res::Matched | Res::NotMatchedGlobally, _) | (_, Comb::Next) => return r,
            (_, Comb::Child) => return Res::RestartFromDescendant,
            (Res::RestartFromDescendant, Comb::Later) => return r,
            _ => {}
        }
        cur = next;
    }
}

impl Compound {
    /// 按标签快速排除。外部内容里的元素都是 Other，交给 [`type_matches`] 比名字。
    #[inline]
    pub(super) fn tag_may_match(&self, el: NodeRef<'_>) -> bool {
        match self.tag {
            Some(t) => matches!(el.tag(), Some(x) if x == t || x == Tag::Other),
            None => true,
        }
    }
}

fn match_compound(simples: &[Simple], el: NodeRef<'_>, cx: &Ctx) -> bool {
    let Some(e) = el.as_element() else {
        return false;
    };
    simples.iter().all(|s| match_simple(s, el, &e, cx))
}

fn match_simple(s: &Simple, el: NodeRef<'_>, e: &Element<'_>, cx: &Ctx) -> bool {
    match s {
        Simple::Type(tag, name) => type_matches(e, *tag, name),
        Simple::Id(id) => e.attr("id") == Some(&**id),
        Simple::Class(c) => e
            .attr("class")
            .is_some_and(|v| v.split_ascii_whitespace().any(|x| x == &**c)),
        Simple::Attr(a) => e.attr(&a.name).is_some_and(|v| attr_matches(a, v)),
        Simple::Nth(n) => nth_matches(n, el, e, cx),
        Simple::Empty => !el.children().any(|c| c.is_element() || c.is_text()),
        Simple::Root => el.parent().is_some_and(|p| p.is_document()),
        Simple::Scope => match cx.scope {
            Some(id) => el.id() == id,
            None => el.parent().is_some_and(|p| p.is_document()),
        },
        Simple::Anchor => cx.anchor == Some(el.id()),
        Simple::Not(sel) => !sel.matches_in(el, cx),
        Simple::Is(sel) => sel.matches_in(el, cx),
        Simple::Has(sel) => has_matches(sel, el, cx),
    }
}

fn type_matches(e: &Element<'_>, tag: Tag, name: &str) -> bool {
    let t = e.tag();
    if t == tag {
        tag != Tag::Other || e.name() == name
    } else {
        // SVG/MathML 里的元素都是 Other，名字可能和 HTML 标签相同（例如 svg 的 <a>）
        t == Tag::Other && e.name() == name
    }
}

fn attr_matches(a: &AttrSel, v: &str) -> bool {
    let (v, want) = (v.as_bytes(), a.value.as_bytes());
    let eq = |x: &[u8], y: &[u8]| {
        if a.ci {
            x.eq_ignore_ascii_case(y)
        } else {
            x == y
        }
    };
    match a.op {
        AttrOp::Exists => true,
        AttrOp::Equals => eq(v, want),
        AttrOp::Includes => v
            .split(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\x0c' | b'\r'))
            .any(|w| eq(w, want)),
        AttrOp::Dash => {
            eq(v, want)
                || (v.len() > want.len() && v[want.len()] == b'-' && eq(&v[..want.len()], want))
        }
        AttrOp::Prefix => v.len() >= want.len() && eq(&v[..want.len()], want),
        AttrOp::Suffix => v.len() >= want.len() && eq(&v[v.len() - want.len()..], want),
        AttrOp::Substring => {
            if a.ci {
                v.windows(want.len()).any(|w| w.eq_ignore_ascii_case(want))
            } else {
                memchr::memmem::find(v, want).is_some()
            }
        }
        AttrOp::Never => false,
    }
}

/// `:has()`：锚点后面（后代或之后的兄弟及其后代）有元素匹配相对选择器。
fn has_matches(sel: &Selector, el: NodeRef<'_>, cx: &Ctx) -> bool {
    let inner = Ctx::new(cx.scope, Some(el.id()));
    sel.list.iter().any(|c| {
        let leading = c.parts[c.parts.len() - 2].comb;
        let hit = |n: NodeRef<'_>| {
            n.is_element() && match_complex(&c.parts, 0, n, &inner) == Res::Matched
        };
        // 只有一个复合选择器时（`:has(> img)`、`:has(+ p)`），候选范围可以收窄到子节点或兄弟
        let single = c.parts.len() == 2;
        match leading {
            Comb::Child if single => el.children().any(hit),
            Comb::Next if single => next_element(el).is_some_and(hit),
            Comb::Later if single => {
                let mut s = el.next_sibling();
                while let Some(n) = s {
                    if hit(n) {
                        return true;
                    }
                    s = n.next_sibling();
                }
                false
            }
            Comb::Descendant | Comb::Child => el.descendants().skip(1).any(hit),
            Comb::Next | Comb::Later => {
                let mut s = el.next_sibling();
                while let Some(n) = s {
                    if n.descendants().any(hit) {
                        return true;
                    }
                    s = n.next_sibling();
                }
                false
            }
        }
    })
}

fn next_element<'a>(n: NodeRef<'a>) -> Option<NodeRef<'a>> {
    let mut cur = n.next_sibling();
    while let Some(c) = cur {
        if c.is_element() {
            return Some(c);
        }
        cur = c.next_sibling();
    }
    None
}

fn prev_element<'a>(n: NodeRef<'a>) -> Option<NodeRef<'a>> {
    let mut cur = n.prev_sibling();
    while let Some(c) = cur {
        if c.is_element() {
            return Some(c);
        }
        cur = c.prev_sibling();
    }
    None
}
