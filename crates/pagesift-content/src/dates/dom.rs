use std::sync::LazyLock;

use aho_corasick::AhoCorasick;
use pagesift_html::Tag;
use regex::Regex;

use crate::dom::{NodeR, element};

use super::{Bounds, Date, MAX_SEGMENT, YEAR, civil_from_days, date_expr, examine_text, parse_iso};

// ---------------------------------------------------------------------------
// 页面上的检索

pub(super) static URL_DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"\D({YEAR})[/_-]([0-1]?[0-9])[/_-]([0-3]?[0-9])(?:\D|$)"
    ))
    .unwrap()
});

/// URL 路径里不带分隔符的 `/20200428/`（前后是 `/`、`-`、`_`、`.` 或结尾，长 ID 不算；自研）。
pub(super) static URL_DATE_COMPACT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"/({YEAR})(0[1-9]|1[0-2])([0-3][0-9])(?:[/_.-]|$)"
    ))
    .unwrap()
});

pub(super) fn url_date(url: &str, b: &Bounds) -> Option<Date> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let from = |re: &Regex, s: &str| {
        let c = re.captures(s)?;
        b.check(Date::ymd(
            c[1].parse().ok()?,
            c[2].parse().ok()?,
            c[3].parse().ok()?,
        ))
    };
    from(&URL_DATE, url).or_else(|| from(&URL_DATE_COMPACT, path))
}

/// 表示发布日期的 meta 名字（htmldate 的 DATE_ATTRIBUTES）。
pub(super) const DATE_ATTRIBUTES: &[&str] = &[
    "analyticsattributes.articledate",
    "article.created",
    "article_date_original",
    "article:post_date",
    "article.published",
    "article:published",
    "article:published_date",
    "article:published_time",
    "article:publicationdate",
    "bt:pubdate",
    "citation_date",
    "citation_publication_date",
    "content_create_date",
    "created",
    "cxenseparse:recs:publishtime",
    "date",
    "date_created",
    "date_published",
    "datecreated",
    "dateposted",
    "datepublished",
    "dc.date",
    "dc.created",
    "dc.date.created",
    "dc.date.issued",
    "dc.date.publication",
    "dcsext.articlefirstpublished",
    "dcterms.created",
    "dcterms.date",
    "dcterms.issued",
    "dc:created",
    "dc:date",
    "displaydate",
    "doc_date",
    "field-name-post-date",
    "gentime",
    "mediator_published_time",
    "meta",
    "og:article:published",
    "og:article:published_time",
    "og:datepublished",
    "og:pubdate",
    "og:publish_date",
    "og:published_time",
    "og:question:published_time",
    "og:regdate",
    "originalpublicationdate",
    "parsely-pub-date",
    "pdate",
    "ptime",
    "pubdate",
    "publishdate",
    "publish_date",
    "publish_time",
    "publish-date",
    "published-date",
    "published_date",
    "published_time",
    "publisheddate",
    "publication_date",
    "rbpubdate",
    "release_date",
    "rnews:datepublished",
    "sailthru.date",
    "shareaholic:article_published_time",
    "timestamp",
    "twt-published-at",
    "video:release_date",
    "vr:published_time",
];
pub(super) const NAME_MODIFIED: &[&str] = &[
    "lastdate",
    "lastmod",
    "lastmodified",
    "last-modified",
    "modified",
    "utime",
];
pub(super) const PROPERTY_MODIFIED: &[&str] = &[
    "article:modified",
    "article:modified_date",
    "article:modified_time",
    "article:post_modified",
    "bt:moddate",
    "datemodified",
    "dc.modified",
    "dcterms.modified",
    "lastmodified",
    "modified_time",
    "modificationdate",
    "og:article:modified_time",
    "og:modified_time",
    "og:updated_time",
    "release_date",
    "revision_date",
    "updated_time",
];

