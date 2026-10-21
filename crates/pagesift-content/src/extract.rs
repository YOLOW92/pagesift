//! 只取正文时的跳过规则：去掉导航、页眉页脚、侧边栏、链接密集块这类页面框架。

use std::cell::Cell;

use pagesift_html::{Element, Html, Tag};

use crate::dom::{NodeR, element, is_hidden, is_non_content_tag};
use crate::markdown::is_block_tag;

/// 正文容器常用的词。
const POS: u8 = 1;
/// 页面框架（导航、页脚、广告、分享……）常用的词。
const NEG: u8 = 2;
/// 页内跳转链接（"skip to content"）的词，同时也算框架词。
const JUMP: u8 = 4;

/// 一个小写词属于哪些类。写成 `match`：编译器先按长度分支再比字节，每个词只判定一次。
fn word_class(w: &str) -> u8 {
    match w {
        "article" | "content" | "entry" | "main" | "post" | "text" | "blog" | "story"
        | "markdown" | "prose" | "doc" | "docs" | "documentation" | "body" => POS,
        "nav" | "navbar" | "navigation" | "menu" | "footer" | "sidebar" | "aside"
        | "breadcrumb" | "breadcrumbs" | "share" | "sharing" | "social" | "related"
        | "recommend" | "recommended" | "comment" | "comments" | "recent" | "cookie"
        | "cookies" | "consent" | "banner" | "advert" | "advertisement" | "ad" | "ads"
        | "sponsor" | "sponsored" | "newsletter" | "subscribe" | "popup" | "modal" | "promo"
        | "toolbar" | "pagination" | "editsection" | "catlinks" => NEG,
        "skip" | "jump" => NEG | JUMP,
        _ => 0,
    }
}

const SKIP_ROLES: &[&str] = &[
    "navigation",
    "banner",
    "contentinfo",
    "complementary",
    "search",
    "dialog",
    "alertdialog",
];

/// class 和 id 拆成词，按词（不分大小写）分类，返回所有词的类别并集。
/// 按词匹配避免 "header" 命中 "head" 这类误伤；词先转小写放进栈上的缓冲区。
fn token_class(el: Element<'_>) -> u8 {
    let mut class = 0;
    let mut buf = [0u8; MAX_TOKEN];
    for value in [el.attr("class"), el.attr("id")].into_iter().flatten() {
        // 按字节找 ASCII 字母数字片段：非 ASCII 字节（含多字节字符）都是分隔符
        let mut len = 0usize;
        for &c in value.as_bytes().iter().chain(std::iter::once(&b' ')) {
            if c.is_ascii_alphanumeric() {
                if len < MAX_TOKEN {
                    buf[len] = c.to_ascii_lowercase();
                }
                len += 1;
                continue;
            }
            if (1..=MAX_TOKEN).contains(&len) {
                // 只含 ASCII 字母数字，必然是合法 UTF-8
                class |= word_class(std::str::from_utf8(&buf[..len]).unwrap_or_default());
            }
            len = 0;
        }
    }
    class
}

/// 在 `<main>`、`<article>` 或 `role="main"` 里面：这里的页头、导航是正文的一部分
/// （文章标题区、日历插件的视图切换），不是页面框架。
fn in_content_area(node: &NodeR<'_>) -> bool {
    node.ancestors().any(|a| {
        a.as_element().is_some_and(|e| {
            matches!(e.tag(), Tag::Main | Tag::Article) || e.attr("role") == Some("main")
        })
    })
}

/// 比所有候选词都长的词不可能命中。
const MAX_TOKEN: usize = 16;

