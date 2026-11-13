//! Document-to-document citation and hyperlink aggregation (multi-parent aware, bounded fan-out).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::hash::{compact_excerpt, host_of, normalize_resource_key, normalize_url};
use crate::model::{BlockKind, BlockRef};

/// Role played by an outbound link edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkRole {
    Citation,
    Navigation,
    Attachment,
    Alternate,
    Canonical,
    Feed,
    Internal,
    External,
}

/// Structural DOM/Markdown zone where the link appeared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkSection {
    MainContent,
    Navigation,
    Header,
    Footer,
    Sidebar,
    ReferenceList,
    MetadataTag,
    #[default]
    Unknown,
}

/// Sample context for one link edge occurrence without cloning the full page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnchorSample {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rel: Option<String>,
    pub section: LinkSection,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heading_path: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_id: Option<String>,
}

/// Raw link observation emitted during DOM/Markdown processing of a single document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawLinkObservation {
    pub target_url: String,
    pub anchor_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rel: Option<String>,
    #[serde(default)]
    pub section: LinkSection,
    #[serde(default)]
    pub heading_path: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_id: Option<String>,
}

/// Aggregated directed document-to-resource citation edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CitationEdge {
    pub from_doc_id: String,
    pub from_resource_key: String,
    pub to_url: String,
    pub to_resource_key: String,
    pub role: LinkRole,
    pub occurrences: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<AnchorSample>,
    pub crawled: bool,
}

const ATTACHMENT_EXTS: &[&str] = &[
    ".pdf", ".docx", ".xlsx", ".pptx", ".doc", ".xls", ".ppt", ".xlsb", ".csv", ".tsv", ".rtf",
    ".odt", ".ods", ".odp", ".epub", ".mobi", ".hwp", ".hwpx", ".zip", ".tar", ".tgz", ".gz",
    ".bz2", ".xz", ".7z", ".zst", ".tzst", ".eml", ".mbox", ".msg", ".sqlite",
];

/// Classify link role deterministically from rel tokens, DOM section, host, and extension.
pub fn classify_link_role(
    from_url: &str,
    to_url: &str,
    rel: Option<&str>,
    section: LinkSection,
) -> LinkRole {
    if let Some(rel_str) = rel {
        let lower = rel_str.to_ascii_lowercase();
        let tokens: Vec<&str> = lower.split_whitespace().collect();
        if tokens.contains(&"canonical") {
            return LinkRole::Canonical;
        }
        if tokens.contains(&"alternate") {
            return LinkRole::Alternate;
        }
        if tokens.contains(&"enclosure") {
            return LinkRole::Attachment;
        }
        if tokens.iter().any(|t| matches!(*t, "cite" | "citation")) {
            return LinkRole::Citation;
        }
    }
    let path_lower = Url::parse(to_url)
        .ok()
        .map(|u| u.path().to_ascii_lowercase())
        .unwrap_or_else(|| to_url.to_ascii_lowercase());
    if ATTACHMENT_EXTS.iter().any(|ext| path_lower.ends_with(ext)) {
        return LinkRole::Attachment;
    }
    if matches!(section, LinkSection::ReferenceList) {
        return LinkRole::Citation;
    }
    if matches!(
        section,
        LinkSection::Navigation | LinkSection::Header | LinkSection::Footer | LinkSection::Sidebar
    ) {
        return LinkRole::Navigation;
    }
    let from_host = host_of(from_url);
    let to_host = host_of(to_url);
    if !from_host.is_empty() && !to_host.is_empty() && from_host != to_host {
        LinkRole::External
    } else if matches!(section, LinkSection::MainContent) {
        LinkRole::Citation
    } else {
        LinkRole::Internal
    }
}

