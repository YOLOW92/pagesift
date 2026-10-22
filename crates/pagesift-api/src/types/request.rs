//! Firecrawl v1/v2 request types and options.
//! Fields and defaults align with Firecrawl source `apps/api/src/controllers/v2/types.ts`.
//! Unknown fields in requests are ignored for backward/forward compatibility.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

/// `formats` item: accepts both `"markdown"` and `{"type": "markdown", ...}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Format {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(flatten)]
    pub options: Map<String, Value>,
}

impl Format {
    pub fn new(kind: &str) -> Self {
        Self {
            kind: kind.to_string(),
            options: Map::new(),
        }
    }
}

impl<'de> Deserialize<'de> for Format {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::String(kind) => Ok(Format::new(&kind)),
            Value::Object(mut map) => {
                let kind = match map.remove("type") {
                    Some(Value::String(k)) => k,
                    _ => {
                        return Err(serde::de::Error::custom(
                            "format object needs a string `type`",
                        ));
                    }
                };
                Ok(Format { kind, options: map })
            }
            _ => Err(serde::de::Error::custom(
                "format must be a string or an object",
            )),
        }
    }
}

pub(crate) fn default_formats() -> Vec<Format> {
    vec![Format::new("markdown")]
}

pub(crate) fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Location {
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub languages: Vec<String>,
}

/// Single page scrape options (`scrapeOptions`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrapeOptions {
    #[serde(default = "default_formats")]
    pub formats: Vec<Format>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub include_tags: Vec<String>,
    #[serde(default)]
    pub exclude_tags: Vec<String>,
    #[serde(default = "yes")]
    pub only_main_content: bool,
    /// Milliseconds.
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Milliseconds.
    #[serde(default)]
    pub wait_for: u64,
    #[serde(default)]
    pub mobile: bool,
    #[serde(default)]
    pub actions: Vec<Value>,
    #[serde(default)]
    pub location: Option<Location>,
    #[serde(default)]
    pub parsers: Option<Value>,
    #[serde(default = "yes")]
    pub remove_base64_images: bool,
    #[serde(default = "yes")]
    pub block_ads: bool,
    /// `basic`, `auto` pass through; others return UNSUPPORTED_OPTION.
    #[serde(default)]
    pub proxy: Option<String>,
    /// PageSift extension: designated named egress (`[[egress]]`).
    #[serde(default)]
    pub egress: Option<String>,
    /// TLS skip verification flag (PageSift always verifies certificates).
    #[serde(default)]
    pub skip_tls_verification: Option<bool>,
    /// Milliseconds.
    #[serde(default)]
    pub max_age: Option<u64>,
    /// Milliseconds.
    #[serde(default)]
    pub min_age: Option<u64>,
    #[serde(default = "yes")]
    pub store_in_cache: bool,

    // ---- PageSift extensions ----
    /// `http`, `browser`, `auto` (default).
    #[serde(default)]
    pub engine: Option<String>,
    /// PageSift extension: bounded, stateless same-origin external JS discovery.
    #[serde(default)]
    pub discover_scripts: bool,
    #[serde(default)]
    pub if_none_match: Option<String>,
    #[serde(default)]
    pub if_modified_since: Option<String>,
    /// Client-side robots.txt bypass flag.
    #[serde(default)]
    pub ignore_robots_txt: bool,
    #[serde(default)]
    pub store_snapshot: bool,
    /// BM25 query filter on main content markdown.
    #[serde(default)]
    pub query_filter: Option<QueryFilter>,
    /// PageSift extension: chunking options (`{"type": "recursive", "size": 400}`, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunking: Option<Value>,
}

/// PageSift extension `queryFilter`: filter main content markdown by query relevance.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryFilter {
    pub query: String,
    /// Absolute threshold: blocks with BM25 score >= threshold are kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f32>,
    /// Relative threshold: score >= ratio * max_score. Defaults to 0.5.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relative: Option<f32>,
    /// Maximum number of blocks to keep.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<usize>,
    /// Token ceiling on retained content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<usize>,
    /// Token floor on retained content. Defaults to 1000.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_tokens: Option<usize>,
    /// Prepend ancestor headings before preserved units.
    pub headings: bool,
}

impl<'de> Deserialize<'de> for QueryFilter {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Full {
            query: String,
            #[serde(default)]
            threshold: Option<f32>,
            #[serde(default)]
            relative: Option<f32>,
            #[serde(default)]
            top_k: Option<usize>,
            #[serde(default)]
            max_tokens: Option<usize>,
            #[serde(default)]
            min_tokens: Option<usize>,
            #[serde(default = "yes")]
            headings: bool,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Query(String),
            Full(Full),
        }
        Ok(match Repr::deserialize(d)? {
            Repr::Query(query) => QueryFilter {
                query,
                threshold: None,
                relative: Some(pagesift_extract_defaults::RELATIVE),
                top_k: None,
                max_tokens: None,
                min_tokens: Some(pagesift_extract_defaults::MIN_TOKENS),
                headings: true,
            },
            Repr::Full(f) => QueryFilter {
                query: f.query,
                relative: f.relative.or(if f.threshold.is_none() {
                    Some(pagesift_extract_defaults::RELATIVE)
                } else {
                    None
                }),
                threshold: f.threshold,
                top_k: f.top_k,
                max_tokens: f.max_tokens,
                min_tokens: f.min_tokens.or(if f.threshold.is_none() {
                    Some(pagesift_extract_defaults::MIN_TOKENS)
                } else {
                    None
                }),
                headings: f.headings,
            },
        })
    }
}

