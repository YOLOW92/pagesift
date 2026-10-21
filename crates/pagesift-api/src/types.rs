//! Firecrawl v2 的请求和响应类型。字段名和默认值以 Firecrawl 源码
//! `apps/api/src/controllers/v2/types.ts` 为准；pagesift 自己的扩展字段单独标注。
//! 请求里的未知字段一律忽略（Firecrawl 会拒绝，我们宽松一些，方便旧客户端）。

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

/// `formats` 的一项：Firecrawl 同时接受 `"markdown"` 和 `{"type": "markdown", ...}` 两种写法。
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

fn default_formats() -> Vec<Format> {
    vec![Format::new("markdown")]
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Location {
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub languages: Vec<String>,
}

/// 单页抓取选项（Firecrawl 的 `scrapeOptions`）。
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
    /// 毫秒。
    #[serde(default)]
    pub timeout: Option<u64>,
    /// 毫秒。
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
    /// `basic`、`auto` 放行，其余取值返回 UNSUPPORTED_OPTION。
    #[serde(default)]
    pub proxy: Option<String>,
    /// pagesift 扩展：走服务端配置的哪个命名出口（`[[egress]]`）。只用于 HTTP 抓取；
    /// 指定了出口时不升级到浏览器（浏览器不走这个出口，出口地址会不一致）。
    #[serde(default)]
    pub egress: Option<String>,
    /// Firecrawl 默认跳过 TLS 校验；pagesift 始终校验证书，这个字段只接受、不生效。
    #[serde(default)]
    pub skip_tls_verification: Option<bool>,
    /// 毫秒。
    #[serde(default)]
    pub max_age: Option<u64>,
    /// 毫秒。
    #[serde(default)]
    pub min_age: Option<u64>,
    #[serde(default = "yes")]
    pub store_in_cache: bool,

    // ---- pagesift 扩展 ----
    /// `http`、`browser`、`auto`（默认）。
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub if_none_match: Option<String>,
    #[serde(default)]
    pub if_modified_since: Option<String>,
    /// 单页抓取可以由调用方关闭 robots 检查（PRD F-2.4）。
    #[serde(default)]
    pub ignore_robots_txt: bool,
    #[serde(default)]
    pub store_snapshot: bool,
    /// 只保留正文里和查询相关的段落（BM25），见 [`QueryFilter`]。不给时输出完整正文。
    #[serde(default)]
    pub query_filter: Option<QueryFilter>,
}

fn default_filter_threshold() -> f32 {
    1.0
}

/// pagesift 扩展 `queryFilter`：按查询过滤正文 Markdown，省 token。
/// 可以直接写查询字符串，也可以写 `{"query": "...", "threshold": 1.0, "topK": 5, "headings": true}`。
/// `summary`、`json` 和 `changeTracking` 都基于过滤后的正文。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryFilter {
    pub query: String,
    /// BM25 分数不低于它的段落才保留，默认 1.0（同 Crawl4AI 的 `bm25_threshold`）。
    pub threshold: f32,
    /// 最多保留分数最高的几段。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<usize>,
    /// 在保留的段落前补上它所在的各级标题，默认开。
    pub headings: bool,
}

impl<'de> Deserialize<'de> for QueryFilter {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Full {
            query: String,
            #[serde(default = "default_filter_threshold")]
            threshold: f32,
            #[serde(default)]
            top_k: Option<usize>,
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
                threshold: default_filter_threshold(),
                top_k: None,
                headings: true,
            },
            Repr::Full(f) => QueryFilter {
                query: f.query,
                threshold: f.threshold,
                top_k: f.top_k,
                headings: f.headings,
            },
        })
    }
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
    /// 秒。
    #[serde(default)]
    pub delay: Option<f64>,
    #[serde(default)]
    pub max_concurrency: Option<usize>,
    #[serde(default)]
    pub scrape_options: ScrapeOptions,
    #[serde(default)]
    pub webhook: Option<Value>,

    // ---- pagesift 扩展 ----
    /// 相关性优先爬取：给了查询（字符串，或关键词数组）时，按链接和查询的相关性决定先爬哪页，
    /// 不再按广度优先。不给时行为和 Firecrawl 一样。
    #[serde(default, deserialize_with = "query_or_keywords")]
    pub query: Option<String>,
    /// 自适应爬取：信息够了就提前停止，需要同时给 `query`。写 `true` 用默认参数。
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

