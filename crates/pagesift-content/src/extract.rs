//! 只取正文时的跳过规则：去掉导航、页眉页脚、侧边栏、链接密集块这类页面框架。

use std::cell::RefCell;
use std::collections::HashMap;

use ego_tree::NodeId;
use scraper::Html;
use scraper::node::Element;

use crate::dom::{NodeR, element, is_hidden, is_non_content_tag, tag};

const POSITIVE: &[&str] = &[
    "article",
    "content",
    "entry",
    "main",
    "post",
    "text",
    "blog",
    "story",
    "markdown",
    "prose",
    "doc",
    "docs",
    "documentation",
    "body",
];

const NEGATIVE: &[&str] = &[
    "nav",
    "navbar",
    "navigation",
    "menu",
    "footer",
    "sidebar",
    "aside",
    "breadcrumb",
    "breadcrumbs",
    "share",
    "sharing",
    "social",
    "related",
    "recommend",
    "recommended",
    "comment",
    "comments",
    "cookie",
    "cookies",
    "consent",
    "banner",
    "advert",
    "advertisement",
    "ad",
    "ads",
    "sponsor",
    "sponsored",
    "newsletter",
    "subscribe",
    "popup",
    "modal",
    "promo",
    "toolbar",
    "pagination",
    "edit",
    "editsection",
    "skip",
    "jump",
    "catlinks",
];

const SKIP_ROLES: &[&str] = &[
    "navigation",
    "banner",
    "contentinfo",
    "complementary",
    "search",
    "dialog",
    "alertdialog",
];

/// class 和 id 拆成小写词，按词匹配，避免 "header" 命中 "head" 这类误伤。
fn tokens(el: &Element) -> Vec<String> {
    let mut out = Vec::new();
    for value in [el.attr("class"), el.attr("id")].into_iter().flatten() {
        for t in value.split(|c: char| !c.is_ascii_alphanumeric()) {
            if !t.is_empty() {
                out.push(t.to_ascii_lowercase());
            }
        }
    }
    out
}

fn has_token(tokens: &[String], set: &[&str]) -> bool {
    tokens.iter().any(|t| set.contains(&t.as_str()))
}

/// 决定渲染时要整棵跳过的子树。
///
/// 两类规则：
/// - 硬跳过：脚本、样式、隐藏元素，以及只取正文时的 `<nav>`、`<aside>`、`<footer>`；
/// - 软跳过（只取正文时）：`<form>`、带导航的 `<header>`、导航类 `role`、类名或 id 命中噪声词、
///   链接密集的块（没打标签的菜单、相关推荐、页脚链接）。
///   软跳过的元素如果装着全页大部分文字，说明它其实是正文的外层容器，不跳过。
pub(crate) struct Skipper {
    only_main: bool,
    protect_chars: usize,
    visible: HashMap<NodeId, Visible>,
    cache: RefCell<HashMap<NodeId, bool>>,
}

/// 子树里可见文字的统计，只排除硬跳过的内容。构造 Skipper 时自底向上一次算好。
#[derive(Default, Clone, Copy)]
struct Visible {
    chars: usize,
    link_chars: usize,
    links: usize,
}

/// 软跳过的元素文字量达到全页的这个比例时，视为正文容器。
const PROTECT_SHARE: f64 = 0.4;
/// 文字太少的页面不做保护，免得导航因为"占比高"被留下。
const PROTECT_MIN_CHARS: usize = 200;
/// 链接密集块：至少这么多个链接，且链接文字占比不低于这个比例。
const LINK_BLOCK_MIN_LINKS: usize = 3;
const LINK_BLOCK_MIN_SHARE: f64 = 0.8;
/// 菜单的链接文字短，内容卡片的链接文字是整句标题：平均链接文字超过这个长度就不算链接块。
const LINK_BLOCK_MAX_AVG_CHARS: f64 = 20.0;

