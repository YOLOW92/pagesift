//! PRD F-7.2 的 5 个工具：定义（参数结构对齐 firecrawl-mcp）和返回示例数据的占位实现。
//!
//! 真正的抓取由上层实现 [`ToolHandler`]，`definition()` 直接返回这里的定义即可：
//!
//! ```text
//! fn definition(&self) -> ToolDef { pagesift_mcp::tools::scrape_def() }
//! ```

use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::tool::{
    BoxFuture, ToolAnnotations, ToolContext, ToolDef, ToolError, ToolHandler, ToolOutput,
    ToolResult,
};

pub const SCRAPE: &str = "pagesift_scrape";
pub const MAP: &str = "pagesift_map";
pub const CRAWL: &str = "pagesift_crawl";
pub const CRAWL_STATUS: &str = "pagesift_crawl_status";
pub const BATCH_SCRAPE: &str = "pagesift_batch_scrape";

/// 单页抓取选项（不含 `url`），对应 firecrawl-mcp 的 `scrapeParamsSchema`，
/// 加上 pagesift 扩展 `engine`、`ifNoneMatch`、`ifModifiedSince`。
pub fn scrape_options_schema() -> Value {
    let str_array = json!({ "type": "array", "items": { "type": "string" } });
    json!({
        "type": "object",
        "properties": {
            "formats": {
                "type": "array",
                "items": {
                    "type": "string",
                    "enum": ["markdown", "html", "rawHtml", "screenshot", "links", "images", "summary",
                             "changeTracking", "branding", "json", "query", "audio"]
                },
                "description": "Output formats. Defaults to [\"markdown\"]."
            },
            "jsonOptions": {
                "type": "object",
                "properties": {
                    "prompt": { "type": "string" },
                    "schema": { "type": "object" }
                },
                "description": "Options for the json format."
            },
            "queryOptions": {
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "maxLength": 10000 },
                    "mode": { "type": "string", "enum": ["directQuote", "freeform"], "default": "freeform" }
                },
                "required": ["prompt"],
                "description": "Options for the query format."
            },
            "screenshotOptions": {
                "type": "object",
                "properties": {
                    "fullPage": { "type": "boolean" },
                    "quality": { "type": "number" },
                    "viewport": {
                        "type": "object",
                        "properties": { "width": { "type": "number" }, "height": { "type": "number" } },
                        "required": ["width", "height"]
                    }
                }
            },
            "parsers": { "type": "array", "items": { "type": "string", "enum": ["pdf"] } },
            "pdfOptions": {
                "type": "object",
                "properties": { "maxPages": { "type": "integer", "minimum": 1, "maximum": 10000 } }
            },
            "onlyMainContent": { "type": "boolean", "description": "Keep only the main content. Defaults to true." },
            "redactPII": { "type": "boolean" },
            "includeTags": str_array,
            "excludeTags": str_array,
            "waitFor": { "type": "number", "description": "Milliseconds to wait before capturing the page." },
            "actions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "type": {
                            "type": "string",
                            "enum": ["wait", "screenshot", "scroll", "scrape", "click", "write", "press",
                                     "executeJavascript", "generatePDF"]
                        },
                        "selector": { "type": "string" },
                        "milliseconds": { "type": "number" },
                        "text": { "type": "string" },
                        "key": { "type": "string" },
                        "direction": { "type": "string", "enum": ["up", "down"] },
                        "script": { "type": "string" },
                        "fullPage": { "type": "boolean" }
                    },
                    "required": ["type"]
                },
                "description": "Browser actions to run before capturing the page."
            },
            "mobile": { "type": "boolean" },
            "skipTlsVerification": { "type": "boolean" },
            "removeBase64Images": { "type": "boolean" },
            "location": {
                "type": "object",
                "properties": {
                    "country": { "type": "string" },
                    "languages": str_array
                }
            },
            "storeInCache": { "type": "boolean" },
            "zeroDataRetention": { "type": "boolean" },
            "maxAge": { "type": "number", "description": "Accept a cached copy at most this many milliseconds old; 0 forces a live fetch." },
            "lockdown": { "type": "boolean" },
            "proxy": {
                "type": "string",
                "enum": ["basic", "stealth", "enhanced", "auto"],
                "description": "Only basic and auto are supported; pagesift does not bypass bot protection."
            },
            "profile": {
                "type": "object",
                "properties": { "name": { "type": "string" }, "saveChanges": { "type": "boolean" } },
                "required": ["name"]
            },
            "timeout": { "type": "integer", "minimum": 1, "description": "Timeout in milliseconds." },
            "engine": {
                "type": "string",
                "enum": ["http", "browser", "auto"],
                "description": "pagesift extension: fetch with plain HTTP, a headless browser, or decide automatically (default)."
            },
            "ifNoneMatch": { "type": "string", "description": "pagesift extension: ETag for a conditional request." },
            "ifModifiedSince": { "type": "string", "description": "pagesift extension: HTTP date for a conditional request." }
        }
    })
}