/// 自适应爬取的停止条件（pagesift 扩展 `adaptive`）。
///
/// 每个查询词的"证据"是包含它的、互不相同的相关段落数；覆盖度是各词
/// `min(1, 证据 / evidencePerTerm)` 的平均值。覆盖度达到 `coverageThreshold` 时停止
/// （`coverage_reached`）；或者连续 `patience` 页带来的新相关内容都少于已收集的 `minGain`
/// 时停止（`saturated`）。
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

/// 带查询的爬取结束时的说明，放在任务状态的 `pagesift` 字段里。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrawlSummary {
    /// `coverage_reached`、`saturated`（自适应提前停止），`limit_reached`、`exhausted`（没有更多页面）、`cancelled`。
    pub stop_reason: String,
    /// 自适应爬取结束时的覆盖度，0 到 1。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<f64>,
    /// 每个查询词（词干）收集到的证据段落数。
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub term_evidence: BTreeMap<String, usize>,
    /// 实际抓取的页面数。
    pub pages: usize,
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
    /// 毫秒。
    #[serde(default)]
    pub timeout: Option<u64>,
}

fn default_search_limit() -> usize {
    10
}

/// `/v2/search`。只有 `scrapeOptions` 里给了 formats 时才抓取搜索结果页面（和 Firecrawl 一致）。
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
    /// 时间范围：`qdr:d`、`qdr:w`、`qdr:m`、`qdr:y`。
    #[serde(default)]
    pub tbs: Option<String>,
    /// 毫秒。
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default)]
    pub scrape_options: Option<ScrapeOptions>,
}

/// `/v2/extract`：从一组页面里按 schema 或提示提取结构化数据。URL 以 `/*` 结尾时先 map 整站。
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

/// pagesift 在 `metadata.pagesift` 里附加的字段。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PagesiftMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 正文 Markdown、原始 HTML、规范化纯文本的 BLAKE3 哈希（小写十六进制）。
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
    /// `http` 或 `browser`：这一页实际用哪种方式抓到的。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// 浏览器渲染时实际用的内核：`lightpanda`、`moli` 或 `chrome`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_engine: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
    /// 各阶段耗时，毫秒。
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub timings: BTreeMap<String, f64>,
    /// 整站爬取里，这一页的正文和之前抓到的某一页完全相同（同一内容的另一个 URL）：那一页的 URL。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
    /// 带查询的爬取里，这一页和查询的相关度（0 到 1）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relevance: Option<f32>,
    /// `queryFilter` 的效果：保留了几段、原来几段，过滤前后的 token 估算。
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

