//! 元素 class/id 的特征位：正文容器的层级、非正文标记（trafilatura）、Readability 的正负面词。
//!
//! 这些规律本来是正则（trafilatura 的 xpaths.py、Readability 的 REGEXPS），其实都是字面子串加上
//! 几种位置条件（开头、整个值、前后是空格、后面不是某个字母）。这里把它们展开成字面量，
//! 用一个 Aho-Corasick 自动机一遍扫出所有命中，再逐个检查位置条件，比正则引擎快得多；
//! 每个元素只算一次（结果缓存在 Skipper 里）。不区分大小写（Readability 本来就不区分；
//! trafilatura 列出的大小写变体合并成一个）。

use std::sync::LazyLock;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, AhoCorasickKind, MatchKind};
use pagesift_html::Element;

/// trafilatura 的正文容器第 0–2 层。
pub(crate) const TIER: [u32; 3] = [1, 1 << 1, 1 << 2];
pub(crate) const ANY_TIER: u32 = TIER[0] | TIER[1] | TIER[2];
/// trafilatura 的非正文标记（分享、作者、标签、相关推荐、广告、元信息、评论……）。
pub(crate) const DISCARD: u32 = 1 << 3;
/// Readability：像"不太可能是正文"，又没有正文容器词（`MAYBE`）。
pub(crate) const UNLIKELY: u32 = 1 << 4;
const MAYBE: u32 = 1 << 5;
/// Readability 的正面、负面词，class 和 id 分开（各加减一次分）。
pub(crate) const POS_CLASS: u32 = 1 << 6;
pub(crate) const NEG_CLASS: u32 = 1 << 7;
pub(crate) const POS_ID: u32 = 1 << 8;
pub(crate) const NEG_ID: u32 = 1 << 9;
/// 评论区（comment / comments / disqus，不含 commentary）。
pub(crate) const COMMENTS: u32 = 1 << 10;

/// 字面量在属性值里要满足的位置条件。
#[derive(Clone, Copy, PartialEq, Eq)]
enum At {
    /// 任意位置。
    Any,
    /// 属性值开头。
    Start,
    /// 整个属性值。
    Exact,
    /// 前面是开头或空格，后面是结尾、空格或 `-`（`(?:^| )ad(?: |-|$)`）。
    Word,
    /// 前后都是开头/结尾或空格（`^hid$| hid$| hid |^hid `）。
    Spaced,
    /// 后面不是 `a`（`comment` 但不是 `commentary`）。
    NotA,
    /// 开头，且后面不是 `a`。
    StartNotA,
}

/// 用在哪个属性上。
#[derive(Clone, Copy, PartialEq, Eq)]
enum On {
    Id,
    Class,
    Both,
}

struct Rule {
    lit: &'static str,
    at: At,
    on: On,
    /// 在 id 上命中设的位、在 class 上命中设的位。
    id_bit: u32,
    class_bit: u32,
}

const fn r(lit: &'static str, at: At, on: On, bit: u32) -> Rule {
    Rule {
        lit,
        at,
        on,
        id_bit: bit,
        class_bit: bit,
    }
}

/// Readability 的正负面词：id 和 class 设不同的位。
const fn rw(lit: &'static str, at: At, id_bit: u32, class_bit: u32) -> Rule {
    Rule {
        lit,
        at,
        on: On::Both,
        id_bit,
        class_bit,
    }
}

use At::*;
use On::*;

const T0: u32 = TIER[0];
const T1: u32 = TIER[1];
const T2: u32 = TIER[2];

