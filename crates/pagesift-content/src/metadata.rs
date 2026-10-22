use pagesift_html::Html;
use url::Url;

use crate::dates::{self, normalize};
use crate::dom::{collapse_whitespace, element};
use crate::jsonld;

/// 页面元数据。字段对应 Firecrawl `metadata` 里的同名字段。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    pub title: Option<String>,
    pub description: Option<String>,
    pub language: Option<String>,
    pub keywords: Option<String>,
    pub robots: Option<String>,
    /// 依次取 JSON-LD、meta。
    pub author: Option<String>,
    pub canonical_url: Option<String>,
    pub favicon: Option<String>,
    /// 原始发布日期（见 [`crate::dates`]）：RFC 3339，保留页面上写的时区偏移（`2024-06-24T08:00:00+08:00`）；
    /// 只知道日期时只有日期（`2024-06-24`）。
    pub published_time: Option<String>,
    /// 最后修改时间：依次取 JSON-LD、meta、`<time itemprop=dateModified>`，格式同 `published_time`。
    pub modified_time: Option<String>,
    /// 页面自己声明是一篇文章：JSON-LD 里有文章类节点，或者 `og:type` 是 `article`。
    pub is_article: bool,
    /// `og:*`、`twitter:*`、`article:*` 和 `dc.*` 这类 meta，按出现顺序保留原始键名。
    pub extra: Vec<(String, String)>,
}

fn clean(s: &str) -> Option<String> {
    let s = collapse_whitespace(s);
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn resolve(base: Option<&Url>, href: &str) -> String {
    match base {
        Some(b) => b
            .join(href)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| href.to_string()),
        None => href.to_string(),
    }
}

