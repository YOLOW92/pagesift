//! Firecrawl v1/v2 response types and metadata structures.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Explanation when a query-directed crawl completes (`pagesift` field in job status).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrawlSummary {
    /// `coverage_reached`, `saturated`, `no_relevant_pages` (adaptive: nothing relevant found and
    /// no lead left to follow), `limit_reached`, `exhausted`, `cancelled`.
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
/// 带查询的爬取里一页的来历（`metadata.pagesift.crawl`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrawlTrace {
    /// 从种子页算起的深度；种子是 0。
    pub depth: usize,
    /// 发现这个链接的页面；种子和 sitemap 里的 URL 没有。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// 链接入队时的分数（越高越先抓）；种子没有。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    /// 附件（PDF、Office 文档）的来源页；普通页面没有。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment_of: Option<String>,
}

/// `metadata.pagesift.ocr`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrMeta {
    pub engine: String,
    pub model: String,
    /// `cpu` 或 `cuda:N`。
    pub device: String,
    /// 文字来自 OCR 的页数。
    pub pages: usize,
    /// 想用 GPU 却退回 CPU 时为真（原因在 `gpu_fallback` 警告里）。
    pub fallback: bool,
}

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
    /// 这次 HTTP 抓取确实按这个档案发（如 `chrome_156`）：站点看到的是这个浏览器，不是 pagesift。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_fingerprint: Option<String>,
    /// Access-barrier recovery attempts, when itna is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub itna: Option<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
    /// Timing per phase in milliseconds.
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub timings: BTreeMap<String, f64>,
    /// OCR：哪个引擎和设备识别了多少页。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ocr: Option<OcrMeta>,
    /// Duplicate canonical reference URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
    /// Query relevance score (0.0 to 1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relevance: Option<f32>,
    /// 带查询的爬取里这一页是怎么来的：深度、父页、入队分数、附件的来源页。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crawl: Option<CrawlTrace>,
    /// Query filter statistics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_filter: Option<QueryFilterStats>,
    /// JSON-LD blocks found on the page, parsed and passed through unchanged.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub json_ld: Vec<Value>,
    /// How the result was produced: one entry per decision (render or not, merge or not) with its inputs.
    /// Only present when the request asks for an explanation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanTrace>,
}

/// `metadata.pagesift.plan`：每一步决策的依据。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanTrace {
    pub steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    /// `render`（是否开浏览器）；`adopt`（是否采用渲染结果）或 `merge`（采用的是 HTTP 与渲染按块合并的结果）。
    pub action: String,
    pub chosen: bool,
    /// 依据来自学到的后验；`false` 表示冷启动时沿用的规则。
    pub learned: bool,
    /// 用来决策的概率（Thompson 抽样值）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p: Option<f64>,
    /// 可得的价值（质量单位）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    /// 资源与时间的代价（质量单位）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charge: Option<f64>,
    /// 这一步之后结果的校准质量分。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<f64>,
    /// 合并时各块的来源：两边都有、只来自 HTTP、只来自渲染。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocks: Option<[usize; 3]>, // [推导] 来源种类数
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
    /// `/v2/scrape`：`/v2/scrape/{id}/interact` 用的 id。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scrape_id: Option<String>,
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
    /// 响应带 `Retry-After`。只用于拦截分类（429/503 的限速判定），不进 JSON、不进缓存。
    #[serde(skip)]
    pub retry_after: bool,
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
    /// 每页的 Markdown：只在 `/v2/parse` 的 PDF 请求了 `parsers: [{type: "pdf", pages: true}]` 时给。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<Vec<PageMarkdown>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<String>>,
    /// `links` 里每个链接的锚文本（同序），爬虫给链接打分用；不进 JSON。
    #[serde(skip)]
    pub link_text: Option<Vec<String>>,
    /// 页面声明的其他语言版本（`hreflang`）：(语言, 地址)。爬虫找英文版用；不进 JSON。
    #[serde(skip)]
    pub alternates: Vec<(String, String)>,
    /// 引擎内部用的校准相对质量分（`pagesift_plan::quality`：只用来比较同一页的结果，不是绝对概率），
    /// 采用渲染结果和按块合并时用；不进 JSON、不进缓存。
    #[serde(skip)]
    pub quality: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    /// PageSift extension: items extracted with the `extract` format's CSS schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract: Option<Value>,
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
    /// PageSift extension: exported files (`exportImages`). JSON lists `{id, format, bytes, hash}`;
    /// the content is downloaded from `GET /v2/attachments/{id}`.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub attachments: Vec<super::Attachment>,
    pub metadata: Metadata,
}

/// Firecrawl `data.pages[]`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageMarkdown {
    /// 从 1 开始的页码。
    pub page_number: usize,
    pub markdown: String,
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
    /// Request fields that were accepted but ignored (`ignored_fields: a, b`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
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
    /// Request fields that were accepted but ignored (`ignored_fields: a, b`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}