static RULES: &[Rule] = &[
    // 正文容器第 0 层（trafilatura BODY_XPATH[0]）
    r("entry-content", Any, Both, T0),
    r("article-content", Any, Both, T0),
    r("art-content", Any, Both, T0),
    r("article__content", Any, Both, T0),
    r("articlebody", Any, Both, T0),
    r("article-body", Any, Both, T0),
    r("article__body", Any, Both, T0),
    r("body-text", Any, Both, T0),
    r("articlecontent", Exact, Id, T0),
    r("post", Exact, Class, T0),
    r("entry", Exact, Class, T0),
    r("post-text", Any, Class, T0),
    r("post_text", Any, Class, T0),
    r("post-body", Any, Class, T0),
    r("postentry", Any, Class, T0),
    r("post-entry", Any, Class, T0),
    r("postcontent", Any, Class, T0),
    r("post-content", Any, Class, T0),
    r("post_content", Any, Class, T0),
    r("post_inner_wrapper", Any, Class, T0),
    r("articletext", Any, Class, T0),
    r("article-text", Any, Class, T0),
    r("page-content", Any, Class, T0),
    r("text-content", Any, Class, T0),
    r("article__container", Any, Class, T0),
    // 第 1 层
    r("article", Exact, Id, T1),
    r("story", Exact, Both, T1),
    r("primary", Start, Id, T1),
    r("story-body", Any, Both, T1),
    r("article ", Start, Class, T1),
    r("postarea", Exact, Class, T1),
    r("art-postcontent", Exact, Class, T1),
    r("text", Exact, Class, T1),
    r("post-bodycopy", Any, Class, T1),
    r("storycontent", Any, Class, T1),
    r("story-content", Any, Class, T1),
    r("theme-content", Any, Class, T1),
    r("blog-content", Any, Class, T1),
    r("section-content", Any, Class, T1),
    r("single-content", Any, Class, T1),
    r("single-post", Any, Class, T1),
    r("main-column", Any, Class, T1),
    r("wpb_text_column", Any, Class, T1),
    r("field-body", Any, Class, T1),
    r("fulltext", Any, Class, T1),
    // 第 2 层
    r("content", Exact, Both, T2),
    r("content-main", Any, Id, T2),
    r("content-body", Any, Id, T2),
    r("contentbody", Any, Id, T2),
    r("main-content", Any, Both, T2),
    r("content-main", Any, Class, T2),
    r("content_main", Any, Class, T2),
    r("content-body", Any, Class, T2),
    r("content__body", Any, Class, T2),
    r("page-content", Any, Class, T2),
    // 非正文标记（trafilatura OVERALL_DISCARD_XPATH、TEASER、评论、图注）
    r("shar", Start, Both, DISCARD),
    r("social", Any, Both, DISCARD),
    r("viral", Any, Both, DISCARD),
    r("newsletter", Any, Both, DISCARD),
    r("syndication", Any, Both, DISCARD),
    r("tags", Any, Both, DISCARD),
    r("sidebar", Any, Both, DISCARD),
    r("banner", Any, Both, DISCARD),
    r("breadcrumb", Any, Both, DISCARD),
    r("bread-crumb", Any, Both, DISCARD),
    r("button", Any, Both, DISCARD),
    r("author", Any, Both, DISCARD),
    r("footer", Any, Both, DISCARD),
    r("teaser", Any, Both, DISCARD),
    r("share", Any, Id, DISCARD),
    r("nav", Any, Id, DISCARD),
    r("menu", Any, Id, DISCARD),
    r("related", Any, Id, DISCARD),
    r("message-container", Any, Both, DISCARD),
    r("premium", Any, Id, DISCARD),
    r("ad-", Start, Id, DISCARD),
    r("comment", NotA, Id, DISCARD),
    r("comol", Any, Id, DISCARD),
    r("disqus_thread", Any, Id, DISCARD),
    r("dsq-comments", Any, Id, DISCARD),
    r("nav", Start, Class, DISCARD),
    r("post-nav", Start, Class, DISCARD),
    r("subnav", Any, Class, DISCARD),
    r("avigation", Any, Class, DISCARD),
    r("navbar", Any, Class, DISCARD),
    r("navbox", Any, Class, DISCARD),
    r("menu", Any, Class, DISCARD),
    r("ad", Word, Class, DISCARD),
    r("-ad-", Any, Class, DISCARD),
    r("dfpad", Any, Class, DISCARD),
    r("outbrain", Any, Class, DISCARD),
    r("taboola", Any, Class, DISCARD),
    r("criteo", Any, Class, DISCARD),
    r("paid-content", Any, Class, DISCARD),
    r("paidcontent", Any, Class, DISCARD),
    r("byline", Any, Class, DISCARD),
    r("share-", Any, Class, DISCARD),
    r("sociable", Any, Class, DISCARD),
    r("embedded", Any, Class, DISCARD),
    r("tag-list", Any, Class, DISCARD),
    r("consent", Any, Class, DISCARD),
    r("elated", Any, Class, DISCARD),
    r("next-", Any, Class, DISCARD),
    r("-stories", Any, Class, DISCARD),
    r("most-popular", Any, Class, DISCARD),
    r("more-on", Any, Class, DISCARD),
    r("meta", Any, Class, DISCARD),
    r("rating", Any, Class, DISCARD),
    r("attachment", Any, Class, DISCARD),
    r("timestamp", Any, Class, DISCARD),
    r("user-info", Any, Class, DISCARD),
    r("user-profile", Any, Class, DISCARD),
    r("-icon", Any, Class, DISCARD),
    r("article-infos", Any, Class, DISCARD),
    r("slide", Any, Class, DISCARD),
    r("overlay", Any, Class, DISCARD),
    r("comment", StartNotA, Class, DISCARD),
    r("article-comments", Any, Class, DISCARD),
    r("post-comments", Any, Class, DISCARD),
    r("comments-title", Any, Class, DISCARD),
    r("caption", Any, Class, DISCARD),
    // 评论区
    r("comment", NotA, Both, COMMENTS),
    r("disqus", Any, Both, COMMENTS),
    // Readability unlikelyCandidates / okMaybeItsACandidate（class 和 id 合起来判断）
    r("-ad-", Any, Both, UNLIKELY),
    r("ai2html", Any, Both, UNLIKELY),
    r("banner", Any, Both, UNLIKELY),
    r("breadcrumbs", Any, Both, UNLIKELY),
    r("combx", Any, Both, UNLIKELY),
    r("comment", Any, Both, UNLIKELY),
    r("community", Any, Both, UNLIKELY),
    r("cover-wrap", Any, Both, UNLIKELY),
    r("disqus", Any, Both, UNLIKELY),
    r("extra", Any, Both, UNLIKELY),
    r("footer", Any, Both, UNLIKELY),
    r("gdpr", Any, Both, UNLIKELY),
    r("header", Any, Both, UNLIKELY),
    r("legends", Any, Both, UNLIKELY),
    r("menu", Any, Both, UNLIKELY),
    r("related", Any, Both, UNLIKELY),
    r("remark", Any, Both, UNLIKELY),
    r("replies", Any, Both, UNLIKELY),
    r("rss", Any, Both, UNLIKELY),
    r("shoutbox", Any, Both, UNLIKELY),
    r("sidebar", Any, Both, UNLIKELY),
    r("skyscraper", Any, Both, UNLIKELY),
    r("social", Any, Both, UNLIKELY),
    r("sponsor", Any, Both, UNLIKELY),
    r("supplemental", Any, Both, UNLIKELY),
    r("ad-break", Any, Both, UNLIKELY),
    r("agegate", Any, Both, UNLIKELY),
    r("pagination", Any, Both, UNLIKELY),
    r("pager", Any, Both, UNLIKELY),
    r("popup", Any, Both, UNLIKELY),
    r("yom-remote", Any, Both, UNLIKELY),
    r("and", Any, Both, MAYBE),
    r("article", Any, Both, MAYBE),
    r("body", Any, Both, MAYBE),
    r("column", Any, Both, MAYBE),
    r("content", Any, Both, MAYBE),
    r("main", Any, Both, MAYBE),
    r("shadow", Any, Both, MAYBE),
    // Readability positive
    rw("article", Any, POS_ID, POS_CLASS),
    rw("body", Any, POS_ID, POS_CLASS),
    rw("content", Any, POS_ID, POS_CLASS),
    rw("entry", Any, POS_ID, POS_CLASS),
    rw("hentry", Any, POS_ID, POS_CLASS),
    rw("h-entry", Any, POS_ID, POS_CLASS),
    rw("main", Any, POS_ID, POS_CLASS),
    rw("page", Any, POS_ID, POS_CLASS),
    rw("pagination", Any, POS_ID, POS_CLASS),
    rw("post", Any, POS_ID, POS_CLASS),
    rw("text", Any, POS_ID, POS_CLASS),
    rw("blog", Any, POS_ID, POS_CLASS),
    rw("story", Any, POS_ID, POS_CLASS),
    // Readability negative
    rw("-ad-", Any, NEG_ID, NEG_CLASS),
    rw("hidden", Any, NEG_ID, NEG_CLASS),
    rw("hid", Spaced, NEG_ID, NEG_CLASS),
    rw("banner", Any, NEG_ID, NEG_CLASS),
    rw("combx", Any, NEG_ID, NEG_CLASS),
    rw("comment", Any, NEG_ID, NEG_CLASS),
    rw("com-", Any, NEG_ID, NEG_CLASS),
    rw("contact", Any, NEG_ID, NEG_CLASS),
    rw("footer", Any, NEG_ID, NEG_CLASS),
    rw("gdpr", Any, NEG_ID, NEG_CLASS),
    rw("masthead", Any, NEG_ID, NEG_CLASS),
    rw("media", Any, NEG_ID, NEG_CLASS),
    rw("meta", Any, NEG_ID, NEG_CLASS),
    rw("outbrain", Any, NEG_ID, NEG_CLASS),
    rw("promo", Any, NEG_ID, NEG_CLASS),
    rw("related", Any, NEG_ID, NEG_CLASS),
    rw("scroll", Any, NEG_ID, NEG_CLASS),
    rw("share", Any, NEG_ID, NEG_CLASS),
    rw("shoutbox", Any, NEG_ID, NEG_CLASS),
    rw("sidebar", Any, NEG_ID, NEG_CLASS),
    rw("skyscraper", Any, NEG_ID, NEG_CLASS),
    rw("sponsor", Any, NEG_ID, NEG_CLASS),
    rw("shopping", Any, NEG_ID, NEG_CLASS),
    rw("tags", Any, NEG_ID, NEG_CLASS),
    rw("widget", Any, NEG_ID, NEG_CLASS),
];