/// Extract `[anchor](url)` links from Markdown when DOM observations are unavailable (e.g. PDF/Office).
pub fn extract_markdown_links(
    markdown: &str,
    base_url: Option<&str>,
    blocks: &[BlockRef],
) -> Vec<RawLinkObservation> {
    let base = base_url.and_then(|u| Url::parse(u).ok());
    let mut out = Vec::new();
    let bytes = markdown.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'!' && bytes.get(i + 1) == Some(&b'[') {
            i += 2;
            continue;
        }
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        let Some(close_bracket) = find_unescaped(markdown, i + 1, b']') else {
            i += 1;
            continue;
        };
        if bytes.get(close_bracket + 1) != Some(&b'(') {
            i = close_bracket + 1;
            continue;
        }
        let Some(close_paren) = find_unescaped(markdown, close_bracket + 2, b')') else {
            i = close_bracket + 2;
            continue;
        };
        let anchor = markdown[i + 1..close_bracket].trim();
        let raw_dest = markdown[close_bracket + 2..close_paren].trim();
        let dest = raw_dest.split_whitespace().next().unwrap_or("");
        if let Some(resolved) = resolve_http_target(dest, base.as_ref()) {
            let block = blocks.iter().find(|b| i >= b.start && i <= b.end);
            out.push(RawLinkObservation {
                target_url: resolved,
                anchor_text: anchor.to_owned(),
                rel: None,
                section: if block.is_some_and(|b| b.kind != BlockKind::Code) {
                    LinkSection::MainContent
                } else {
                    LinkSection::Unknown
                },
                heading_path: block.map(|b| b.heading_path.clone()).unwrap_or_default(),
                block_id: block.map(|b| b.block_id.clone()),
            });
        }
        i = close_paren + 1;
    }
    out
}

fn find_unescaped(s: &str, start: usize, needle: u8) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'\n' {
            return None;
        }
        if bytes[i] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn resolve_http_target(raw: &str, base: Option<&Url>) -> Option<String> {
    let trimmed = raw.trim().trim_matches(|c| c == '<' || c == '>');
    if trimmed.is_empty()
        || trimmed.starts_with('#')
        || trimmed.starts_with("javascript:")
        || trimmed.starts_with("mailto:")
    {
        return None;
    }
    let parsed = Url::parse(trimmed)
        .ok()
        .or_else(|| base.and_then(|b| b.join(trimmed).ok()))?;
    matches!(parsed.scheme(), "http" | "https").then(|| normalize_url(parsed.as_str()))
}

/// Deduplicate raw link hits into bounded `(to_resource_key, role)` citation edges.
pub fn aggregate_citations(
    from_doc_id: &str,
    from_url: &str,
    from_resource_key: &str,
    observations: &[RawLinkObservation],
    max_edges_per_doc: usize,
    max_samples: usize,
) -> Vec<CitationEdge> {
    let mut edges: Vec<CitationEdge> = Vec::new();
    let mut index: HashMap<(String, LinkRole), usize> = HashMap::new();

    for obs in observations {
        let norm_target = normalize_url(&obs.target_url);
        let to_key = normalize_resource_key(&norm_target);
        if to_key == from_resource_key {
            continue;
        }
        let role = classify_link_role(from_url, &norm_target, obs.rel.as_deref(), obs.section);
        let key = (to_key.clone(), role);
        let sample = AnchorSample {
            text: compact_excerpt(&obs.anchor_text, 120), // [策略] 锚文本采样摘要长度
            rel: obs.rel.clone(),
            section: obs.section,
            heading_path: obs.heading_path.clone(),
            block_id: obs.block_id.clone(),
        };

        if let Some(&slot) = index.get(&key) {
            edges[slot].occurrences += 1;
            if edges[slot].samples.len() < max_samples
                && !edges[slot]
                    .samples
                    .iter()
                    .any(|s| s.text == sample.text && s.block_id == sample.block_id)
            {
                edges[slot].samples.push(sample);
            }
            continue;
        }
        if edges.len() >= max_edges_per_doc {
            continue;
        }
        index.insert(key, edges.len());
        edges.push(CitationEdge {
            from_doc_id: from_doc_id.to_owned(),
            from_resource_key: from_resource_key.to_owned(),
            to_url: norm_target,
            to_resource_key: to_key,
            role,
            occurrences: 1,
            samples: if max_samples > 0 {
                vec![sample]
            } else {
                Vec::new()
            },
            crawled: false,
        });
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::build_block_refs;

    #[test]
    fn extracts_and_aggregates_markdown_citations_and_attachments() {
        let md = "# Spec\n\nSee [Pricing](https://example.com/pricing?utm_source=x) and [PDF](/files/spec.pdf).\n\nRead [Pricing](/pricing) again.";
        let blocks = build_block_refs("doc_a", md, &[], 64, 160);
        let obs = extract_markdown_links(md, Some("https://example.com/docs"), &blocks);
        assert_eq!(obs.len(), 3);
        let edges = aggregate_citations(
            "doc_a",
            "https://example.com/docs",
            "https://example.com/docs",
            &obs,
            256,
            3,
        );
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0].role, LinkRole::Citation);
        assert_eq!(edges[0].occurrences, 2);
        assert_eq!(edges[1].role, LinkRole::Attachment);
        assert_eq!(edges[1].to_url, "https://example.com/files/spec.pdf");
    }
}
