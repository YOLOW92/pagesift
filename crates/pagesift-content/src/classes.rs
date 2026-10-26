//! 类名与标识符语义分类词典：正文、框架、跳转与严格文章装饰词判定。

use pagesift_html::Element;

pub(crate) const POS: u8 = 1;
pub(crate) const NEG: u8 = 2;
pub(crate) const JUMP: u8 = 4;
pub(crate) const STRICT: u8 = 8;
pub(crate) const MAX_TOKEN: usize = 16;

pub(crate) const SKIP_ROLES: &[&str] = &[
    "navigation",
    "banner",
    "contentinfo",
    "complementary",
    "search",
    "dialog",
    "alertdialog",
];

/// 一个小写词属于哪些类。按长度和字节模式快速匹配。
pub(crate) fn word_class(w: &str) -> u8 {
    match w {
        "article" | "content" | "entry" | "main" | "post" | "text" | "blog" | "story"
        | "markdown" | "prose" | "doc" | "docs" | "documentation" | "body" => POS,
        "nav" | "navbar" | "navigation" | "menu" | "footer" | "sidebar" | "aside" | "share"
        | "sharing" | "social" | "related" | "recommended" | "recent" | "cookie" | "cookies"
        | "consent" | "advert" | "advertisement" | "ad" | "ads" | "sponsor" | "sponsored"
        | "newsletter" | "subscribe" | "popup" | "modal" | "promo" | "toolbar" | "pagination"
        | "editsection" | "catlinks" | "bookmark" | "bookmarks" | "extra" | "labs" | "labstabs"
        // `excerpt` 不算：博客列表的摘要、文章导语是正文（WCXB listing +0.014）。`teaser` 保留：
        // 商品页的评论预览（`product-reviews-teaser`）是噪声，去掉它 product −0.009。
        | "endorser" | "endorsers" | "recommender" | "recommenders" | "quickview" | "teaser" => NEG,
        "skip" | "jump" => NEG | JUMP,
        "comment" | "comments" | "byline" | "tags" | "tag" | "breadcrumb" | "breadcrumbs"
        | "author" | "disqus" | "rating" | "print" | "follow" | "outbrain" | "taboola" | "embed" => {
            STRICT
        }
        _ => 0,
    }
}

/// 署名区（`authors`、`author-list`）。论文站的作者行是纯链接列表，链接密度规则会把它
/// 当导航剪掉；作者是正文的一部分，保留。
///
/// 按整个 class/id 词元匹配，不做子串匹配：`authors-widget`、`related-authors`、
/// `post-authors-box` 这类侧栏组件不算（0.72.8 子串匹配把它们全放进了正文）。
pub(crate) fn is_author_list(el: Element<'_>) -> bool {
    [el.attr("class"), el.attr("id")]
        .into_iter()
        .flatten()
        .flat_map(str::split_ascii_whitespace)
        .any(|token| {
            [
                "authors",
                "author-list",
                "authors-list",
                "author_list",
                "authorlist",
                "ltx_authors",
            ]
            .iter()
            .any(|name| token.eq_ignore_ascii_case(name))
        })
}

/// 只给辅助技术看的文字（`VisuallyHidden`、`sr-only`、`screen-reader-text`）。正文模式里
/// 这些文字对普通读者不可见，常常是"Repository navigation"这类区块标题。
pub(crate) fn is_visually_hidden(el: Element<'_>) -> bool {
    el.attr("class").is_some_and(|class| {
        [
            "visuallyhidden",
            "visually-hidden",
            "sr-only",
            "screen-reader-text",
            "screenreader",
        ]
        .iter()
        .any(|needle| contains_ascii_ci(class, needle))
    })
}

/// 不分配内存的 ASCII 大小写不敏感子串判断。`reason()` 会对每个元素调用，不能在这里
/// `to_ascii_lowercase()` 分配。
pub(crate) fn contains_ascii_ci(haystack: &str, needle: &str) -> bool {
    let (haystack, needle) = (haystack.as_bytes(), needle.as_bytes());
    !needle.is_empty()
        && needle.len() <= haystack.len()
        && haystack
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
}

/// 在 `<main>`、`<article>` 或 `role="main"` 里面：这里的页头、导航是正文的一部分。
pub(crate) fn in_content_area(node: &crate::dom::NodeR<'_>) -> bool {
    node.ancestors().any(|a| {
        a.as_element().is_some_and(|e| {
            matches!(
                e.tag(),
                pagesift_html::Tag::Main | pagesift_html::Tag::Article
            ) || e.attr("role") == Some("main")
        })
    })
}

pub(crate) fn is_main_landmark(el: Element<'_>) -> bool {
    el.tag() == pagesift_html::Tag::Main || el.attr("role") == Some("main")
}
pub(crate) fn token_class(el: Element<'_>) -> u8 {
    let mut class = 0;
    let mut buf = [0u8; MAX_TOKEN];
    for value in [el.attr("class"), el.attr("id")].into_iter().flatten() {
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
