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
        | "editsection" | "catlinks" => NEG,
        "skip" | "jump" => NEG | JUMP,
        "comment" | "comments" | "byline" | "tags" | "tag" | "breadcrumb" | "breadcrumbs"
        | "author" | "authors" | "disqus" | "rating" | "print" | "follow" | "outbrain"
        | "taboola" | "embed" => STRICT,
        _ => 0,
    }
}

/// 在 `<main>`、`<article>` 或 `role="main"` 里面：这里的页头、导航是正文的一部分。
pub(crate) fn in_content_area(node: &crate::dom::NodeR<'_>) -> bool {
    node.ancestors().any(|a| {
        a.as_element().is_some_and(|e| {
            matches!(e.tag(), pagesift_html::Tag::Main | pagesift_html::Tag::Article) || e.attr("role") == Some("main")
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