/// htmldate 的 examine_header（原始发布日期模式）。
pub(super) fn examine_header(metas: &[NodeR<'_>], b: &Bounds) -> Option<Date> {
    let mut reserve: Option<Date> = None;
    for node in metas {
        let Some(el) = element(node) else { continue };
        let content = el.attr("content");
        if content.is_none() && el.attr("datetime").is_none() {
            continue;
        }
        let tryf = |s: Option<&str>| s.and_then(|s| date_expr(s, b));
        let mut found = None;
        if let Some(name) = el.attr("name") {
            let a = name.to_ascii_lowercase();
            if a == "og:url" {
                reserve = content.and_then(|c| url_date(c, b)).or(reserve);
            } else if DATE_ATTRIBUTES.contains(&a.as_str()) {
                found = tryf(content);
            } else if NAME_MODIFIED.contains(&a.as_str()) {
                reserve = tryf(content).or(reserve);
            }
        } else if let Some(prop) = el.attr("property") {
            let a = prop.to_ascii_lowercase();
            if DATE_ATTRIBUTES.contains(&a.as_str()) {
                found = tryf(content);
            } else if PROPERTY_MODIFIED.contains(&a.as_str()) {
                reserve = tryf(content).or(reserve);
            }
        } else if let Some(ip) = el.attr("itemprop") {
            let a = ip.to_ascii_lowercase();
            if matches!(a.as_str(), "datecreated" | "datepublished" | "pubyear") {
                found = tryf(el.attr("datetime").or(content));
            } else if a == "copyrightyear"
                && let Some(y) = content.and_then(|c| c.trim().parse::<i32>().ok())
                && let Some(d) = b.check(Date::ymd(y, 1, 1))
            {
                reserve = Some(d);
            }
        } else if el
            .attr("pubdate")
            .is_some_and(|p| p.eq_ignore_ascii_case("pubdate"))
        {
            found = tryf(content);
        } else if let Some(he) = el.attr("http-equiv") {
            let a = he.to_ascii_lowercase();
            if a == "date" {
                found = tryf(content);
            } else if a == "last-modified" {
                reserve = tryf(content).or(reserve);
            }
        }
        if found.is_some() {
            return found;
        }
    }
    reserve
}

pub(super) static JSON_PUBLISHED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"(?i)"datePublished": ?"({YEAR}-[0-1]?[0-9]-[0-3]?[0-9](?:[T ][0-9]{{2}}:[0-9]{{2}}:[0-9]{{2}}(?:\.[0-9]+)?(?:Z|[+-][0-9]{{2}}:?[0-9]{{2}})?)?)"#
    ))
    .unwrap()
});