/// 决定渲染时要整棵跳过的子树。
///
/// 两类规则：
/// - 硬跳过：脚本、样式、隐藏元素，以及只取正文时的 `<nav>`、`<aside>`、`<footer>`；
/// - 软跳过（只取正文时）：带导航的 `<header>`、导航类 `role`、类名或 id 命中噪声词、
///   链接密集的块（没打标签的菜单、相关推荐、页脚链接）。
///   软跳过的元素如果装着全页大部分文字，说明它其实是正文的外层容器，不跳过。
pub(crate) struct Skipper {
    only_main: bool,
    /// 放宽后只保留硬跳过和 `<nav>`、`<aside>`、`<footer>`，不再用软跳过规则。
    relaxed: bool,
    total_chars: usize,
    protect_chars: usize,
    /// 按节点下标索引；只取正文时才计算。
    visible: Vec<Visible>,
    /// 子树里（不含自身）有没有块级元素，按节点下标索引。
    has_block: Vec<bool>,
    /// 跳过判断的缓存：0 未算，1 不跳过，2 跳过。
    cache: Vec<Cell<u8>>,
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
fn is_skip_link(node: &NodeR<'_>, el: Element<'_>) -> bool {
    if !el
        .attr("href")
        .is_some_and(|h| h.trim_start().starts_with('#'))
    {
        return false;
    }
    if token_class(el) & JUMP != 0 {
        return true;
    }
    let text: String = node
        .descendants()
        .filter_map(|d| d.as_text().map(|t| t.to_lowercase()))
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

fn is_hard_skip(node: &NodeR<'_>) -> bool {
    node.tag().is_some_and(is_non_content_tag) || is_hidden(node)
}

/// 自底向上一遍算出每个元素的可见文字统计和"有没有块级后代"。
fn compute(html: &Html<'_>, only_main: bool) -> (Vec<Visible>, Vec<bool>) {
    let n = html.len();
    let order: Vec<NodeR<'_>> = html
        .root()
        .descendants()
        .filter(|d| d.is_element())
        .collect();
    let mut visible = if only_main {
        vec![Visible::default(); n]
    } else {
        Vec::new()
    };
    let mut has_block = vec![false; n];
    for node in order.iter().rev() {
        let id = node.id().index();
        let hard = only_main && is_hard_skip(node);
        let mut v = Visible::default();
        let mut block = false;
        for child in node.children() {
            if let Some(t) = child.as_text() {
                if only_main && !hard {
                    v.chars += crate::dom::count_non_whitespace(t);
                }
                continue;
            }
            let Some(tag) = child.tag() else { continue };
            let c = child.id().index();
            block |= is_block_tag(tag) || has_block[c];
            if only_main && !hard {
                let cv = visible[c];
                v.chars += cv.chars;
                if tag == Tag::A {
                    v.link_chars += cv.chars;
                    v.links += 1;
                } else {
                    v.link_chars += cv.link_chars;
                    v.links += cv.links;
                }
            }
        }
        has_block[id] = block;
        if only_main {
            visible[id] = v;
        }
    }
    (visible, has_block)
}

impl Skipper {
    pub(crate) fn new(html: &Html<'_>, only_main: bool) -> Self {
        let body = html.body();
        let (visible, has_block) = compute(html, only_main);
        let total = visible.get(body.id().index()).map_or(0, |v| v.chars);
        Self {
            only_main,
            relaxed: false,
            total_chars: total,
            protect_chars: ((total as f64 * PROTECT_SHARE) as usize).max(PROTECT_MIN_CHARS),
            visible,
            has_block,
            cache: (0..html.len()).map(|_| Cell::new(0)).collect(),
        }
    }

    /// 页面可见文字的总字符数（只排除硬跳过的内容）。
    pub(crate) fn total_chars(&self) -> usize {
        self.total_chars
    }

    /// 关掉软跳过规则，用于正文被删过头时的重试。
    pub(crate) fn relax(&mut self) {
        self.relaxed = true;
        self.cache.iter().for_each(|c| c.set(0));
    }

    pub(crate) fn skip(&self, node: &NodeR<'_>) -> bool {
        if !node.is_element() {
            return node.is_comment();
        }
        let cell = &self.cache[node.id().index()];
        match cell.get() {
            1 => false,
            2 => true,
            _ => {
                let v = self.decide(node);
                cell.set(1 + v as u8);
                v
            }
        }
    }

    /// 子树里（不含自身）有没有块级元素。
    pub(crate) fn has_block_descendant(&self, node: &NodeR<'_>) -> bool {
        self.has_block[node.id().index()]
    }

    fn decide(&self, node: &NodeR<'_>) -> bool {
        self.reason(node).is_some()
    }

    /// 跳过这个元素的原因；不跳过时为 `None`。诊断工具用它解释每个被删掉的块。
    pub(crate) fn reason(&self, node: &NodeR<'_>) -> Option<&'static str> {
        let el = element(node)?;
        let tag = el.tag();
        if is_non_content_tag(tag) {
            return Some("non-content tag");
        }
        if is_hidden(node) {
            return Some("hidden");
        }
        if !self.only_main {
            return None;
        }
        if matches!(tag, Tag::Nav | Tag::Aside | Tag::Footer) {
            return Some("nav/aside/footer");
        }
        if tag == Tag::A {
            return is_skip_link(node, el).then_some("skip link");
        }
        if self.relaxed || matches!(tag, Tag::Body | Tag::Html) {
            return None;
        }
        let vis = self
            .visible
            .get(node.id().index())
            .copied()
            .unwrap_or_default();
        if vis.chars >= self.protect_chars {
            return None;
        }
        let link_block = vis.links >= LINK_BLOCK_MIN_LINKS
            && vis.link_chars as f64 >= vis.chars as f64 * LINK_BLOCK_MIN_SHARE
            && (vis.link_chars as f64 / vis.links as f64) <= LINK_BLOCK_MAX_AVG_CHARS;
        // <main> 里的链接列表多半是内容本身（目录页、相册、分类列表），不当菜单删
        if link_block && !node.ancestors().any(|a| a.tag() == Some(Tag::Main)) {
            Some("link block")
        } else if tag == Tag::Header
            && !in_content_area(node)
            && node.descendants().any(|d| d.tag() == Some(Tag::Nav))
        {
            Some("header with nav")
        } else if el.attr("role").is_some_and(|r| SKIP_ROLES.contains(&r)) {
            Some("skip role")
        } else if token_class(el) & (NEG | POS) == NEG {
            Some("negative class/id")
        } else {
            None
        }
    }

    /// 元素自身的可见文字数（只算未被硬跳过的部分；只取正文时才有）。
    pub(crate) fn visible_chars(&self, node: &NodeR<'_>) -> usize {
        self.visible.get(node.id().index()).map_or(0, |v| v.chars)
    }

    pub(crate) fn only_main(&self) -> bool {
        self.only_main
    }

    pub(crate) fn is_relaxed(&self) -> bool {
        self.relaxed
    }
}
