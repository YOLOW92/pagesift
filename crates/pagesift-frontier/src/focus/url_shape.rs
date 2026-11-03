//! 从 URL 和锚文本看出链接的形状：栏目入口、子站的根、语言段和英文版入口，以及 URL 里
//! 可以当文字看的部分。链接打分（[`super::Focus`]）按这些形状给保底分或降权。

use url::Url;

#[derive(PartialEq, Eq, Debug)]
pub(super) enum Section {
    /// 本站的一级栏目（`/docs/`、`/services/`）。
    SameHost,
    /// 别的主机的根（子站首页）。
    Subsite,
}

/// 栏目入口：路径一段的是栏目，路径为空的是子站的根；锚文本都要短（1 到 4 个词）。
/// 开头的语言段不算（`/en/studies/`、`/english/research/` 是英文版的一级栏目）。
pub(super) fn section(url: &Url, anchor: Option<&str>) -> Option<Section> {
    let mut segs: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
    if segs
        .first()
        .is_some_and(|s| is_language_code(s) || is_language_name(s))
    {
        segs.remove(0);
    }
    let segments = segs.len();
    let short = anchor.is_some_and(|a| (1..=4).contains(&a.split_whitespace().count()));
    if !short || url.query().is_some() {
        return None;
    }
    match segments {
        0 => Some(Section::Subsite),
        1 => Some(Section::SameHost),
        _ => None,
    }
}

/// 路径里有别的语言的代码段（`/de/`、`/ja/`、`/zh-cn/`）：英文查询不该进去。
pub(super) fn foreign_language_segment(url: &Url) -> bool {
    const CODES: &[&str] = &[
        "de", "fr", "es", "it", "nl", "pt", "ru", "ja", "jp", "zh", "zh-cn", "zh-tw", "zh-hans",
        "zh-hant", "ko", "kr", "ar", "sv", "fi", "da", "no", "nb", "pl", "tr", "cs", "el", "he",
        "hi", "th", "vi", "id", "hu", "ro", "uk", "ca", "eu", "gl", "cy", "ga", "mi",
    ];
    url.path()
        .split('/')
        .filter(|s| !s.is_empty())
        .any(|seg| CODES.contains(&language_code(seg).as_str()))
}

/// 通往英文版的入口：锚文本是 "English"/"EN" 之类，或者路径里有 `/en/`、`/english/` 一段。
pub(super) fn language_portal(url: &Url, anchor: Option<&str>) -> bool {
    let anchor = anchor.unwrap_or_default().trim().to_ascii_lowercase();
    if matches!(
        anchor.as_str(),
        "english"
            | "en"
            | "eng"
            | "in english"
            | "english site"
            | "english version"
            | "english page"
            | "english top"
            | "english home"
    ) {
        return true;
    }
    // 只认英文版的入口本身（`/en/`、`/top/en/`、`/en.html`），不认它下面的每一页（那些按自己的内容打分）
    url.path()
        .split('/')
        .rfind(|seg| !seg.is_empty())
        .is_some_and(|seg| {
            matches!(
                language_code(seg).as_str(),
                "en" | "english" | "en-us" | "en-gb"
            )
        })
}

/// 路径段是英文版的名字（`english`、`eng`）：和语言代码一样是语言段。
pub(super) fn is_language_name(seg: &str) -> bool {
    matches!(language_code(seg).as_str(), "english" | "eng")
}

/// 语言标签是英文（`en`、`en-US`）。
pub(super) fn is_english(lang: &str) -> bool {
    let primary = lang.split(['-', '_']).next().unwrap_or_default();
    primary.eq_ignore_ascii_case("en")
}

/// 路径段是语言代码：两个字母，或者 `xx-yy`（可以带扩展名）。
pub(super) fn is_language_code(seg: &str) -> bool {
    let code = language_code(seg);
    let two = |p: &str| p.len() == 2 && p.bytes().all(|b| b.is_ascii_alphabetic());
    two(&code)
        || code
            .split_once('-')
            .is_some_and(|(a, b)| two(a) && (2..=4).contains(&b.len()))
}

/// 路径段当语言代码看：去掉扩展名，小写（`en.html` → `en`）。
fn language_code(seg: &str) -> String {
    seg.split_once('.')
        .map_or(seg, |(s, _)| s)
        .to_ascii_lowercase()
}

/// URL 里可以当文字看的部分：主机名的标签（`iss.example.edu` 的 iss）、路径段和查询串。
/// 子域名常常就是栏目名（international、admissions、iss），路径同理。
pub(super) fn locator(url: &Url) -> String {
    let mut s = String::with_capacity(url.as_str().len());
    if let Some(host) = url.host_str() {
        for label in host.split('.') {
            if label != "www" {
                s.push_str(label);
                s.push(' ');
            }
        }
    }
    s.push_str(url.path());
    if let Some(q) = url.query() {
        s.push(' ');
        s.push_str(q);
    }
    s
}

/// 网址的“形状”：同一模板生成的页面（`/item/123`、`/item/456`）形状相同。路径段全是数字的写成
/// `{n}`，含数字的写成 `{id}`，其余照原样；查询串只留参数名（排序）。结构化抽取缓存 LLM 生成的
/// schema 时用它作键的一部分（0.97）。
pub fn shape(url: &Url) -> String {
    let mut out = String::new();
    for seg in url.path().split('/').filter(|s| !s.is_empty()) {
        out.push('/');
        if seg.bytes().all(|b| b.is_ascii_digit()) {
            out.push_str("{n}");
        } else if seg.bytes().any(|b| b.is_ascii_digit()) {
            out.push_str("{id}");
        } else {
            out.push_str(seg);
        }
    }
    let mut keys: Vec<String> = url.query_pairs().map(|(k, _)| k.into_owned()).collect();
    keys.sort();
    keys.dedup();
    if !keys.is_empty() {
        out.push('?');
        out.push_str(&keys.join("&"));
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}
