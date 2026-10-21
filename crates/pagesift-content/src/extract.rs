//! 只取正文时的跳过规则：去掉导航、页眉页脚、侧边栏、链接密集块这类页面框架。

use std::cell::Cell;

use pagesift_html::{Element, Html, Tag};

use crate::dom::{NodeR, element, is_hidden, is_non_content_tag, short_text};
use crate::markdown::is_block_tag;

/// 正文容器常用的词。
const POS: u8 = 1;
/// 页面框架（导航、页脚、广告、分享……）常用的词。
const NEG: u8 = 2;
/// 页内跳转链接（"skip to content"）的词，同时也算框架词。
const JUMP: u8 = 4;
/// 只在文章模式里算框架的词：评论、标签、署名、面包屑。普通页面上它们可能就是内容
/// （论坛的评论、标签页），文章页上它们在正文之外。
const STRICT: u8 = 8;

/// 一个小写词属于哪些类。写成 `match`：编译器先按长度分支再比字节，每个词只判定一次。
///
/// 不算框架词的：`banner`（建站主题里多指页首大图区，标题和导语就在里面；广告用 `ad`、`advert`，
/// 页头用 `role="banner"` 识别）、`comment(s)`（评论区是页面内容）、`breadcrumb(s)`（页面在站内的位置，
/// `<nav>` 里的面包屑照样按 `<nav>` 去掉）、`recommend`（多是动词，如 `doctor-recommend`）。
fn word_class(w: &str) -> u8 {
    match w {
        "article" | "content" | "entry" | "main" | "post" | "text" | "blog" | "story"
        | "markdown" | "prose" | "doc" | "docs" | "documentation" | "body" => POS,
        "nav" | "navbar" | "navigation" | "menu" | "footer" | "sidebar" | "aside" | "share"
        | "sharing" | "social" | "related" | "recommended" | "recent" | "cookie" | "cookies"
        | "consent" | "advert" | "advertisement" | "ad" | "ads" | "sponsor" | "sponsored"
        | "newsletter" | "subscribe" | "popup" | "modal" | "promo" | "toolbar" | "pagination"
        | "editsection" | "catlinks" => NEG,
        "skip" | "jump" => NEG | JUMP,
        "comment" | "comments" | "byline" | "tags" | "tag" | "breadcrumb" | "breadcrumbs"
        | "author" | "authors" | "disqus" | "rating" | "print" | "follow" | "outbrain"
        | "taboola" | "embed" => STRICT,
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
/// 一个类名（空白分隔的一项）里同时有正文词和框架词时按框架词算：`related-content-boxes`、
/// `menu-content`、`menu-item--main` 是框架里的部件（BEM 写法里块名在前）；正文容器另外带着
/// 只有正文词的类名（`main-content has-sidebar`）。
fn token_class(el: Element<'_>) -> u8 {
    let mut class = 0;
    let mut buf = [0u8; MAX_TOKEN];
    for value in [el.attr("class"), el.attr("id")].into_iter().flatten() {
        // 按字节找 ASCII 字母数字片段：非 ASCII 字节（含多字节字符）都是分隔符
        let mut len = 0usize;
        let mut name = 0u8;
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
                name |= word_class(std::str::from_utf8(&buf[..len]).unwrap_or_default());
            }
            len = 0;
            if c.is_ascii_whitespace() {
                if name & NEG != 0 {
                    name &= !POS;
                }
                class |= name;
                name = 0;
            }
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

fn is_main_landmark(el: Element<'_>) -> bool {
    el.tag() == Tag::Main || el.attr("role") == Some("main")
}

/// 比所有候选词都长的词不可能命中。
const MAX_TOKEN: usize = 16;

/// 正文被删过头时放宽规则的程度。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Relax {
    None,
    /// 软跳过规则只留链接块：菜单、页脚链接还删，按类名、role、页头删的块放回来。
    LinkBlocks,
    /// 只保留硬跳过和 `<nav>`、`<aside>`、`<footer>`。
    All,
}

/// 决定渲染时要整棵跳过的子树。
///
/// 两类规则：
/// - 硬跳过：脚本、样式、隐藏元素，以及只取正文时的 `<nav>`、`<aside>`、`<footer>`；
/// - 软跳过（只取正文时）：带导航的 `<header>`、导航类 `role`、类名或 id 命中噪声词、
///   链接密集的块（没打标签的菜单、相关推荐、页脚链接）。
///   软跳过的元素如果装着全页大部分文字，说明它其实是正文的外层容器，不跳过。
pub(crate) struct Skipper {
    only_main: bool,
    /// 文章模式：文章主体里也删链接列表，评论、标签、署名这类块也算框架。
    strict: bool,
    /// 文章模式下，按严格规则删掉的块最多能有多少非链接文字（主体的一部分）：大块的不删，
    /// 免得类名凑巧命中（`meta`、`slide`）把正文删掉。
    strict_max_chars: usize,

    relaxed: Relax,
    total_chars: usize,
    protect_chars: usize,
    /// 按节点下标索引；只取正文时才计算。
    visible: Vec<Visible>,
    /// 子树里（不含自身）有没有块级元素，按节点下标索引。
    has_block: Vec<bool>,
    /// 跳过判断的缓存：0 未算，1 不跳过，2 跳过。
    cache: Vec<Cell<u8>>,
    /// class/id 的分类（[`token_class`]），和模式无关，每个节点只算一次；[`UNKNOWN`] 表示还没算。
    classes: Vec<Cell<u8>>,
    /// [`crate::features`] 的特征位，每个节点只算一次；`u32::MAX` 表示还没算。
    features: Vec<Cell<u32>>,
}

/// 子树里可见文字的统计，只排除硬跳过的内容。构造 Skipper 时自底向上一次算好。
#[derive(Default, Clone, Copy)]
struct Visible {
    chars: usize,
    link_chars: usize,
    links: usize,
    images: u32,
    /// 包着图片的链接数（缩略图、卡片）。
    img_links: u32,
    /// 有文字的链接数，和其中文字不到 10 个字符的（"更多"、"分享"这类）。
    text_links: u32,
    short_links: u32,
}

const UNKNOWN: u8 = 0xFF;

/// 文章模式的严格规则只删非链接文字不到主体这个比例的块。
const STRICT_MAX_SHARE: f64 = 0.3;

/// 软跳过的元素文字量达到全页的这个比例时，视为正文容器。
const PROTECT_SHARE: f64 = 0.4;
/// 文字太少的页面不做保护，免得导航因为"占比高"被留下。
const PROTECT_MIN_CHARS: usize = 200;
/// `<nav>`、`<aside>`、`<footer>` 装着全页这个比例以上的文字、链接文字又不到这个比例时，
/// 是整页的外层容器，不当页面框架。
const WRAPPER_SHARE: f64 = 0.7;
const WRAPPER_MAX_LINK_SHARE: f64 = 0.5;
/// 链接密集块：至少这么多个链接，且链接文字占比不低于这个比例。
const LINK_BLOCK_MIN_LINKS: usize = 3;
const LINK_BLOCK_MIN_SHARE: f64 = 0.8;
/// 菜单的链接文字短，内容卡片的链接文字是整句标题：平均链接文字超过这个长度就不算链接块。
/// 一半以上的链接包着图片（相册、商品卡片）也不算。
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
    node.tag().is_some_and(is_non_content_tag) && !is_text_button(node) || is_hidden(node)
}

/// 按钮文字超过这么多（非空白字符）就算正文；短的是"菜单""搜索""提交"这类控件。
const TEXT_BUTTON_MIN_CHARS: usize = 15;

/// 装着成段文字的按钮（手风琴标题、整块可点的卡片）：文字照常显示，算正文。
fn is_text_button(node: &NodeR<'_>) -> bool {
    node.tag() == Some(Tag::Button) && !short_text(node, TEXT_BUTTON_MIN_CHARS)
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
                v.images += cv.images + u32::from(tag == Tag::Img);
                if tag == Tag::A {
                    v.link_chars += cv.chars;
                    v.links += 1;
                    v.img_links += u32::from(cv.images > 0);
                    v.text_links += u32::from(cv.chars > 0);
                    v.short_links += u32::from(cv.chars > 0 && cv.chars < 10);
                } else {
                    v.link_chars += cv.link_chars;
                    v.links += cv.links;
                    v.img_links += cv.img_links;
                    v.text_links += cv.text_links;
                    v.short_links += cv.short_links;
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
            strict: false,
            strict_max_chars: 0,

            relaxed: Relax::None,
            total_chars: total,
            protect_chars: ((total as f64 * PROTECT_SHARE) as usize).max(PROTECT_MIN_CHARS),
            visible,
            has_block,
            cache: (0..html.len()).map(|_| Cell::new(0)).collect(),
            classes: (0..html.len()).map(|_| Cell::new(UNKNOWN)).collect(),
            features: (0..html.len()).map(|_| Cell::new(u32::MAX)).collect(),
        }
    }

    /// 节点 class/id 的分类，带缓存。
    fn classes(&self, node: &NodeR<'_>, el: Element<'_>) -> u8 {
        let cell = &self.classes[node.id().index()];
        let c = cell.get();
        if c != UNKNOWN {
            return c;
        }
        let c = token_class(el);
        cell.set(c);
        c
    }

    /// 元素的 class/id 特征位（见 [`crate::features`]），带缓存；文本节点为 0。
    pub(crate) fn features(&self, node: &NodeR<'_>) -> u32 {
        let Some(el) = node.as_element() else {
            return 0;
        };
        let cell = &self.features[node.id().index()];
        let f = cell.get();
        if f != u32::MAX {
            return f;
        }
        let f = crate::features::compute(el);
        cell.set(f);
        f
    }

    /// class/id 是评论区。
    pub(crate) fn is_comments(&self, node: &NodeR<'_>) -> bool {
        self.features(node) & crate::features::COMMENTS != 0
    }

    /// 打开或关掉文章模式（清掉判断缓存）。
    /// `root_chars` 是文章主体的非链接文字数，用来给严格规则设上限。
    pub(crate) fn set_strict(&mut self, strict: bool, root_chars: usize) {
        self.strict = strict;
        self.strict_max_chars = (root_chars as f64 * STRICT_MAX_SHARE) as usize;
        self.cache.iter().for_each(|c| c.set(0));
    }

    /// 子树里不在链接里的可见文字数：正文的量，菜单和链接列表不算。
    pub(crate) fn content_chars(&self, node: &NodeR<'_>) -> usize {
        self.visible
            .get(node.id().index())
            .map_or(0, |v| v.chars.saturating_sub(v.link_chars))
    }

    /// 页面可见文字的总字符数（只排除硬跳过的内容）。
    pub(crate) fn total_chars(&self) -> usize {
        self.total_chars
    }

    /// 放宽软跳过规则，用于正文被删过头时的重试。
    pub(crate) fn relax(&mut self, level: Relax) {
        self.relaxed = level;
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
        if is_non_content_tag(tag) && !is_text_button(node) {
            return Some("non-content tag");
        }
        if is_hidden(node) {
            return Some("hidden");
        }
        if !self.only_main {
            return None;
        }
        if matches!(tag, Tag::Nav | Tag::Aside | Tag::Footer) && !self.landmark_is_content(node) {
            return Some("nav/aside/footer");
        }
        if tag == Tag::A {
            return is_skip_link(node, el).then_some("skip link");
        }
        // 页面标明的正文容器（itemprop="articleBody"、article-body、post-content……）本身不按
        // 文章模式的规则删：正文本身可能就是一串链接（目录、合集）
        let declared_body = self.strict
            && (self.features(node) & crate::features::ANY_TIER != 0
                || el
                    .attr("itemprop")
                    .is_some_and(|v| v.eq_ignore_ascii_case("articleBody")));
        if self.strict && !declared_body {
            if crate::article::is_cleaned_tag(tag) {
                return Some("article: cleaned tag");
            }
            if crate::article::link_dense(node, self) {
                return Some("article: link dense");
            }
        }
        // 页面自己标出的正文区域（`<main>`、`role="main"`）不用软规则删
        if self.relaxed == Relax::All
            || matches!(tag, Tag::Body | Tag::Html)
            || is_main_landmark(el)
        {
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
        let dense = vis.links >= LINK_BLOCK_MIN_LINKS
            && vis.link_chars as f64 >= vis.chars as f64 * LINK_BLOCK_MIN_SHARE;
        // 链接文字长（整句标题）或者带图的链接列表是内容卡片，首页和列表页上要留着；
        // 文章模式里它们是指向别的文章的推荐，不是这篇文章的正文
        let link_block = dense
            && (self.strict
                || ((vis.link_chars as f64 / vis.links as f64) <= LINK_BLOCK_MAX_AVG_CHARS
                    && (vis.img_links as usize) * 2 < vis.links));
        // <main> 里的链接列表多半是内容本身（目录页、相册、分类列表），<article> 里的是文章的
        // 标签、分类、目录，都不当菜单删。`role="main"` 常标在整页的外层容器上，只在放宽规则时
        // 才这样对待（文档首页的目录）
        let in_relaxed_main =
            |e: Element<'_>| self.relaxed == Relax::LinkBlocks && e.attr("role") == Some("main");
        let strict_small =
            self.strict && vis.chars.saturating_sub(vis.link_chars) <= self.strict_max_chars;
        // 链接密集的块不可能是正文本身，文章模式里不受大小上限限制
        if link_block
            && !declared_body
            && (self.strict
                || !node.ancestors().any(|a| {
                    matches!(a.tag(), Some(Tag::Main | Tag::Article))
                        || a.as_element().is_some_and(in_relaxed_main)
                }))
        {
            Some("link block")
        } else if self.relaxed == Relax::LinkBlocks {
            None
        } else if tag == Tag::Header
            && !in_content_area(node)
            && node.descendants().any(|d| d.tag() == Some(Tag::Nav))
        {
            Some("header with nav")
        } else if el.attr("role").is_some_and(|r| SKIP_ROLES.contains(&r)) {
            Some("skip role")
        } else if self.classes(node, el) & (NEG | POS) == NEG {
            Some("negative class/id")
        } else if strict_small
            && !declared_body
            && (self.classes(node, el) & STRICT != 0
                || crate::article::is_furniture(el, self.features(node)))
        {
            Some("article furniture")
        } else {
            None
        }
    }

    /// `<nav>`、`<aside>`、`<footer>` 不当页面框架的两种情况：装着全页大部分文字、链接又不多的
    /// （有的网站把整页包在 `<aside>` 里）；`<article>` 里的 `<nav>`、`<footer>`（文章目录、
    /// 署名和标签）。后者照样过软跳过规则。`<article>` 里的 `<aside>` 多是"相关文章"，仍然去掉。
    fn landmark_is_content(&self, node: &NodeR<'_>) -> bool {
        let v = self
            .visible
            .get(node.id().index())
            .copied()
            .unwrap_or_default();
        if v.chars as f64 >= self.total_chars as f64 * WRAPPER_SHARE
            && (v.link_chars as f64) < v.chars as f64 * WRAPPER_MAX_LINK_SHARE
        {
            return true;
        }
        node.tag() != Some(Tag::Aside) && node.ancestors().any(|a| a.tag() == Some(Tag::Article))
    }

    /// 元素自身的可见文字数（只算未被硬跳过的部分；只取正文时才有）。
    pub(crate) fn visible_chars(&self, node: &NodeR<'_>) -> usize {
        self.visible.get(node.id().index()).map_or(0, |v| v.chars)
    }

    /// 链接统计：（有文字的链接数，其中短链接数，子树里的图片数）。
    pub(crate) fn link_stats(&self, node: &NodeR<'_>) -> (usize, usize, usize) {
        let v = self
            .visible
            .get(node.id().index())
            .copied()
            .unwrap_or_default();
        (
            v.text_links as usize,
            v.short_links as usize,
            v.images as usize,
        )
    }

    /// 可见文字数、链接文字数、链接数。诊断用。
    pub(crate) fn stats(&self, node: &NodeR<'_>) -> (usize, usize, usize) {
        let v = self
            .visible
            .get(node.id().index())
            .copied()
            .unwrap_or_default();
        (v.chars, v.link_chars, v.links)
    }

    pub(crate) fn only_main(&self) -> bool {
        self.only_main
    }

    pub(crate) fn is_relaxed(&self) -> bool {
        self.relaxed != Relax::None
    }
}
