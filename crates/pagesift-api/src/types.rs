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
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
    /// 各阶段耗时，毫秒。
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub timings: BTreeMap<String, f64>,
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
