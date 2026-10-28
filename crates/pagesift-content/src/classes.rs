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
        // 下面的名字都含 "author"：值里没有它就不用切词比较（每个元素都要查）
        .filter(|value| contains_ascii_ci(value, "author"))
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
    const NEEDLES: [&str; 5] = [
        "visuallyhidden",
        "visually-hidden",
        "sr-only",
        "screen-reader-text",
        "screenreader",
    ];
    const FIRST: [bool; 256] = first_bytes(&NEEDLES);
    el.attr("class")
        .is_some_and(|class| contains_any_ascii_ci_with(class, &NEEDLES, &FIRST))
}

/// 各个词的首字母（大小写都算）：扫描时先查这张表，不是候选开头的字节直接跳过。
const fn first_bytes(needles: &[&str]) -> [bool; 256] {
    let mut t = [false; 256];
    let mut i = 0;
    while i < needles.len() {
        let b = needles[i].as_bytes()[0];
        t[b.to_ascii_lowercase() as usize] = true;
        t[b.to_ascii_uppercase() as usize] = true;
        i += 1;
    }
    t
}

/// `needles`（小写 ASCII）里有任何一个是 `haystack` 的子串（ASCII 大小写不敏感）。只扫一遍：
/// 在首字母相同的位置才比较，等价于对每个词调用 [`contains_ascii_ci`]。
#[cfg(test)]
pub(crate) fn contains_any_ascii_ci(haystack: &str, needles: &[&str]) -> bool {
    let first = needles.iter().fold([false; 256], |mut t, n| {
        if let Some(&b) = n.as_bytes().first() {
            t[usize::from(b.to_ascii_lowercase())] = true;
            t[usize::from(b.to_ascii_uppercase())] = true;
        }
        t
    });
    contains_any_ascii_ci_with(haystack, needles, &first)
}

/// [`contains_any_ascii_ci`]，首字母表（[`first_bytes`]）预先算好：`skip()` 对每个元素都调用，
/// class 常有上百个字节，每个字节只查一次表。
fn contains_any_ascii_ci_with(haystack: &str, needles: &[&str], first: &[bool; 256]) -> bool {
    let h = haystack.as_bytes();
    h.iter().enumerate().any(|(i, &c)| {
        first[usize::from(c)] && {
            let c = c.to_ascii_lowercase();
            needles.iter().any(|n| {
                let n = n.as_bytes();
                n.first() == Some(&c)
                    && h.len() - i >= n.len()
                    && h[i..i + n.len()].eq_ignore_ascii_case(n)
            })
        }
    })
}

/// 不分配内存的 ASCII 大小写不敏感子串判断。`reason()` 会对每个元素调用，不能在这里
/// `to_ascii_lowercase()` 分配。
pub(crate) fn contains_ascii_ci(haystack: &str, needle: &str) -> bool {
    let (haystack, needle) = (haystack.as_bytes(), needle.as_bytes());
    let Some(&first) = needle.first() else {
        return false;
    };
    if needle.len() > haystack.len() {
        return false;
    }
    // 首字母（大小写各一个）用 memchr2 跳着找，找到了再比整个词
    let last = haystack.len() - needle.len();
    memchr::memchr2_iter(
        first.to_ascii_lowercase(),
        first.to_ascii_uppercase(),
        &haystack[..=last],
    )
    .any(|i| haystack[i..i + needle.len()].eq_ignore_ascii_case(needle))
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

#[cfg(test)]
mod tests {
    use super::{contains_any_ascii_ci, contains_ascii_ci};

    #[test]
    fn contains_ascii_ci_matches_the_window_scan() {
        let reference = |h: &str, n: &str| {
            !n.is_empty()
                && n.len() <= h.len()
                && h.as_bytes()
                    .windows(n.len())
                    .any(|w| w.eq_ignore_ascii_case(n.as_bytes()))
        };
        const ATOMS: &[&str] = &[
            "a", "A", "au", "Author", "THOR", "-", "x", "é", "中", " ", "aut", "sr-",
        ];
        let mut seed = 0x51_7cc1_b727_220au64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            let h: String = (0..next() % 10)
                .map(|_| ATOMS[(next() % ATOMS.len() as u64) as usize])
                .collect();
            for n in ["author", "a", "-x", "sr-only", "", "thor", "é"] {
                assert_eq!(contains_ascii_ci(&h, n), reference(&h, n), "{h:?} {n:?}");
            }
        }
    }

    #[test]
    fn contains_any_matches_each_needle() {
        let needles = [
            "visuallyhidden",
            "visually-hidden",
            "sr-only",
            "screen-reader-text",
        ];
        for h in [
            "",
            "s",
            "SR-ONLY",
            "a VisuallyHidden b",
            "visually-hid",
            "x screen-reader-tex",
            "nav screen-reader-text",
            "é sr-onlyé",
            "ssssr-only",
        ] {
            let each = needles.iter().any(|n| contains_ascii_ci(h, n));
            assert_eq!(contains_any_ascii_ci(h, &needles), each, "{h:?}");
        }
    }
}
