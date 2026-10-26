//! 只取正文时的跳过规则：去掉导航、页眉页脚、侧边栏、链接密集块这类页面框架。

use std::cell::Cell;

use pagesift_html::{Element, Html, Tag};

use crate::dom::{NodeR, element, is_hidden, is_non_content_tag, short_text};
use crate::markdown::is_block_tag;

use crate::classes::{
    JUMP, NEG, POS, SKIP_ROLES, STRICT, in_content_area, is_main_landmark, token_class,
};

/// 正文被删过头时放宽规则的程度。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Relax {
    None,
    /// 软跳过规则只留链接块：菜单、页脚链接还删，按类名、role、页头删的块放回来。
    LinkBlocks,
    /// 只保留硬跳过和 `<nav>`、`<aside>`、`<footer>`。
    All,
    /// 在 `All` 的基础上连隐藏元素也保留：服务端渲染的单页应用常把整页内容包在 `display:none` 里
    /// 等脚本显示。只在前面几级的结果几乎为空时才用。
    Hidden,
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
    /// 页面里可能有公式（源码里出现过 math、MathJax、mjx-）：没有时渲染器不逐个元素检查公式。
    has_math: bool,
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
    attachments: Vec<Cell<u8>>,
    order: Vec<pagesift_html::NodeId>,
}