static AC: LazyLock<AhoCorasick> = LazyLock::new(|| {
    AhoCorasickBuilder::new()
        .ascii_case_insensitive(true)
        .match_kind(MatchKind::Standard)
        // 规则只有两百来条，DFA 占的内存很小，重叠匹配比 NFA 快得多
        .kind(Some(AhoCorasickKind::DFA))
        .build(RULES.iter().map(|r| r.lit))
        .expect("feature patterns")
});

fn holds(at: At, v: &[u8], start: usize, end: usize) -> bool {
    let before = start.checked_sub(1).map(|i| v[i]);
    let after = v.get(end).copied();
    match at {
        Any => true,
        Start => start == 0,
        Exact => start == 0 && end == v.len(),
        Word => before.is_none_or(|c| c == b' ') && after.is_none_or(|c| c == b' ' || c == b'-'),
        Spaced => before.is_none_or(|c| c == b' ') && after.is_none_or(|c| c == b' '),
        NotA => after.is_none_or(|c| !c.eq_ignore_ascii_case(&b'a')),
        StartNotA => start == 0 && after.is_none_or(|c| !c.eq_ignore_ascii_case(&b'a')),
    }
}

fn scan(v: &str, attr: On, bits: &mut u32) {
    let bytes = v.as_bytes();
    for m in AC.find_overlapping_iter(v) {
        let rule = &RULES[m.pattern().as_usize()];
        if (rule.on == attr || rule.on == Both) && holds(rule.at, bytes, m.start(), m.end()) {
            *bits |= if attr == Id {
                rule.id_bit
            } else {
                rule.class_bit
            };
        }
    }
}

