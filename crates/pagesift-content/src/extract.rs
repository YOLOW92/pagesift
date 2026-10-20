//! 正文定位：思路来自 Readability 的段落打分，针对中文标点和现代站点结构做了调整。

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

fn class_weight(el: &Element) -> f64 {
    let t = tokens(el);
    let mut w = 0.0;
    if has_token(&t, POSITIVE) {
        w += 25.0;
    }
    if has_token(&t, NEGATIVE) {
        w -= 25.0;
    }
    w
}

/// 决定渲染时要整棵跳过的子树。
#[derive(Clone, Copy)]
pub(crate) struct Skipper {
    pub only_main: bool,
}

impl Skipper {
    pub(crate) fn skip(&self, node: &NodeR<'_>) -> bool {
        let Some(el) = element(node) else {
            return node.value().is_comment();
        };
        let name = el.name();
        if is_non_content_tag(name) || is_hidden(el) {
            return true;
        }
        if !self.only_main {
            return false;
        }
        if matches!(name, "nav" | "aside" | "footer" | "form") {
            return true;
        }
        if el.attr("role").is_some_and(|r| SKIP_ROLES.contains(&r)) {
            return true;
        }
        let t = tokens(el);
        has_token(&t, NEGATIVE) && !has_token(&t, POSITIVE)
    }
}

#[derive(Default, Clone, Copy)]
struct TextStats {
    chars: usize,
    link_chars: usize,
    commas: usize,
}

fn stats(node: &NodeR<'_>, skipper: Skipper) -> TextStats {
    let mut s = TextStats::default();
    walk_stats(node, skipper, false, &mut s);
    s
}

fn walk_stats(node: &NodeR<'_>, skipper: Skipper, in_link: bool, s: &mut TextStats) {
    for child in node.children() {
        if let Some(t) = child.value().as_text() {
            let mut n = 0;
            for c in t.chars() {
                if !c.is_whitespace() {
                    n += 1;
                }
                if matches!(c, ',' | '，' | '、' | '。' | '；') {
                    s.commas += 1;
                }
            }
            s.chars += n;
            if in_link {
                s.link_chars += n;
            }
        } else if child.value().is_element() && !skipper.skip(&child) {
            let link = in_link || tag(&child) == Some("a");
            walk_stats(&child, skipper, link, s);
        }
    }
}

const BLOCK_CHILDREN: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "table",
    "ul",
    "ol",
    "pre",
    "blockquote",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "figure",
    "main",
    "header",
    "footer",
    "aside",
    "nav",
    "dl",
    "form",
];

fn is_paragraph_like(node: &NodeR<'_>) -> bool {
    match tag(node) {
        Some("p" | "pre" | "td" | "blockquote") => true,
        // 没有块级子元素的 div/section 视作段落：很多站点不用 <p>
        Some("div" | "section") => !node
            .children()
            .any(|c| tag(&c).is_some_and(|t| BLOCK_CHILDREN.contains(&t))),
        _ => false,
    }
}

fn tag_base_score(name: &str) -> f64 {
    match name {
        "article" => 10.0,
        "div" | "main" | "section" => 5.0,
        "pre" | "td" | "blockquote" => 3.0,
        "address" | "ol" | "ul" | "dl" | "dd" | "dt" | "li" | "form" => -3.0,
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "th" => -5.0,
        _ => 0.0,
    }
}

const MIN_PARAGRAPH_CHARS: usize = 25;
const MIN_ALTERNATIVES: usize = 3;