/// Firecrawl 的 `changeTracking` 结果（格式 `{"type": "changeTracking", "modes": [...], "tag": ...}`）。
/// 和上一次抓取同一 URL（同一 `tag`）的正文 Markdown 比较。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeTracking {
    /// 上一次抓取的时间；第一次抓取时是 null。
    pub previous_scrape_at: Option<String>,
    /// `new`、`same`、`changed`、`removed`。
    pub change_status: String,
    /// `visible`（通过链接或 sitemap 发现）或 `hidden`。pagesift 不从历史里发现页面，总是 `visible`。
    pub visibility: String,
    /// `git-diff` 模式：`{"text": 统一格式文本, "json": parse-diff 结构}`，只在 `changed` 时给。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<Value>,
    /// `json` 模式：按 schema 提取前后两版，变了的字段给出 `{"previous", "current"}`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    /// pagesift 扩展：块级 diff 的结论，区分噪声变化（日期、计数等）和实质变化。
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
    /// pagesift 扩展：作者，依次取 JSON-LD 和 meta。
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
    /// pagesift 只有 basic。
    pub proxy_used: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_at: Option<String>,
    /// og:*、twitter:*、dc.* 这类，键名按 Firecrawl 的 camelCase 风格（如 `ogImage`）。
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
    pub metadata: Metadata,
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
    /// `scraping`、`completed`、`failed`、`cancelled`。
    pub status: String,
    pub completed: usize,
    pub total: usize,
    pub credits_used: usize,
    pub expires_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    pub data: Vec<Document>,
    /// pagesift 扩展：带查询的爬取结束后的说明（停止原因、覆盖度）。
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formats_accept_strings_and_objects() {
        let r: ScrapeRequest = serde_json::from_value(json!({
            "url": "https://example.com",
            "formats": ["markdown", {"type": "screenshot", "fullPage": true}],
        }))
        .unwrap();
        assert_eq!(r.options.formats[0], Format::new("markdown"));
        assert_eq!(r.options.formats[1].kind, "screenshot");
        assert_eq!(r.options.formats[1].options["fullPage"], json!(true));
    }

    #[test]
    fn defaults_follow_firecrawl_and_unknown_fields_are_ignored() {
        let r: ScrapeRequest = serde_json::from_value(json!({
            "url": "https://example.com",
            "origin": "python-sdk",
            "integration": null,
            "zeroDataRetention": false,
        }))
        .unwrap();
        let o = r.options;
        assert_eq!(o.formats, vec![Format::new("markdown")]);
        assert!(o.only_main_content && o.block_ads && o.store_in_cache && o.remove_base64_images);
        assert_eq!(o.wait_for, 0);
        assert!(!o.ignore_robots_txt);
    }

    #[test]
    fn crawl_and_map_defaults() {
        let c: CrawlRequest =
            serde_json::from_value(json!({"url": "https://example.com"})).unwrap();
        assert_eq!((c.limit, c.sitemap.as_str()), (10_000, "include"));
        assert!(c.scrape_options.only_main_content);
        let m: MapRequest = serde_json::from_value(json!({"url": "https://example.com"})).unwrap();
        assert_eq!(m.limit, 5_000);
        assert!(m.include_subdomains && m.ignore_query_parameters);
    }

    #[test]
    fn query_filter_accepts_string_or_object() {
        let r: ScrapeRequest = serde_json::from_value(json!({
            "url": "https://example.com", "queryFilter": "refund policy",
        }))
        .unwrap();
        let f = r.options.query_filter.unwrap();
        assert_eq!(
            (f.query.as_str(), f.threshold, f.top_k, f.headings),
            ("refund policy", 1.0, None, true)
        );
        let r: ScrapeRequest = serde_json::from_value(json!({
            "url": "https://example.com",
            "queryFilter": {"query": "q", "threshold": 0.5, "topK": 3, "headings": false},
        }))
        .unwrap();
        let f = r.options.query_filter.unwrap();
        assert_eq!((f.threshold, f.top_k, f.headings), (0.5, Some(3), false));
        assert!(ScrapeOptions::default().query_filter.is_none());
    }

    #[test]
    fn crawl_query_and_adaptive_options() {
        let c: CrawlRequest =
            serde_json::from_value(json!({"url": "https://example.com"})).unwrap();
        assert_eq!((c.query, c.adaptive), (None, None));
        let c: CrawlRequest = serde_json::from_value(json!({
            "url": "https://example.com", "query": ["refund", "policy"], "adaptive": true,
        }))
        .unwrap();
        assert_eq!(c.query.as_deref(), Some("refund policy"));
        assert_eq!(c.adaptive, Some(AdaptiveOptions::default()));
        let c: CrawlRequest = serde_json::from_value(json!({
            "url": "https://example.com", "query": "  ", "adaptive": {"patience": 2},
        }))
        .unwrap();
        assert_eq!(c.query, None, "blank query is no query");
        assert_eq!(c.adaptive.unwrap().patience, 2);
        let c: CrawlRequest = serde_json::from_value(json!({
            "url": "https://example.com", "query": null, "adaptive": false,
        }))
        .unwrap();
        assert_eq!((c.query, c.adaptive), (None, None));
    }

    #[test]
    fn change_tracking_serializes_firecrawl_fields() {
        let doc = Document {
            change_tracking: Some(ChangeTracking {
                previous_scrape_at: None,
                change_status: "new".into(),
                visibility: "visible".into(),
                diff: None,
                json: None,
                pagesift: None,
            }),
            ..Default::default()
        };
        let v = serde_json::to_value(&doc).unwrap();
        assert_eq!(
            v["changeTracking"],
            json!({"previousScrapeAt": null, "changeStatus": "new", "visibility": "visible"})
        );
        assert!(
            serde_json::to_value(Document::default())
                .unwrap()
                .get("changeTracking")
                .is_none()
        );
    }

    #[test]
    fn metadata_serializes_firecrawl_names_and_extension() {
        let mut meta = Metadata {
            source_url: Some("https://example.com/a".into()),
            status_code: 200,
            proxy_used: "basic".into(),
            pagesift: Some(PagesiftMeta {
                content_hash: Some("ab".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        meta.extra
            .insert("ogImage".into(), json!("https://example.com/i.png"));
        let v = serde_json::to_value(&meta).unwrap();
        assert_eq!(v["sourceURL"], "https://example.com/a");
        assert_eq!(v["statusCode"], 200);
        assert_eq!(v["proxyUsed"], "basic");
        assert_eq!(v["ogImage"], "https://example.com/i.png");
        assert_eq!(v["pagesift"]["contentHash"], "ab");
        assert!(v.get("title").is_none());
    }
}
