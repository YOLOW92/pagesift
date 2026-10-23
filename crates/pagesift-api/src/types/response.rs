//! Firecrawl v1/v2 response types and metadata structures.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Explanation when a query-directed crawl completes (`pagesift` field in job status).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrawlSummary {
    /// `coverage_reached`, `saturated`, `limit_reached`, `exhausted`, `cancelled`.
    pub stop_reason: String,
    /// Coverage percentage (0.0 to 1.0) when adaptive crawl completes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<f64>,
    /// Number of distinct evidence blocks gathered for each term.
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub term_evidence: BTreeMap<String, usize>,
    /// Actual pages fetched.
    pub pages: usize,
}

/// PageSift extensions inside `metadata.pagesift`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PagesiftMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_hash: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub redirect_chain: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub not_modified: bool,
    /// `http` or `browser`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// `lightpanda`, `moli`, or `chrome`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_engine: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
    /// Timing per phase in milliseconds.
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub timings: BTreeMap<String, f64>,
    /// Duplicate canonical reference URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
    /// Query relevance score (0.0 to 1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relevance: Option<f32>,
    /// Query filter statistics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_filter: Option<QueryFilterStats>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryFilterStats {
    pub kept_blocks: usize,
    pub total_blocks: usize,
    pub tokens_before: usize,
    pub tokens_after: usize,
}

/// Firecrawl `changeTracking` response object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeTracking {
    pub previous_scrape_at: Option<String>,
    /// `new`, `same`, `changed`, `removed`.
    pub change_status: String,
    /// `visible` or `hidden`.
    pub visibility: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pagesift: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keywords: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub robots: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favicon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(rename = "sourceURL", skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    pub status_code: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub proxy_used: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_at: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pagesift: Option<PagesiftMeta>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_tracking: Option<ChangeTracking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunks: Option<Vec<DocChunk>>,
    pub metadata: Metadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocChunk {
    pub text: String,
    pub heading_path: Vec<String>,
    pub start: usize,
    pub end: usize,
    pub tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapLink {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScrapeResponse {
    pub success: bool,
    pub data: Document,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobStarted {
    pub success: bool,
    pub id: String,
    pub url: String,
    #[serde(rename = "invalidURLs", skip_serializing_if = "Option::is_none")]
    pub invalid_urls: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobStatus {
    pub success: bool,
    pub status: String,
    pub completed: usize,
    pub total: usize,
    pub credits_used: usize,
    pub expires_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    pub data: Vec<Document>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pagesift: Option<CrawlSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobError {
    pub id: String,
    pub timestamp: String,
    pub url: String,
    pub code: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobErrors {
    pub errors: Vec<JobError>,
    pub robots_blocked: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapResponse {
    pub success: bool,
    pub links: Vec<MapLink>,
}