/// JSON-LD（和 `application/settings+json`）里第一个 `"datePublished"`。
pub(super) fn json_search(scripts: &[NodeR<'_>], b: &Bounds) -> Option<Date> {
    for node in scripts {
        let Some(el) = element(node) else { continue };
        let ty = el.attr("type").unwrap_or("").trim().to_ascii_lowercase();
        if ty != "application/ld+json" && ty != "application/settings+json" {
            continue;
        }
        let text: String = node
            .children()
            .filter_map(|c| c.as_text().map(str::to_string))
            .collect();
        if !text.contains("\"date") {
            continue;
        }
        if let Some(c) = JSON_PUBLISHED.captures(&text)
            && let Some(d) = b.check(parse_iso(&c[1], false))
        {
            return Some(d);
        }
    }
    None
}

/// 换算成 UTC 的秒数（只有带时区的才能换算）。
pub(super) fn instant(d: &Date) -> Option<i64> {
    let (h, mi, s, off) = d.time?;
    let off = off?;
    let days = days_from_civil(d.y, d.m, d.d);
    Some(days * 86_400 + i64::from(h * 3600 + mi * 60 + s) - i64::from(off))
}

pub(super) fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// meta 给的是 UTC 时刻，而 JSON-LD 给的是同一时刻的本地时间：用本地的，发布日期按发布方当地的那一天算（自研规则）。
pub(super) fn prefer_local(found: Option<Date>, other: impl Fn() -> Option<Date>) -> Option<Date> {
    let d = found?;
    if d.time.is_some_and(|t| t.3 == Some(0))
        && let Some(o) = other()
        && o.time.is_some_and(|t| t.3.is_some_and(|x| x != 0))
        && instant(&o) == instant(&d)
    {
        return Some(o);
    }
    Some(d)
}

pub(super) fn earliest(a: Option<Date>, c: Date) -> Option<Date> {
    Some(match a {
        Some(a) if a.key() <= c.key() => a,
        _ => c,
    })
}

pub(super) const MAX_CANDIDATES: usize = 1000;

/// 元素文本的开头部分：空白折叠后够 [`MAX_SEGMENT`] 个字符就停（后面的规则只看这么多），
/// 不用拼出整棵子树的文本（候选里常有整个页脚这样的大元素）。
pub(super) fn text_prefix(node: &NodeR<'_>) -> String {
    let mut out = String::new();
    let mut seen = 0usize;
    let mut in_ws = true;
    for d in node.descendants() {
        let Some(t) = d.as_text() else { continue };
        out.push_str(t);
        for c in t.chars() {
            if c.is_whitespace() {
                if !in_ws {
                    seen += 1;
                    in_ws = true;
                }
            } else {
                seen += 1;
                in_ws = false;
            }
        }
        if seen > MAX_SEGMENT + 1 {
            break;
        }
    }
    out
}

/// htmldate 的 examine_elements：候选元素的文本和 `title` 属性，第一个能解析的。
pub(super) fn examine_elements(nodes: &[NodeR<'_>], b: &Bounds) -> Option<Date> {
    if nodes.is_empty() || nodes.len() > MAX_CANDIDATES {
        return None;
    }
    nodes.iter().find_map(|n| {
        let text = text_prefix(n);
        examine_text(&text, b)
            .or_else(|| element(n)?.attr("title").and_then(|t| examine_text(t, b)))
    })
}

/// htmldate 的 examine_abbr_elements。
pub(super) fn examine_abbr(abbrs: &[NodeR<'_>], b: &Bounds) -> Option<Date> {
    if abbrs.is_empty() || abbrs.len() > MAX_CANDIDATES {
        return None;
    }
    let mut reference = None;
    for n in abbrs {
        let el = element(n)?;
        if let Some(ut) = el.attr("data-utime") {
            if let Ok(secs) = ut.trim().parse::<i64>() {
                let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
                let r = secs.rem_euclid(86_400) as u32;
                let date = Date {
                    y,
                    m,
                    d,
                    time: Some((r / 3600, r / 60 % 60, r % 60, Some(0))),
                };
                reference = earliest(reference, date);
            }
        } else if el
            .attr("class")
            .is_some_and(|c| matches!(c, "date-published" | "published" | "time published"))
        {
            if let Some(t) = el.attr("title") {
                if let Some(d) = date_expr(t, b) {
                    return Some(d);
                }
            } else {
                let text = text_prefix(n);
                if text.len() > 10
                    && let Some(d) = date_expr(&text, b)
                {
                    reference = earliest(reference, d);
                }
            }
        }
    }
    b.check(reference).or_else(|| examine_elements(abbrs, b))
}

/// htmldate 的 examine_time_elements（原始发布日期模式：`pubdate` 或 `entry-date` 类直接采用，否则取最早）。
pub(super) fn examine_time(times: &[NodeR<'_>], b: &Bounds) -> Option<Date> {
    if times.is_empty() || times.len() > MAX_CANDIDATES {
        return None;
    }
    let mut reference = None;
    for n in times {
        let el = element(n)?;
        let dt = el.attr("datetime").unwrap_or("");
        if dt.len() > 6 {
            let class = el.attr("class").unwrap_or("");
            if el.attr("pubdate") == Some("pubdate")
                || class.starts_with("entry-date")
                || class.starts_with("entry-time")
            {
                if let Some(d) = date_expr(dt, b) {
                    return Some(d);
                }
            } else if let Some(d) = date_expr(dt, b) {
                reference = earliest(reference, d);
            }
        } else {
            let own: String = n
                .children()
                .filter_map(|c| c.as_text().map(str::to_string))
                .collect();
            if own.len() > 6
                && let Some(d) = date_expr(&own, b)
            {
                reference = earliest(reference, d);
            }
        }
    }
    b.check(reference)
}

/// 元素的 id/class 里有没有日期线索（htmldate 的 is_date_candidate：id 和 class 只看源码里先出现的那个）。
/// 候选元素的 id/class 线索，按用途分组（同 htmldate 的 is_date_candidate），一个自动机一遍扫完。
pub(super) const CUE_DATE: u8 = 1;
pub(super) const CUE_FIRST: u8 = 2;
pub(super) const CUE_CLASS: u8 = 4;
pub(super) const CUE_LASTMOD: u8 = 8;
pub(super) const CUES: &[(&str, u8)] = &[
    ("date", CUE_DATE),
    ("Date", CUE_DATE),
    ("datum", CUE_DATE),
    ("Datum", CUE_DATE),
    ("meta", CUE_FIRST),
    ("Meta", CUE_FIRST),
    ("time", CUE_FIRST),
    ("publish", CUE_FIRST),
    ("footer", CUE_FIRST),
    ("info", CUE_CLASS),
    ("post_detail", CUE_CLASS),
    ("block-content", CUE_CLASS),
    ("byline", CUE_CLASS),
    ("subline", CUE_CLASS),
    ("posted", CUE_CLASS),
    ("submitted", CUE_CLASS),
    ("created-post", CUE_CLASS),
    ("publication", CUE_CLASS),
    ("author", CUE_CLASS),
    ("autor", CUE_CLASS),
    ("field-content", CUE_CLASS),
    ("fa-clock-o", CUE_CLASS),
    ("fa-calendar", CUE_CLASS),
    ("fecha", CUE_CLASS),
    ("parution", CUE_CLASS),
    ("footer-info-lastmod", CUE_LASTMOD),
];
pub(super) static AC_CUES: LazyLock<AhoCorasick> =
    LazyLock::new(|| AhoCorasick::new(CUES.iter().map(|c| c.0)).unwrap());
pub(super) static AC_BANNER: LazyLock<AhoCorasick> = LazyLock::new(|| {
    AhoCorasick::builder()
        .ascii_case_insensitive(true)
        .build(["edition", "today", "current-date", "currentdate"])
        .unwrap()
});

/// 属性值命中了哪些分组。
pub(super) fn cue_bits(v: &str) -> u8 {
    AC_CUES
        .find_overlapping_iter(v)
        .fold(0, |acc, m| acc | CUES[m.pattern().as_usize()].1)
}

pub(super) fn is_date_candidate(el: &pagesift_html::Element<'_>) -> bool {
    let (mut first, mut itemprop, mut class, mut id) = (None, None, 0u8, 0u8);
    for (k, v) in el.attrs() {
        match k {
            "itemprop" => itemprop = Some(v),
            "class" if first.is_none() || class == 0 => {
                class = cue_bits(v);
                first.get_or_insert(class);
            }
            "id" if first.is_none() || id == 0 => {
                id = cue_bits(v);
                first.get_or_insert(id);
            }
            _ => {}
        }
    }
    // htmldate 把 D 换成 d 再找 date/datum，等价于找这四种写法
    if first.is_some_and(|f| f & CUE_DATE != 0)
        || itemprop.is_some_and(|v| cue_bits(v) & CUE_DATE != 0)
    {
        return true;
    }
    let Some(first) = first else { return false };
    first & CUE_FIRST != 0 || class & CUE_CLASS != 0 || id & CUE_LASTMOD != 0
}

/// "今日日期"栏（class/id 带 edition、today、current-date）：显示的是访问当天，不是发布日期（自研规则）。
pub(super) fn is_banner(node: &NodeR<'_>, el: &pagesift_html::Element<'_>) -> bool {
    let named = |e: &pagesift_html::Element<'_>| {
        [e.attr("class"), e.attr("id")]
            .into_iter()
            .flatten()
            .any(|v| AC_BANNER.is_match(v))
    };
    named(el)
        || node
            .ancestors()
            .take(3)
            .filter_map(|a| element(&a))
            .any(|a| named(&a))
}

/// 清理时整段跳过的元素（htmldate 的 CLEANING_LIST，和 archive.org 的横幅）。
pub(super) fn pruned(tag: Tag, el: &pagesift_html::Element<'_>) -> bool {
    match tag {
        Tag::Applet
        | Tag::Audio
        | Tag::Canvas
        | Tag::Datalist
        | Tag::Embed
        | Tag::Frame
        | Tag::Frameset
        | Tag::Iframe
        | Tag::Label
        | Tag::Map
        | Tag::Math
        | Tag::Noframes
        | Tag::Object
        | Tag::Picture
        | Tag::Svg
        | Tag::Track
        | Tag::Video => true,
        Tag::Other => el.name() == "rdf",
        Tag::Div => matches!(el.attr("id"), Some("wm-ipp-base" | "wm-ipp")),
        _ => false,
    }
}