/// 元素的特征位。
pub(crate) fn compute(el: Element<'_>) -> u32 {
    let mut bits = 0;
    if let Some(v) = el.attr("id").filter(|v| !v.is_empty()) {
        scan(v, Id, &mut bits);
    }
    if let Some(v) = el.attr("class").filter(|v| !v.is_empty()) {
        scan(v, Class, &mut bits);
    }
    if bits & MAYBE != 0 {
        bits &= !UNLIKELY;
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(html: &str) -> u32 {
        let dom = pagesift_html::Html::parse(html);
        let el = dom
            .root()
            .descendants()
            .find_map(|n| n.as_element().filter(|e| e.name() == "div"))
            .unwrap();
        compute(el)
    }

    #[test]
    fn anchors_and_attributes() {
        assert_ne!(bits(r#"<div class="entry-content clearfix">"#) & TIER[0], 0);
        assert_ne!(bits(r#"<div class="post">"#) & TIER[0], 0);
        assert_eq!(
            bits(r#"<div class="post-list">"#) & TIER[0],
            0,
            "only the exact value `post`"
        );
        assert_ne!(bits(r#"<div class="top ad sticky">"#) & DISCARD, 0);
        assert_eq!(
            bits(r#"<div class="header-adaptive">"#) & DISCARD,
            0,
            "`ad` must be a word"
        );
        assert_ne!(bits(r#"<div id="comment-123">"#) & DISCARD, 0);
        assert_eq!(
            bits(r#"<div id="commentary">"#) & DISCARD,
            0,
            "commentary is content"
        );
        assert_ne!(bits(r#"<div class="sidebar">"#) & UNLIKELY, 0);
        assert_eq!(
            bits(r#"<div class="sidebar main">"#) & UNLIKELY,
            0,
            "maybe-words cancel unlikely"
        );
        let b = bits(r#"<div id="main" class="share-bar">"#);
        assert_ne!(b & POS_ID, 0);
        assert_ne!(b & NEG_CLASS, 0);
        assert_eq!(b & NEG_ID, 0);
    }
}
