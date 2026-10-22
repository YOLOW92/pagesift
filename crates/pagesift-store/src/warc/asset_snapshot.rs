//! Webpage time-capsule asset snapshot and embedded resource sniffer.
//! Extracts referenced CSS, JS, Fonts, Images, and Favicons to build self-contained archives.

use std::collections::BTreeSet;

/// A secondary asset linked to a root webpage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubresourceAsset {
    pub uri: String,
    pub mime_type: String,
    pub content: Vec<u8>,
}

/// Webpage snapshot comprising the primary HTML response and all subsidiary assets.
#[derive(Clone, Debug)]
pub struct WebpageSnapshot {
    pub target_uri: String,
    pub status_code: u16,
    pub html_content: Vec<u8>,
    pub http_headers: Vec<(String, String)>,
    pub assets: Vec<SubresourceAsset>,
}

pub struct AssetSniffer;

impl AssetSniffer {
    /// Sniff candidate asset URLs from raw HTML content.
    /// Operates using zero-copy slicing without requiring full DOM parser allocations.
    pub fn sniff_asset_urls(html: &str) -> Vec<String> {
        let mut urls = BTreeSet::new();

        // 1. Scan attributes: src=, href=, data-src=
        for pattern in &["src=\"", "src='", "href=\"", "href='", "data-src=\"", "data-src='"] {
            let mut start_idx = 0;
            while let Some(found) = html[start_idx..].find(pattern) {
                let actual_start = start_idx + found + pattern.len();
                let quote = pattern.chars().last().unwrap();
                if let Some(end) = html[actual_start..].find(quote) {
                    let url_str = &html[actual_start..actual_start + end];
                    if is_relevant_asset_url(url_str) {
                        urls.insert(url_str.trim().to_string());
                    }
                    start_idx = actual_start + end + 1;
                } else {
                    break;
                }
            }
        }

        // 2. Scan CSS url(...) references
        let mut css_idx = 0;
        while let Some(found) = html[css_idx..].find("url(") {
            let actual_start = css_idx + found + 4;
            if let Some(end) = html[actual_start..].find(')') {
                let raw = html[actual_start..actual_start + end].trim().trim_matches(|c| c == '\'' || c == '"');
                if is_relevant_asset_url(raw) {
                    urls.insert(raw.to_string());
                }
                css_idx = actual_start + end + 1;
            } else {
                break;
            }
        }

        urls.into_iter().collect()
    }
}

fn is_relevant_asset_url(url: &str) -> bool {
    if url.is_empty() || url.starts_with('#') || url.starts_with("javascript:") || url.starts_with("data:") {
        return false;
    }

    let lower = url.to_lowercase();
    let has_asset_ext = [
        ".css", ".js", ".png", ".jpg", ".jpeg", ".gif", ".webp",
        ".svg", ".ico", ".woff", ".woff2", ".ttf", ".eot"
    ].iter().any(|ext| lower.contains(ext));

    let has_asset_path = lower.contains("/css/")
        || lower.contains("/js/")
        || lower.contains("/assets/")
        || lower.contains("/images/")
        || lower.contains("/static/")
        || lower.contains("/fonts/");

    has_asset_ext || has_asset_path
}
