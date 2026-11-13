use pagesift_content::yolowhtml_dom::{Html, NodeRef};
use pagesift_content::{Fingerprints, Metadata};
use pagesift_graph::{LinkSection, RawLinkObservation};
use serde_json::Value;
use url::Url;

#[derive(Debug, Clone, Default)]
pub struct DomGraphData {
    pub links: Vec<RawLinkObservation>,
    pub canonical_url: Option<String>,
    pub alternates: Vec<(String, String)>,
    pub json_ld: Vec<Value>,
    pub raw_html_hash: Option<String>,
    pub markdown_hash: Option<String>,
    pub text_hash: Option<String>,
}

fn collapse_ws(input: &str) -> String {
    let mut out = String::with_capacity(input.len().min(160)); // [策略] 锚文本预分配上限
    let mut in_ws = false;
    for ch in input.trim().chars() {
        if ch.is_whitespace() {
            if !in_ws && !out.is_empty() {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(ch);
            in_ws = false;
            const MAX_ANCHOR_LEN: usize = 180; // [策略] 锚文本截断长度
            if out.len() >= MAX_ANCHOR_LEN {
                break;
            }
        }
    }
    out.trim().to_string()
}

fn resolve_href(base: Option<&Url>, raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('#')
        || trimmed.starts_with("javascript:")
        || trimmed.starts_with("data:")
        || trimmed.starts_with("mailto:")
        || trimmed.starts_with("tel:")
    {
        return None;
    }
    if let Some(b) = base {
        b.join(trimmed).ok().map(|u| u.to_string())
    } else if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        Some(trimmed.to_string())
    } else {
        None
    }
}

fn contains_token(haystack: &str, needles: &[&str]) -> bool {
    let lower = haystack.to_ascii_lowercase();
    needles.iter().any(|n| lower.contains(n))
}

fn classify_section(anchor: NodeRef<'_>) -> LinkSection {
    for anc in anchor.ancestors() {
        let Some(el) = anc.as_element() else {
            continue;
        };
        let tag = el.name();
        let role = el.attr("role").unwrap_or("");
        if tag == "nav" || role.eq_ignore_ascii_case("navigation") {
            return LinkSection::Navigation;
        }
        if tag == "footer" || role.eq_ignore_ascii_case("contentinfo") {
            return LinkSection::Footer;
        }
        if tag == "header" || role.eq_ignore_ascii_case("banner") {
            return LinkSection::Header;
        }
        if tag == "aside" || role.eq_ignore_ascii_case("complementary") {
            return LinkSection::Sidebar;
        }
        for attr_val in [el.attr("id"), el.attr("class")].into_iter().flatten() {
            if contains_token(
                attr_val,
                &["reference", "bibliography", "citation", "footnote"],
            ) {
                return LinkSection::ReferenceList;
            }
            if contains_token(attr_val, &["footer", "copyright"]) {
                return LinkSection::Footer;
            }
            if contains_token(attr_val, &["sidebar", "aside"]) {
                return LinkSection::Sidebar;
            }
            if contains_token(attr_val, &["navbar", "topnav", "sidenav", "breadcrumb"]) {
                return LinkSection::Navigation;
            }
        }
        if tag == "main" || tag == "article" || role.eq_ignore_ascii_case("main") {
            return LinkSection::MainContent;
        }
    }
    LinkSection::Unknown
}

fn heading_level(tag: &str) -> Option<u8> {
    match tag {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3), // [协议] HTML 标题层级 h3
        "h4" => Some(4), // [协议] HTML 标题层级 h4
        "h5" => Some(5), // [协议] HTML 标题层级 h5
        "h6" => Some(6), // [协议] HTML 标题层级 h6
        _ => None,
    }
}