mod pagesift_extract_defaults {
    pub const RELATIVE: f32 = 0.5;
    pub const MIN_TOKENS: usize = 1000;
}

impl Default for ScrapeOptions {
    fn default() -> Self {
        serde_json::from_value(Value::Object(Map::new())).expect("defaults deserialize")
    }
}

impl ScrapeOptions {
    pub fn wants(&self, kind: &str) -> bool {
        self.formats.iter().any(|f| f.kind == kind)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScrapeRequest {
    pub url: String,
    #[serde(flatten)]
    pub options: ScrapeOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchScrapeRequest {
    pub urls: Vec<String>,
    #[serde(default = "yes")]
    pub ignore_invalid_urls: bool,
    #[serde(default)]
    pub max_concurrency: Option<usize>,
    #[serde(default)]
    pub webhook: Option<Value>,
    #[serde(flatten)]
    pub options: ScrapeOptions,
}

fn default_crawl_limit() -> usize {
    10_000
}

fn default_sitemap() -> String {
    "include".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrawlRequest {
    pub url: String,
    #[serde(default)]
    pub include_paths: Vec<String>,
    #[serde(default)]
    pub exclude_paths: Vec<String>,
    #[serde(default)]
    pub max_discovery_depth: Option<usize>,
    #[serde(default = "default_crawl_limit")]
    pub limit: usize,
    #[serde(default)]
    pub crawl_entire_domain: bool,
    #[serde(default)]
    pub allow_external_links: bool,
    #[serde(default)]
    pub allow_subdomains: bool,
    #[serde(default)]
    pub ignore_robots_txt: bool,
    #[serde(default = "default_sitemap")]
    pub sitemap: String,
    #[serde(default)]
    pub ignore_query_parameters: bool,
    /// Seconds.
    #[serde(default)]
    pub delay: Option<f64>,
    #[serde(default)]
    pub max_concurrency: Option<usize>,
    #[serde(default)]
    pub scrape_options: ScrapeOptions,
    #[serde(default)]
    pub webhook: Option<Value>,

    // ---- PageSift extensions ----
    #[serde(default, deserialize_with = "query_or_keywords")]
    pub query: Option<String>,
    #[serde(default, deserialize_with = "adaptive_options")]
    pub adaptive: Option<AdaptiveOptions>,
}

fn query_or_keywords<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Repr {
        Query(String),
        Keywords(Vec<String>),
    }
    Ok(match Option::<Repr>::deserialize(d)? {
        Some(Repr::Query(q)) => Some(q),
        Some(Repr::Keywords(k)) => Some(k.join(" ")),
        None => None,
    }
    .filter(|q| !q.trim().is_empty()))
}

fn adaptive_options<'de, D: Deserializer<'de>>(d: D) -> Result<Option<AdaptiveOptions>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Repr {
        Flag(bool),
        Options(AdaptiveOptions),
    }
    Ok(match Option::<Repr>::deserialize(d)? {
        Some(Repr::Flag(true)) => Some(AdaptiveOptions::default()),
        Some(Repr::Flag(false)) | None => None,
        Some(Repr::Options(o)) => Some(o),
    })
}

/// Adaptive crawl stopping conditions (PageSift extension `adaptive`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AdaptiveOptions {
    pub coverage_threshold: f64,
    pub evidence_per_term: usize,
    pub patience: usize,
    pub min_gain: f64,
}

impl Default for AdaptiveOptions {
    fn default() -> Self {
        Self {
            coverage_threshold: 0.9,
            evidence_per_term: 3,
            patience: 5,
            min_gain: 0.05,
        }
    }
}

fn default_map_limit() -> usize {
    5_000
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MapRequest {
    pub url: String,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default = "yes")]
    pub include_subdomains: bool,
    #[serde(default = "yes")]
    pub ignore_query_parameters: bool,
    #[serde(default = "default_sitemap")]
    pub sitemap: String,
    #[serde(default = "default_map_limit")]
    pub limit: usize,
    #[serde(default)]
    pub ignore_robots_txt: bool,
    /// Milliseconds.
    #[serde(default)]
    pub timeout: Option<u64>,
}

fn default_search_limit() -> usize {
    10
}

/// `/v1/search` and `/v2/search`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest {
    pub query: String,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    /// Time range: `qdr:d`, `qdr:w`, `qdr:m`, `qdr:y`.
    #[serde(default)]
    pub tbs: Option<String>,
    /// Milliseconds.
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default)]
    pub scrape_options: Option<ScrapeOptions>,
}

/// `/v1/extract` and `/v2/extract`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractRequest {
    pub urls: Vec<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub schema: Option<Value>,
    #[serde(default)]
    pub enable_web_search: bool,
    #[serde(default)]
    pub ignore_invalid_urls: bool,
    #[serde(default)]
    pub scrape_options: Option<ScrapeOptions>,
}
