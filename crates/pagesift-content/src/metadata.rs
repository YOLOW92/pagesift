use pagesift_html::Html;
use url::Url;

use crate::datetime::to_utc_rfc3339;
use crate::dom::{collapse_whitespace, element, tag};
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
    /// 依次取 JSON-LD、meta、`<time datetime>`，统一为 UTC 的 RFC 3339（`2024-06-24T08:00:00Z`）。
    pub published_time: Option<String>,
    /// 同 `published_time`。
    pub modified_time: Option<String>,
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

pub(crate) fn extract(html: &Html<'_>, base: Option<&Url>) -> Metadata {
    let mut m = Metadata::default();
    let mut title_tag = None;
    let mut og_title = None;
    let mut og_description = None;
    let mut ld_blocks = Vec::new();
    // 按出现顺序收集的候选值，取第一个能解析的
    let mut meta_published = Vec::new();
    let mut meta_modified = Vec::new();
    let mut time_published = Vec::new();
    let mut time_any = Vec::new();
    let mut time_modified = Vec::new();

    for node in html.root().descendants() {
        let Some(el) = element(&node) else { continue };
        match el.name() {
            "html" => {
                if m.language.is_none() {
                    m.language = el.attr("lang").and_then(clean);
                }
            }
            "title" if title_tag.is_none() && !in_svg(&node) => {
                let text: String = node
                    .descendants()
                    .filter_map(|d| d.as_text().map(|t| t.to_string()))
                    .collect();
                title_tag = clean(&text);
            }
            "meta" => {
                let key = el
                    .attr("name")
                    .or_else(|| el.attr("property"))
                    .or_else(|| el.attr("itemprop"));
                let (Some(key), Some(content)) = (key, el.attr("content").and_then(clean)) else {
                    continue;
                };
                let lower = key.trim().to_ascii_lowercase();
                match lower.as_str() {
                    "description" => set_once(&mut m.description, content),
                    "keywords" => set_once(&mut m.keywords, content),
                    "robots" => set_once(&mut m.robots, content),
                    "author" | "dc.creator" | "dcterms.creator" | "parsely-author"
                    | "sailthru.author" | "article:author"
                        if !content.starts_with("http") =>
                    {
                        set_once(&mut m.author, content)
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
                    m.extra.push((
                        key.trim().to_string(),
                        el.attr("content").and_then(clean).unwrap_or_default(),
                    ));
                }
            }
            "script"
                if el
                    .attr("type")
                    .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/ld+json")) =>
            {
                let text: String = node
                    .children()
                    .filter_map(|c| c.as_text().map(|t| t.to_string()))
                    .collect();
                ld_blocks.push(text);
            }
            "time" => {
                let Some(dt) = el.attr("datetime") else {
                    continue;
                };
                match el.attr("itemprop") {
                    Some("dateModified") => time_modified.push(dt.to_string()),
                    Some("datePublished") => time_published.push(dt.to_string()),
                    _ if el.attr("pubdate").is_some() => time_published.push(dt.to_string()),
                    _ => time_any.push(dt.to_string()),
                }
            }
            "link" => {
                let rel = el.attr("rel").unwrap_or("").to_ascii_lowercase();
                let Some(href) = el.attr("href").map(str::trim).filter(|h| !h.is_empty()) else {
                    continue;
                };
                let rels: Vec<&str> = rel.split_whitespace().collect();
                if rels.contains(&"canonical") {
                    set_once(&mut m.canonical_url, resolve(base, href));
                }
                if rels.contains(&"icon") {
                    set_once(&mut m.favicon, resolve(base, href));
                }
            }
            _ => {}
        }
    }

    let ld = jsonld::extract(&ld_blocks);
    if ld.author.is_some() {
        m.author = ld.author;
    }
    let first_date = |c: &[String]| c.iter().find_map(|s| to_utc_rfc3339(s));
    m.published_time = ld
        .published
        .or_else(|| first_date(&meta_published))
        .or_else(|| first_date(&time_published))
        .or_else(|| first_date(&time_any));
    m.modified_time = ld
        .modified
        .or_else(|| first_date(&meta_modified))
        .or_else(|| first_date(&time_modified));

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

fn in_svg(node: &crate::dom::NodeR<'_>) -> bool {
    node.ancestors().any(|a| tag(&a) == Some("svg"))
}