/// 选出正文所在的节点（按文档顺序）。通常是一个，正文被拆成多个兄弟块时会有多个。
/// 找不到合适候选时退回语义容器或 `<body>`。
pub(crate) fn main_roots(html: &Html, skipper: Skipper) -> Vec<NodeId> {
    let body = html
        .tree
        .root()
        .descendants()
        .find(|n| tag(n) == Some("body"))
        .unwrap_or_else(|| html.tree.root());

    let mut scores: HashMap<NodeId, f64> = HashMap::new();
    let mut order: Vec<NodeId> = Vec::new();

    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        if node.value().is_element() && node.id() != body.id() && skipper.skip(&node) {
            continue;
        }
        if is_paragraph_like(&node) {
            let s = stats(&node, skipper);
            if s.chars >= MIN_PARAGRAPH_CHARS {
                let score = 1.0 + s.commas as f64 + (s.chars as f64 / 100.0).min(3.0);
                for (level, ancestor) in node.ancestors().take(3).enumerate() {
                    let Some(el) = element(&ancestor) else { break };
                    let entry = scores.entry(ancestor.id()).or_insert_with(|| {
                        order.push(ancestor.id());
                        tag_base_score(el.name()) + class_weight(el)
                    });
                    let divider = match level {
                        0 => 1.0,
                        1 => 2.0,
                        _ => 3.0 * level as f64,
                    };
                    *entry += score / divider;
                }
            }
        }
        for child in node.children() {
            if child.value().is_element() {
                stack.push(child);
            }
        }
    }

    let mut final_scores: HashMap<NodeId, f64> = HashMap::new();
    let mut best: Option<(NodeId, f64)> = None;
    for id in order {
        let node = html.tree.get(id).expect("scored node exists");
        let score = scores[&id] * (1.0 - link_density(&stats(&node, skipper)));
        final_scores.insert(id, score);
        if best.is_none_or(|(_, b)| score > b) {
            best = Some((id, score));
        }
    }

    let Some((mut top, mut top_score)) = best else {
        return vec![semantic_main(body, skipper).unwrap_or(body.id())];
    };

    // 多个高分候选落在同一个祖先下（比如正文被切成一组 <section>），说明正文是那个祖先
    let mut ranked: Vec<(NodeId, f64)> = final_scores
        .iter()
        .filter(|(id, s)| **id != top && **s >= top_score * 0.75)
        .map(|(id, s)| (*id, *s))
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    ranked.truncate(5);
    if ranked.len() >= MIN_ALTERNATIVES {
        let top_node = html.tree.get(top).expect("candidate exists");
        for ancestor in top_node.ancestors() {
            if ancestor.id() == body.id() || !ancestor.value().is_element() {
                break;
            }
            let covered = ranked
                .iter()
                .filter(|(id, _)| {
                    html.tree
                        .get(*id)
                        .is_some_and(|n| n.ancestors().any(|a| a.id() == ancestor.id()))
                })
                .count();
            if covered >= MIN_ALTERNATIVES {
                top = ancestor.id();
                top_score = final_scores.get(&top).copied().unwrap_or(top_score);
                break;
            }
        }
    }

    // 沿父节点往上：只要父节点的分数没掉到三分之一以下，又出现比下面一层更高的分数，就提升
    {
        let mut last = top_score;
        let threshold = top_score / 3.0;
        let mut node = html.tree.get(top).expect("candidate exists");
        while let Some(parent) = node.parent() {
            if parent.id() == body.id() || !parent.value().is_element() {
                break;
            }
            let Some(&score) = final_scores.get(&parent.id()) else {
                break;
            };
            if score < threshold {
                break;
            }
            if score > last {
                top = parent.id();
                top_score = score;
            }
            last = score;
            node = parent;
        }
    }

    // 祖先里的文字明显更多、而且不是链接堆（导航、推荐列表），说明正文比候选更大
    {
        let top_chars = stats(&html.tree.get(top).expect("candidate exists"), skipper).chars;
        let mut node = html.tree.get(top).expect("candidate exists");
        while let Some(parent) = node.parent() {
            if parent.id() == body.id() || !parent.value().is_element() {
                break;
            }
            let Some(&score) = final_scores.get(&parent.id()) else {
                break;
            };
            if score < top_score / 3.0 {
                break;
            }
            let s = stats(&parent, skipper);
            if s.chars as f64 >= top_chars as f64 * 1.5 && link_density(&s) <= 0.25 {
                top = parent.id();
            }
            node = parent;
        }
    }

    let top_node = html.tree.get(top).expect("candidate exists");
    let Some(parent) = top_node.parent() else {
        return vec![top];
    };

    // 兄弟块合并：得分够高的兄弟候选，或者本身就是一段像样正文的兄弟节点
    let threshold = (top_score * 0.2).max(10.0);
    let mut roots = Vec::new();
    for sibling in parent.children() {
        if sibling.id() == top {
            roots.push(top);
            continue;
        }
        if !sibling.value().is_element() || skipper.skip(&sibling) {
            continue;
        }
        let include = if final_scores
            .get(&sibling.id())
            .is_some_and(|s| *s >= threshold)
        {
            true
        } else if tag(&sibling) == Some("p") {
            let s = stats(&sibling, skipper);
            (s.chars >= 80 && link_density(&s) < 0.25)
                || (s.chars > 0 && link_density(&s) == 0.0 && ends_sentence(&sibling))
        } else {
            false
        };
        if include {
            roots.push(sibling.id());
        }
    }
    roots
}

fn link_density(s: &TextStats) -> f64 {
    if s.chars == 0 {
        1.0
    } else {
        s.link_chars as f64 / s.chars as f64
    }
}

fn ends_sentence(node: &NodeR<'_>) -> bool {
    let text: String = node
        .descendants()
        .filter_map(|d| d.value().as_text().map(|t| t.to_string()))
        .collect();
    text.trim_end().ends_with(['.', '。', '!', '！', '?', '？'])
}

/// 唯一的 `<article>`，否则 `<main>` 或 `role="main"`。
fn semantic_main(body: NodeR<'_>, skipper: Skipper) -> Option<NodeId> {
    let visible = |n: &NodeR<'_>| {
        !n.ancestors()
            .any(|a| a.value().is_element() && skipper.skip(&a))
    };
    let articles: Vec<_> = body
        .descendants()
        .filter(|n| tag(n) == Some("article") && visible(n))
        .collect();
    if articles.len() == 1 {
        return Some(articles[0].id());
    }
    body.descendants()
        .find(|n| {
            (tag(n) == Some("main") || element(n).is_some_and(|e| e.attr("role") == Some("main")))
                && visible(n)
        })
        .map(|n| n.id())
}