fn with_url(mut options: Value, url_schema: Value, extra_required: &[&str]) -> Value {
    options["properties"]["url"] = url_schema;
    let mut req = vec![json!("url")];
    req.extend(extra_required.iter().map(|r| json!(r)));
    options["required"] = Value::Array(req);
    options
}

fn annotations(title: &str, read_only: bool) -> ToolAnnotations {
    ToolAnnotations {
        title: Some(title.into()),
        read_only_hint: Some(read_only),
        destructive_hint: Some(false),
        idempotent_hint: None,
        open_world_hint: Some(true),
    }
}

pub fn scrape_def() -> ToolDef {
    let schema = with_url(
        scrape_options_schema(),
        json!({ "type": "string", "format": "uri", "description": "The page to scrape." }),
        &[],
    );
    ToolDef::new(SCRAPE, schema)
        .title("Scrape a page")
        .description(
            "Fetch one URL and return clean content: markdown by default, or html, rawHtml, links, \
             images and metadata. Use it when you know the page you need. For several known URLs use \
             pagesift_batch_scrape; to find URLs on a site use pagesift_map; to collect a whole site use \
             pagesift_crawl. pagesift respects robots.txt and reports blocked pages as errors instead of \
             returning their content.",
        )
        .annotations(annotations("pagesift scrape", true))
}

pub fn map_def() -> ToolDef {
    let schema = json!({
        "type": "object",
        "properties": {
            "url": { "type": "string", "format": "uri", "description": "The site to map." },
            "search": { "type": "string", "description": "Only return URLs related to this term." },
            "sitemap": { "type": "string", "enum": ["include", "skip", "only"] },
            "includeSubdomains": { "type": "boolean" },
            "limit": { "type": "number" },
            "ignoreQueryParameters": { "type": "boolean" }
        },
        "required": ["url"]
    });
    ToolDef::new(MAP, schema)
        .title("Map a site")
        .description(
            "List the URLs of a website from its sitemaps, robots.txt and homepage links, without \
             fetching each page. Use it to find the right page before scraping, or to plan a batch scrape.",
        )
        .annotations(annotations("pagesift map", true))
}

pub fn crawl_def() -> ToolDef {
    let mut scrape_options = scrape_options_schema();
    scrape_options["description"] = json!("Scrape options applied to every crawled page.");
    let schema = json!({
        "type": "object",
        "properties": {
            "url": { "type": "string", "description": "The URL to start crawling from." },
            "prompt": { "type": "string" },
            "excludePaths": { "type": "array", "items": { "type": "string" }, "description": "Regular expressions for paths to skip." },
            "includePaths": { "type": "array", "items": { "type": "string" }, "description": "Regular expressions for paths to keep." },
            "maxDiscoveryDepth": { "type": "number" },
            "sitemap": { "type": "string", "enum": ["skip", "include", "only"] },
            "limit": { "type": "number", "description": "Maximum number of pages to crawl." },
            "allowExternalLinks": { "type": "boolean" },
            "allowSubdomains": { "type": "boolean" },
            "crawlEntireDomain": { "type": "boolean" },
            "delay": { "type": "number", "description": "Seconds to wait between requests to the same host." },
            "maxConcurrency": { "type": "number" },
            "webhook": { "type": "string" },
            "webhookHeaders": { "type": "object", "additionalProperties": { "type": "string" } },
            "deduplicateSimilarURLs": { "type": "boolean" },
            "ignoreQueryParameters": { "type": "boolean" },
            "query": { "type": "string", "description": "Crawl the pages most relevant to this query first." },
            "adaptive": { "type": "boolean", "description": "With query: stop once the collected pages cover it." },
            "attachments": { "type": "boolean", "description": "With query: also fetch PDF/Office documents linked from relevant pages." },
            "attachmentLimit": { "type": "number" },
            "scrapeOptions": scrape_options
        },
        "required": ["url"]
    });
    ToolDef::new(CRAWL, schema)
        .title("Crawl a site")
        .description(
            "Start a crawl job that follows links from a URL and scrapes every page found. Returns a job \
             id right away; poll it with pagesift_crawl_status. Results can be large, so set a \
             conservative limit and use includePaths/excludePaths to stay on topic.",
        )
        .annotations(annotations("pagesift crawl", false))
}

pub fn crawl_status_def() -> ToolDef {
    let schema = json!({
        "type": "object",
        "properties": { "id": { "type": "string", "description": "Job id returned by pagesift_crawl or pagesift_batch_scrape." } },
        "required": ["id"]
    });
    ToolDef::new(CRAWL_STATUS, schema)
        .title("Crawl status")
        .description(
            "Get the status, progress and pages collected so far for a crawl or batch scrape job. \
             Read-only.",
        )
        .annotations(ToolAnnotations {
            open_world_hint: Some(false),
            idempotent_hint: Some(true),
            ..annotations("pagesift crawl status", true)
        })
}

