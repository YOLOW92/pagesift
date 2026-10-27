//! 家具页和内容集群的识别：新闻、活动、关于、法律页脚、搜索、标签、带日期的路径。
//!
//! 这些页面在任何查询下都几乎不是答案，但它们的导航和页脚照样含着查询词，不降权就会靠
//! "凑词"挤掉真正的内容页（159 校基线里 46 所零分校有 12 所是新闻集群、9 所是关于/治理集群）。
//! 规则只看 URL 路径段和锚文本，不看查询，所以对任何任务都成立。

use pagesift_net::Url;

/// 家具页链接的分数乘以它。
pub(crate) const FURNITURE_FACTOR: f32 = 0.2;
/// 按日期归档的文章（叶子）的分数乘数：比一般家具页再低一半，它通不到别的页面。
pub(crate) const DATED_FACTOR: f32 = 0.1;
/// 枢纽页（链接比正文多）作为父页传给链接的相关度乘以它。
pub(crate) const HUB_FACTOR: f32 = 0.5;

/// 路径段或锚文本里的词命中就算家具（小写比较，整词）。
const WORDS: &[&str] = &[
    // 新闻、活动、媒体
    "news",
    "changelog",
    "release-notes",
    "whats-new",
    "newsroom",
    "press",
    "media",
    "blog",
    "blogs",
    "stories",
    "story",
    "events",
    "event",
    "calendar",
    "announcements",
    "magazine",
    "podcast",
    "podcasts",
    "video",
    "videos",
    "gallery",
    "photos",
    "spotlight",
    "features",
    // 分类和检索
    "tag",
    "tags",
    "category",
    "categories",
    "topic",
    "topics",
    "search",
    "archive",
    "archives",
    "feed",
    "rss",
    "sitemap",
    "site-map",
    // 关于、治理、品牌、荣誉
    "about",
    "history",
    "leadership",
    "governance",
    "president",
    "chancellor",
    "provost",
    "board",
    "trustees",
    "mission",
    "vision",
    "values",
    "strategy",
    "brand",
    "awards",
    "honors",
    "honours",
    "rankings",
    "accolades",
    "facts",
    "figures",
    // 法律和页脚
    "privacy",
    "accessibility",
    "legal",
    "terms",
    "cookies",
    "cookie",
    "disclaimer",
    "copyright",
    "contact",
    "contacts",
    "directions",
    "maps",
    "map",
    "directory",
    // 账号和交易
    "login",
    "logout",
    "signin",
    "sign-in",
    "account",
    "cart",
    "checkout",
    // 招聘、捐赠、校友
    "careers",
    "jobs",
    "vacancies",
    "giving",
    "donate",
    "donations",
    "alumni",
    // 展览、纪念、人物
    "exhibition",
    "exhibitions",
    "exhibit",
    "museum",
    "anniversary",
    "memorial",
    "people",
    "staff",
    "profiles",
    "profile",
    "obituaries",
    // 技术路径
    ".well-known",
    "wp-json",
    "wp-admin",
    "cgi-bin",
    "api",
];

fn is_word(s: &str) -> bool {
    let s = s.to_ascii_lowercase();
    WORDS.contains(&s.as_str())
}

/// 年份段后面跟着月份段（`/2026/05/`），或者单独的年份段：新闻和博客的归档路径。
fn is_date_path(segs: &[&str]) -> bool {
    let year = |s: &str| {
        s.len() == 4 && s.starts_with(['1', '2']) && s.bytes().all(|b| b.is_ascii_digit())
    };
    let month = |s: &str| (1..=2).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit());
    segs.windows(2).any(|w| year(w[0]) && month(w[1])) || segs.iter().any(|s| year(s))
}

/// 按日期归档的路径（`/news/2026/05/story/`、`/blog/2026/`）：新闻稿、博客、纪要这类叶子文章。
pub(crate) fn is_dated_url(u: &Url) -> bool {
    let segs: Vec<&str> = u.path().split('/').filter(|s| !s.is_empty()).collect();
    is_date_path(&segs)
}

/// URL 是不是家具页：任一路径段是家具词（含 `news-and-events` 这类连字符组合），或者带日期路径。
/// `wanted` 说一个词是不是查询本身要找的（查 "api" 时 `/api/` 不是家具）。
pub(crate) fn is_furniture_url(u: &Url, wanted: &dyn Fn(&str) -> bool) -> bool {
    let segs: Vec<&str> = u.path().split('/').filter(|s| !s.is_empty()).collect();
    if is_date_path(&segs) {
        return true;
    }
    let hit = |w: &str| is_word(w) && !wanted(w);
    segs.iter().any(|seg| {
        let stem = seg.rsplit_once('.').map_or(*seg, |(stem, _)| stem);
        hit(seg) || hit(stem) || stem.split(['-', '_']).any(hit)
    })
}

/// 锚文本是不是家具：很短（最多 3 个词）而且有一个词是家具词（查询要找的词不算）。
/// 长锚文本（"International students celebrate at the spring festival"）由 URL 规则兜底。
pub(crate) fn is_furniture_anchor(anchor: &str, wanted: &dyn Fn(&str) -> bool) -> bool {
    let words: Vec<&str> = anchor
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    !words.is_empty() && words.len() <= 3 && words.iter().any(|w| is_word(w) && !wanted(w))
}

/// 枢纽页：链接多于正文的词。`links` 是页面上的链接数，`words` 是正文的词数。
pub(crate) fn is_hub(links: usize, words: usize) -> bool {
    links >= 8 && links * 2 > words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no(_: &str) -> bool {
        false
    }

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn news_about_legal_and_dated_paths_are_furniture() {
        for p in [
            "https://e.edu/news/2026/05/story/",
            "https://e.edu/about/history/",
            "https://e.edu/privacy/",
            "https://e.edu/news-and-events/x",
            "https://e.edu/blog/2024/",
            "https://e.edu/.well-known/openid",
            "https://e.edu/exhibition/item-3/",
            "https://e.edu/tag/students/",
        ] {
            assert!(is_furniture_url(&u(p), &no), "{p}");
        }
        for p in [
            "https://e.edu/",
            "https://e.edu/admissions/international/",
            "https://e.edu/study/degrees/",
            "https://iss.e.edu/visa/",
            "https://e.edu/prospective/tuition/",
        ] {
            assert!(!is_furniture_url(&u(p), &no), "{p}");
        }
    }

    #[test]
    fn short_anchors_with_a_furniture_word_are_furniture() {
        assert!(is_furniture_anchor("News", &no));
        assert!(is_furniture_anchor("Contact us", &no));
        assert!(is_furniture_anchor("Site map", &no));
        assert!(!is_furniture_anchor("International students", &no));
        assert!(!is_furniture_anchor(
            "International students celebrate at the spring festival",
            &no
        ));
        assert!(!is_furniture_anchor("", &no));
    }

    #[test]
    fn hubs_have_more_links_than_words() {
        assert!(is_hub(40, 60));
        assert!(!is_hub(5, 3));
        assert!(!is_hub(10, 400));
    }
}