pub(crate) fn extract(html: &Html<'_>, source: &str, base: Option<&Url>) -> Metadata {
    let mut m = Metadata::default();
    let mut title_tag = None;
    let mut og_title = None;
    let mut og_description = None;
    let mut ld_blocks = Vec::new();
    // 按出现顺序收集的候选值，取第一个能解析的
    let mut meta_published = Vec::new();
    let mut meta_modified = Vec::new();
    let mut time_modified = Vec::new();

    crate::dates::walk(html.root(), |node| {
        let Some(el) = element(node) else {
            return false;
        };
        match el.name() {
            "svg" | "style" | "code" | "pre" => return false,
            "html" => {
                if m.language.is_none() {
                    m.language = el.attr("lang").and_then(clean);
                }
            }
            "title" if title_tag.is_none() => {
                let text: String = node
                    .descendants()
                    .filter_map(|d| d.as_text().map(|t| t.to_string()))
                    .collect();
                title_tag = clean(&text);
                return false;
            }
            "meta" => {
                let key = el
                    .attr("name")
                    .or_else(|| el.attr("property"))
                    .or_else(|| el.attr("itemprop"));
                let (Some(key), Some(content)) = (key, el.attr("content").and_then(clean)) else {
                    return false;
                };
                let lower = key.trim().to_ascii_lowercase();
                match lower.as_str() {
                    "description" => set_once(&mut m.description, content.clone()),
                    "keywords" => set_once(&mut m.keywords, content.clone()),
                    "robots" => set_once(&mut m.robots, content.clone()),
                    "author" | "dc.creator" | "dcterms.creator" | "parsely-author"
                    | "sailthru.author" | "article:author"
                        if !content.starts_with("http") =>
                    {
                        set_once(&mut m.author, content.clone())
                    }
                    "og:title" => set_once(&mut og_title, content.clone()),
                    "og:description" => set_once(&mut og_description, content.clone()),
                    "article:published_time"
                    | "datepublished"
                    | "og:published_time"
                    | "publishdate"
                    | "pubdate"
                    | "date"
                    | "dc.date"
                    | "dc.date.issued"
                    | "dcterms.created"
                    | "dcterms.issued"
                    | "parsely-pub-date"
                    | "sailthru.date" => meta_published.push(content.clone()),
                    "article:modified_time"
                    | "datemodified"
                    | "og:updated_time"
                    | "dcterms.modified"
                    | "last-modified" => meta_modified.push(content.clone()),
                    _ => {}
                }
                if lower.starts_with("og:")
                    || lower.starts_with("twitter:")
                    || lower.starts_with("article:")
                    || lower.starts_with("dc.")
                    || lower.starts_with("dcterms.")
                {
                    m.extra.push((key.trim().to_string(), content));
                }
                return false;
            }
            "script" => {
                if el
                    .attr("type")
                    .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/ld+json"))
                {
                    let text: String = node
                        .children()
                        .filter_map(|c| c.as_text().map(|t| t.to_string()))
                        .collect();
                    ld_blocks.push(text);
                }
                return false;
            }
            "time" if el.attr("itemprop") == Some("dateModified") => {
                if let Some(dt) = el.attr("datetime") {
                    time_modified.push(dt.to_string());
                }
                return false;
            }
            "link" => {
                let rel = el.attr("rel").unwrap_or("").to_ascii_lowercase();
                if let Some(href) = el.attr("href").map(str::trim).filter(|h| !h.is_empty()) {
                    let rels: Vec<&str> = rel.split_whitespace().collect();
                    if rels.contains(&"canonical") {
                        set_once(&mut m.canonical_url, resolve(base, href));
                    }
                    if rels.contains(&"icon") {
                        set_once(&mut m.favicon, resolve(base, href));
                    }
                }
                return false;
            }
            _ => {}
        }
        true
    });

    let ld = jsonld::extract(&ld_blocks);
    // og:type=article 很多建站工具每一页都写（WordPress 的 Yoast 就是），单独不算；
    // 要么 JSON-LD 声明了文章类型，要么 og:type=article 而且有发布时间
    let og_article = m
        .extra
        .iter()
        .any(|(k, v)| k == "og:type" && v.eq_ignore_ascii_case("article"));
    let has_published = ld.published.is_some() || !meta_published.is_empty();
    if ld.author.is_some() {
        m.author = ld.author;
    }
    let first_date = |c: &[String]| c.iter().find_map(|s| normalize(s));
    m.published_time = dates::published(html, source, base.map(Url::as_str)).map(|d| d.render());
    m.modified_time = ld
        .modified
        .or_else(|| first_date(&meta_modified))
        .or_else(|| first_date(&time_modified));

    // Older sites omit publication metadata. Corroborate their article claim
    // with one substantial semantic article or an explicit article-body block.
    let structural_article = og_article && !ld.article && !has_published && {
        let candidates: Vec<_> = html
            .root()
            .descendants()
            .filter(|n| {
                n.as_element().is_some_and(|e| {
                    e.name() == "article"
                        || crate::features::compute(e) & crate::features::TIER[0] != 0
                        || e.attr("itemprop") == Some("articleBody")
                })
            })
            .filter(|n| {
                let mut count = 0;
                let mut chars = 0;
                for p in n
                    .descendants()
                    .filter(|d| d.as_element().is_some_and(|e| e.name() == "p"))
                {
                    count += 1;
                    chars += p.text().chars().count();
                    if count >= 3 && chars >= 400 {
                        return true;
                    }
                }
                false
            })
            .collect();
        let ids: std::collections::HashSet<_> = candidates.iter().map(|n| n.id()).collect();
        candidates
            .iter()
            .filter(|n| !n.ancestors().any(|a| ids.contains(&a.id())))
            .count()
            == 1
    };
    m.is_article = ld.article || (og_article && (has_published || structural_article));
    m.title = title_tag.or(og_title);
    if m.description.is_none() {
        m.description = og_description;
    }
    if m.favicon.is_none()
        && let Some(base) = base
        && let Ok(u) = base.join("/favicon.ico")
    {
        m.favicon = Some(u.to_string());
    }
    m
}

fn set_once(slot: &mut Option<String>, value: String) {
    if slot.is_none() {
        *slot = Some(value);
    }
}