pub fn batch_scrape_def() -> ToolDef {
    let schema = json!({
        "type": "object",
        "properties": {
            "urls": {
                "type": "array",
                "items": { "type": "string", "format": "uri" },
                "minItems": 1,
                "description": "The URLs to scrape."
            },
            "options": scrape_options_schema()
        },
        "required": ["urls"]
    });
    ToolDef::new(BATCH_SCRAPE, schema)
        .title("Batch scrape")
        .description(
            "Scrape a list of known URLs as one job with the same options. Returns a job id; poll it \
             with pagesift_crawl_status.",
        )
        .annotations(annotations("pagesift batch scrape", false))
}

/// 5 个工具的定义，顺序固定。
pub fn definitions() -> Vec<ToolDef> {
    vec![
        scrape_def(),
        map_def(),
        crawl_def(),
        crawl_status_def(),
        batch_scrape_def(),
    ]
}

/// 返回固定示例数据的占位工具，用于测试和联调。
pub fn placeholder_tools() -> Vec<Arc<dyn ToolHandler>> {
    definitions()
        .into_iter()
        .map(|def| Arc::new(Placeholder { def }) as Arc<dyn ToolHandler>)
        .collect()
}

struct Placeholder {
    def: ToolDef,
}

impl ToolHandler for Placeholder {
    fn definition(&self) -> ToolDef {
        self.def.clone()
    }

    fn call(&self, args: Map<String, Value>, ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            let s = |k: &str| {
                args.get(k)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            let out = match self.def.name.as_str() {
                SCRAPE => {
                    ctx.progress(0.0, Some(1.0), Some("fetching"));
                    let url = s("url");
                    ctx.progress(1.0, Some(1.0), Some("done"));
                    json!({
                        "markdown": "# Example Domain\n\nThis is placeholder content from pagesift-mcp.",
                        "metadata": { "sourceURL": url, "url": url, "statusCode": 200, "title": "Example Domain" },
                        "warning": "placeholder result: the scraping engine is not wired in yet"
                    })
                }
                MAP => {
                    let base = s("url").trim_end_matches('/').to_owned();
                    json!({ "links": [
                        { "url": format!("{base}/") },
                        { "url": format!("{base}/about") },
                    ]})
                }
                CRAWL => json!({ "success": true, "id": "placeholder-crawl", "url": s("url") }),
                BATCH_SCRAPE => {
                    json!({ "success": true, "id": "placeholder-batch", "invalidURLs": [] })
                }
                CRAWL_STATUS => {
                    let id = s("id");
                    if !id.starts_with("placeholder-") {
                        return Err(ToolError::new(format!("Unknown job id: {id}"))
                            .with_details(json!({ "code": "INVALID_REQUEST" })));
                    }
                    json!({ "status": "completed", "total": 0, "completed": 0, "data": [] })
                }
                other => return Err(ToolError::new(format!("{other} is not implemented"))),
            };
            Ok(ToolOutput::json(out))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::validate;

    #[test]
    fn definitions_are_well_formed() {
        let defs = definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, [SCRAPE, MAP, CRAWL, CRAWL_STATUS, BATCH_SCRAPE]);
        for d in &defs {
            assert_eq!(d.input_schema["type"], "object", "{}", d.name);
            assert!(d.description.is_some());
        }
        assert_eq!(scrape_def().input_schema["required"], json!(["url"]));
        assert!(
            crawl_def().input_schema["properties"]["scrapeOptions"]["properties"]["url"].is_null()
        );
    }

    #[test]
    fn firecrawl_style_arguments_validate() {
        let ok = json!({
            "url": "https://example.com",
            "formats": ["markdown", "links"],
            "onlyMainContent": true,
            "waitFor": 500,
            "location": { "country": "US", "languages": ["en"] },
            "engine": "http"
        });
        validate(&scrape_def().input_schema, &ok).unwrap();
        let ok = json!({ "urls": ["https://a.example", "https://b.example"], "options": { "formats": ["html"] } });
        validate(&batch_scrape_def().input_schema, &ok).unwrap();
        let ok = json!({ "url": "https://example.com", "limit": 10, "scrapeOptions": { "formats": ["markdown"] } });
        validate(&crawl_def().input_schema, &ok).unwrap();

        assert!(
            validate(
                &scrape_def().input_schema,
                &json!({"url": "https://a", "formats": ["pdf"]})
            )
            .is_err()
        );
        assert!(validate(&batch_scrape_def().input_schema, &json!({"urls": []})).is_err());
        assert!(validate(&crawl_status_def().input_schema, &json!({})).is_err());
    }

    #[tokio::test]
    async fn placeholders_return_examples() {
        let tools = placeholder_tools();
        let args = |v: Value| v.as_object().unwrap().clone();
        let out = tools[0]
            .call(
                args(json!({"url": "https://example.com"})),
                ToolContext::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            out.structured_content.unwrap()["metadata"]["statusCode"],
            200
        );
        let err = tools[3]
            .call(args(json!({"id": "nope"})), ToolContext::default())
            .await
            .unwrap_err();
        assert!(err.message.contains("Unknown job id"));
    }
}