/// Reusable per-worker vectors indexed by DOM node id.
#[derive(Default)]
pub(crate) struct Scratch {
    visible: Vec<Visible>,
    has_block: Vec<bool>,
    cache: Vec<Cell<u8>>,
    classes: Vec<Cell<u8>>,
    features: Vec<Cell<u32>>,
    attachments: Vec<Cell<u8>>,
    order: Vec<pagesift_html::NodeId>,
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
fn compute(
    html: &Html<'_>,
    only_main: bool,
    mut visible: Vec<Visible>,
    mut has_block: Vec<bool>,
    mut order: Vec<pagesift_html::NodeId>,
) -> (Vec<Visible>, Vec<bool>, Vec<pagesift_html::NodeId>) {
    let n = html.len();
    order.clear();
    order.extend(
        html.root()
            .descendants()
            .filter(|d| d.is_element())
            .map(|node| node.id()),
    );
    visible.clear();
    if only_main {
        visible.resize(n, Visible::default());
    }
    has_block.clear();
    has_block.resize(n, false);
    for id in order.iter().rev() {
        let node = html.get(*id);
        let id = node.id().index();
        let hard = only_main && is_hard_skip(&node);
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
    (visible, has_block, order)
}

impl Skipper {
    pub(crate) fn new_with_scratch(
        html: &Html<'_>,
        only_main: bool,
        scratch: &mut Scratch,
    ) -> Self {
        let body = html.body();
        let (visible, has_block, order) = compute(
            html,
            only_main,
            std::mem::take(&mut scratch.visible),
            std::mem::take(&mut scratch.has_block),
            std::mem::take(&mut scratch.order),
        );
        let total = visible.get(body.id().index()).map_or(0, |v| v.chars);
        let n = html.len();
        let mut cache = std::mem::take(&mut scratch.cache);
        cache.clear();
        cache.resize_with(n, || Cell::new(0));
        let mut classes = std::mem::take(&mut scratch.classes);
        classes.clear();
        classes.resize_with(n, || Cell::new(UNKNOWN));
        let mut features = std::mem::take(&mut scratch.features);
        features.clear();
        features.resize_with(n, || Cell::new(u32::MAX));
        let mut attachments = std::mem::take(&mut scratch.attachments);
        attachments.clear();
        attachments.resize_with(n, || Cell::new(0));
        Self {
            only_main,
            strict: false,
            strict_max_chars: 0,

            relaxed: Relax::None,
            has_math: true,
            total_chars: total,
            protect_chars: ((total as f64 * PROTECT_SHARE) as usize).max(PROTECT_MIN_CHARS),
            visible,
            has_block,
            cache,
            classes,
            features,
            attachments,
            order,
        }
    }

    pub(crate) fn recycle(self, scratch: &mut Scratch) {
        if [
            self.visible.capacity(),
            self.has_block.capacity(),
            self.cache.capacity(),
            self.classes.capacity(),
            self.features.capacity(),
            self.attachments.capacity(),
            self.order.capacity(),
        ]
        .into_iter()
        .max()
        .unwrap_or(0)
            > 16 * 1024
        {
            *scratch = Scratch::default();
            return;
        }
        scratch.visible = self.visible;
        scratch.has_block = self.has_block;
        scratch.cache = self.cache;
        scratch.classes = self.classes;
        scratch.features = self.features;
        scratch.attachments = self.attachments;
        scratch.order = self.order;
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
        if is_hidden(node) && self.relaxed != Relax::Hidden {
            return Some("hidden");
        }
        if !self.only_main {
            return None;
        }
        // 署名区是正文：作者行通常是纯链接，不能当链接墙剪掉。
        if crate::classes::is_author_list(el) {
            return None;
        }
        if crate::classes::is_visually_hidden(el) {
            return Some("visually hidden");
        }
        if matches!(tag, Tag::Nav | Tag::Aside | Tag::Footer) && !self.landmark_is_content(node) {
            return Some("nav/aside/footer");
        }
        if tag == Tag::A {
            return is_skip_link(node, el).then_some("skip link");
        }
        // An embedded quotation is editorial content. Only protect compact
        // wrappers consisting of a quote and its loader, never a recommendation
        // container that merely happens to contain a quote somewhere below it.
        if matches!(tag, Tag::Div | Tag::Section)
            && el
                .attr("class")
                .is_some_and(|s| s.to_ascii_lowercase().contains("embed"))
            && node.children().any(|n| n.tag() == Some(Tag::Blockquote))
            && node.children().all(|n| {
                n.is_comment()
                    || n.as_text().is_some_and(|t| t.trim().is_empty())
                    || matches!(n.tag(), Some(Tag::Blockquote | Tag::Script))
            })
        {
            return None;
        }
        // 页面标明的正文容器（itemprop="articleBody"、article-body、post-content……）本身不按
        // 文章模式的规则删：正文本身可能就是一串链接（目录、合集）
        let declared_body = self.strict
            && (self.features(node) & crate::features::ANY_TIER != 0
                || el
                    .attr("itemprop")
                    .is_some_and(|v| v.eq_ignore_ascii_case("articleBody")));
        let attachment = self.strict && self.editorial_attachment(node);
        if self.strict && !declared_body && !attachment {
            if crate::editorial::recommendation_list(node) {
                return Some("article: attributed recommendation list");
            }
            if crate::article::is_cleaned_tag(tag) {
                return Some("article: cleaned tag");
            }
            if crate::article::link_dense(node, self) {
                return Some("article: link dense");
            }
        }
        // 页面自己标出的正文区域（`<main>`、`role="main"`）不用软规则删
        if matches!(self.relaxed, Relax::All | Relax::Hidden)
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
            && !attachment
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
            && !attachment
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
        // 正文区里的 `<aside>` 不一定都是"相关文章"：食谱的配料表、教程的提示框、文章的资料框
        // 常用 `<aside>`。名字带负面词（sidebar/related/ad……）或链接密集的照旧当框架去掉。
        if node.tag() == Some(Tag::Aside)
            && in_content_area(node)
            && v.chars >= 20
            && (v.link_chars as f64) < v.chars as f64 * 0.5
            && node
                .as_element()
                .is_none_or(|el| self.classes(node, el) & NEG == 0)
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

    pub(crate) fn editorial_attachment(&self, node: &NodeR<'_>) -> bool {
        let cache = &self.attachments[node.id().index()];
        if cache.get() == 0 {
            cache.set(if crate::editorial::attachment(node) {
                1
            } else {
                2
            });
        }
        cache.get() == 1
    }

    pub(crate) fn only_main(&self) -> bool {
        self.only_main
    }

    pub(crate) fn set_has_math(&mut self, v: bool) {
        self.has_math = v;
    }

    pub(crate) fn has_math(&self) -> bool {
        self.has_math
    }

    pub(crate) fn relaxed(&self) -> Relax {
        self.relaxed
    }

    pub(crate) fn is_relaxed(&self) -> bool {
        self.relaxed != Relax::None
    }
}