/// "Skip to content"、"Jump to content" 这类页内跳转链接。
fn is_skip_link(node: &NodeR<'_>, el: &Element) -> bool {
    if !el
        .attr("href")
        .is_some_and(|h| h.trim_start().starts_with('#'))
    {
        return false;
    }
    let t = tokens(el);
    if t.iter().any(|t| t == "skip" || t == "jump") {
        return true;
    }
    let text: String = node
        .descendants()
        .filter_map(|d| d.value().as_text().map(|t| t.to_lowercase()))
        .collect();
    let text = text.trim_start();
    [
        "skip to",
        "skip navigation",
        "jump to",
        "跳到",
        "跳转到",
        "跳至",
    ]
    .iter()
    .any(|p| text.starts_with(p))
}

fn is_hard_skip(el: &Element) -> bool {
    is_non_content_tag(el.name()) || is_hidden(el)
}

fn compute_visible(root: NodeR<'_>) -> HashMap<NodeId, Visible> {
    let order: Vec<NodeR<'_>> = root
        .descendants()
        .filter(|n| n.value().is_element())
        .collect();
    let mut map: HashMap<NodeId, Visible> = HashMap::with_capacity(order.len());
    for node in order.iter().rev() {
        let mut v = Visible::default();
        if !element(node).is_some_and(is_hard_skip) {
            for child in node.children() {
                if let Some(t) = child.value().as_text() {
                    v.chars += t.chars().filter(|c| !c.is_whitespace()).count();
                } else if let Some(c) = map.get(&child.id()) {
                    v.chars += c.chars;
                    if tag(&child) == Some("a") {
                        v.link_chars += c.chars;
                        v.links += 1;
                    } else {
                        v.link_chars += c.link_chars;
                        v.links += c.links;
                    }
                }
            }
        }
        map.insert(node.id(), v);
    }
    map
}

impl Skipper {
    pub(crate) fn new(html: &Html, only_main: bool) -> Self {
        let body = html
            .tree
            .root()
            .descendants()
            .find(|n| tag(n) == Some("body"))
            .unwrap_or_else(|| html.tree.root());
        let visible = if only_main {
            compute_visible(html.tree.root())
        } else {
            HashMap::new()
        };
        let total = visible.get(&body.id()).map_or(0, |v| v.chars);
        Self {
            only_main,
            protect_chars: ((total as f64 * PROTECT_SHARE) as usize).max(PROTECT_MIN_CHARS),
            visible,
            cache: RefCell::new(HashMap::new()),
        }
    }

    pub(crate) fn skip(&self, node: &NodeR<'_>) -> bool {
        if !node.value().is_element() {
            return node.value().is_comment();
        }
        if let Some(&v) = self.cache.borrow().get(&node.id()) {
            return v;
        }
        let v = self.compute(node);
        self.cache.borrow_mut().insert(node.id(), v);
        v
    }

    fn compute(&self, node: &NodeR<'_>) -> bool {
        let Some(el) = element(node) else {
            return false;
        };
        let name = el.name();
        if is_hard_skip(el) {
            return true;
        }
        if !self.only_main {
            return false;
        }
        if matches!(name, "nav" | "aside" | "footer") {
            return true;
        }
        if name == "a" {
            return is_skip_link(node, el);
        }
        if matches!(name, "body" | "html") {
            return false;
        }
        let vis = self.visible.get(&node.id()).copied().unwrap_or_default();
        let link_block = vis.links >= LINK_BLOCK_MIN_LINKS
            && vis.link_chars as f64 >= vis.chars as f64 * LINK_BLOCK_MIN_SHARE
            && (vis.link_chars as f64 / vis.links as f64) <= LINK_BLOCK_MAX_AVG_CHARS;
        let soft = link_block
            || name == "form"
            || (name == "header" && node.descendants().any(|d| tag(&d) == Some("nav")))
            || el.attr("role").is_some_and(|r| SKIP_ROLES.contains(&r))
            || {
                let t = tokens(el);
                has_token(&t, NEGATIVE) && !has_token(&t, POSITIVE)
            };
        soft && vis.chars < self.protect_chars
    }
}