/// Extract link observations and canonical fallback directly from an already-parsed DOM tree.
pub fn collect_from_dom(
    dom: &Html<'_>,
    base_url: Option<&str>,
) -> (Vec<RawLinkObservation>, Option<String>) {
    let mut base = base_url.and_then(|u| Url::parse(u).ok());
    for node in dom.root().descendants() {
        if let Some(el) = node.as_element()
            && el.name() == "base"
            && let Some(href) = el.attr("href")
            && let Ok(parsed) = base
                .as_ref()
                .map_or_else(|| Url::parse(href), |b| b.join(href))
        {
            base = Some(parsed);
            break;
        }
    }

    let mut links = Vec::new();
    let mut canonical = None;
    let mut heading_stack: Vec<(u8, String)> = Vec::new();

    for node in dom.root().descendants() {
        let Some(el) = node.as_element() else {
            continue;
        };
        let tag = el.name();
        if let Some(lvl) = heading_level(tag) {
            let text = collapse_ws(&node.text());
            if !text.is_empty() {
                while heading_stack.last().is_some_and(|(l, _)| *l >= lvl) {
                    heading_stack.pop();
                }
                heading_stack.push((lvl, text));
            }
            continue;
        }

        if tag == "a"
            && let Some(href) = el.attr("href")
            && let Some(target_url) = resolve_href(base.as_ref(), href)
        {
            let text = collapse_ws(&node.text());
            let anchor_text = if text.is_empty() {
                el.attr("title")
                    .or_else(|| el.attr("aria-label"))
                    .map(collapse_ws)
                    .unwrap_or_default()
            } else {
                text
            };
            let rel = el.attr("rel").map(collapse_ws).filter(|r| !r.is_empty());
            let section = classify_section(node);
            let heading_path = heading_stack.iter().map(|(_, h)| h.clone()).collect();
            links.push(RawLinkObservation {
                target_url,
                anchor_text,
                rel,
                section,
                heading_path,
                block_id: None,
            });
            continue;
        }

        if tag == "meta"
            && let Some(name) = el.attr("name")
            && matches!(
                name.to_ascii_lowercase().as_str(),
                "citation_pdf_url" | "citation_fulltext_html_url" | "citation_xml_url"
            )
            && let Some(content) = el.attr("content")
            && let Some(target_url) = resolve_href(base.as_ref(), content)
        {
            let lower_name = name.to_ascii_lowercase();
            links.push(RawLinkObservation {
                target_url,
                anchor_text: lower_name.clone(),
                rel: Some(lower_name),
                section: LinkSection::MetadataTag,
                heading_path: Vec::new(),
                block_id: None,
            });
            continue;
        }

        if tag == "link"
            && let Some(href) = el.attr("href")
            && let Some(rel_raw) = el.attr("rel")
        {
            let rel_lower = rel_raw.to_ascii_lowercase();
            let tokens: Vec<&str> = rel_lower.split_whitespace().collect();
            if canonical.is_none()
                && tokens.contains(&"canonical")
                && let Some(resolved) = resolve_href(base.as_ref(), href)
            {
                canonical = Some(resolved);
            }
            if tokens
                .iter()
                .any(|t| *t == "enclosure" || *t == "related" || *t == "cita" || *t == "cite-as")
                && let Some(target_url) = resolve_href(base.as_ref(), href)
            {
                links.push(RawLinkObservation {
                    target_url,
                    anchor_text: rel_lower.clone(),
                    rel: Some(rel_lower),
                    section: LinkSection::MetadataTag,
                    heading_path: Vec::new(),
                    block_id: None,
                });
            }
        }
    }

    (links, canonical)
}

/// Bundle DOM links together with already-parsed content metadata and fingerprints.
pub fn collect_dom_graph_data(
    dom: &Html<'_>,
    meta: &Metadata,
    fingerprints: Option<&Fingerprints>,
    base_url: Option<&str>,
) -> DomGraphData {
    let (links, dom_canonical) = collect_from_dom(dom, base_url);
    DomGraphData {
        links,
        canonical_url: meta.canonical_url.clone().or(dom_canonical),
        alternates: meta.alternates.clone(),
        json_ld: meta.json_ld.clone(),
        raw_html_hash: fingerprints.map(|f| f.raw_html.clone()),
        markdown_hash: fingerprints.map(|f| f.markdown.clone()),
        text_hash: fingerprints.map(|f| f.text.clone()),
    }
}

/// Parse HTML on demand (e.g. for rendered browser HTML or uploaded HTML) and collect DOM graph data.
pub fn extract_from_html_str(
    html: &str,
    meta: &Metadata,
    fingerprints: Option<&Fingerprints>,
    base_url: Option<&str>,
) -> DomGraphData {
    let dom = Html::parse(html);
    collect_dom_graph_data(&dom, meta, fingerprints, base_url)
}
