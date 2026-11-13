//! PRD F-7.2 的 5 个工具：定义（参数名对齐 firecrawl-mcp，取值范围按引擎能力生成）和返回示例数据的占位实现。
//!
//! 真正的抓取由上层实现 [`ToolHandler`]，`definition()` 直接返回这里的定义即可：
//!
//! ```text
//! fn definition(&self) -> ToolDef { pagesift_mcp::tools::scrape_def(&caps) }
//! ```

use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::options::Capabilities;
use crate::output;
use crate::tool::{
    BoxFuture, ToolAnnotations, ToolContext, ToolDef, ToolError, ToolHandler, ToolOutput,
    ToolResult,
};

pub const SCRAPE: &str = "pagesift_scrape";
pub const MAP: &str = "pagesift_map";
pub const CRAWL: &str = "pagesift_crawl";
pub const POLL_JOB: &str = "pagesift_poll_job";
pub const CANCEL_JOB: &str = "pagesift_cancel_job";
pub const BATCH_SCRAPE: &str = "pagesift_batch_scrape";
pub const INTERACT: &str = "pagesift_interact";
pub const INTERACT_STOP: &str = "pagesift_interact_stop";
pub const AGENT: &str = "pagesift_agent";
pub const AGENT_STATUS: &str = "pagesift_agent_status";
pub const GRAPH_QUERY: &str = "pagesift_graph_query";

/// 单页抓取选项（不含 `url`），由引擎能力生成，见 [`crate::options`]。
pub fn scrape_options_schema(caps: &Capabilities) -> Value {
    crate::options::scrape_options_schema(caps)
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
        // 只读工具重复调用没有额外副作用；会启动任务或改页面的工具不标。
        idempotent_hint: read_only.then_some(true),
        open_world_hint: Some(true),
    }
}

pub fn scrape_def(caps: &Capabilities) -> ToolDef {
    let schema = with_url(
        scrape_options_schema(caps),
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
             returning their content. Results are cached for 48 hours by default; set maxAge to 0 to \
             force a live fetch. With keepSession the browser page stays open for pagesift_interact and \
             its id is returned as metadata.scrapeId; kept pages expire after 10 minutes (sooner when \
             idle under memory pressure). Advanced options: actions runs browser steps before capture, \
             chunking splits Markdown, queryFilter keeps blocks relevant to a query, and parsers \
             selects PDF pages.",
        )
        .output_schema(output::document())
        .annotations(annotations("pagesift scrape", true))
}

/// 会改页面状态（点击可能提交表单），不是只读，也不保证无害。
fn acting_annotations(title: &str) -> ToolAnnotations {
    ToolAnnotations {
        title: Some(title.into()),
        read_only_hint: Some(false),
        destructive_hint: Some(true),
        idempotent_hint: Some(false),
        open_world_hint: Some(true),
    }
}

/// `actions` 是浏览器实际支持的动作类型。
pub fn interact_def(actions: &[String]) -> ToolDef {
    let schema = json!({
        "type": "object",
        "properties": {
            "scrapeId": { "type": "string", "description": "metadata.scrapeId of a pagesift_scrape call with keepSession." },
            "prompt": { "type": "string", "description": "What to do on the page, in plain language (needs an LLM on the server). Send prompt or actions." },
            "actions": { "type": "array", "items": crate::options::action_schema(actions), "description": "Browser actions to run on the page. Send prompt or actions." },
            // [协议] Firecrawl API reference：timeout 1–300 秒
            "timeout": { "type": "number", "minimum": 1, "maximum": 300, "description": "Seconds, default 30." }
        },
        "required": ["scrapeId"],
        "additionalProperties": false
    });
    ToolDef::new(INTERACT, schema)
        .title("Act on a kept page")
        .description(
            "Continue on a browser page kept by pagesift_scrape with keepSession. Send either prompt \
             (plain-language instruction; needs an LLM on the server) or actions (a list of browser \
             actions), not both. Actions can permanently change the page or lose unsaved state. State \
             carries over between calls. Returns the page afterwards in data; with a prompt also output \
             (the answer) and steps. Finish with pagesift_interact_stop to free the page. No live view or \
             CDP URL is offered.",
        )
        .output_schema(output::interact())
        .annotations(acting_annotations("pagesift interact"))
}

pub fn interact_stop_def() -> ToolDef {
    let schema = json!({
        "type": "object",
        "properties": {
            "scrapeId": { "type": "string", "description": "metadata.scrapeId of the kept page." }
        },
        "required": ["scrapeId"],
        "additionalProperties": false
    });
    ToolDef::new(INTERACT_STOP, schema)
        .title("Close a kept page")
        .description(
            "Close a browser page kept by pagesift_scrape keepSession or pagesift_interact and free its \
             memory. Data already returned stays with the caller; the scrapeId becomes invalid, so later \
             calls with it fail. Calling stop twice or after expiry returns an error. Pages expire \
             automatically after 10 minutes if never stopped, and are reclaimed sooner when idle under \
             memory pressure.",
        )
        .output_schema(output::interact_stopped())
        .annotations(acting_annotations("pagesift interact stop"))
}

pub fn map_def() -> ToolDef {
    let schema = json!({
        "type": "object",
        "properties": {
            "url": { "type": "string", "format": "uri", "description": "The site to map." },
            "search": { "type": "string", "description": "Only rank URLs related to this term first; all other URLs stay in the list after them." },
            "sitemap": {
                "type": "string",
                "enum": ["include", "skip", "only"],
                "description": "include (default): use sitemaps plus homepage links; skip: ignore sitemaps; only: use sitemaps and skip the homepage."
            },
            "includeSubdomains": { "type": "boolean", "description": "Include other subdomains of the same site. Defaults to true." },
            "limit": { "type": "integer", "minimum": 1, "description": "Maximum URLs to return after ranking. Defaults to 5000." },
            "ignoreQueryParameters": { "type": "boolean", "description": "Treat URLs that differ only by query string as one. Defaults to true." },
            "timeout": { "type": "integer", "minimum": 1, "description": "Timeout in milliseconds." }
        },
        "required": ["url"],
        "additionalProperties": false
    });
    ToolDef::new(MAP, schema)
        .title("Map a site")
        .description(
            "List a website's URLs from its sitemaps, robots.txt and homepage links without fetching \
             each page. sitemap include (default) uses both sitemaps and page links, skip ignores \
             sitemaps, only uses sitemaps without reading the homepage. If no sitemap is found it falls \
             back to homepage links; limit truncates after search ranking. Use pagesift_scrape for one \
             page and pagesift_crawl to fetch many pages.",
        )
        .output_schema(output::map())
        .annotations(annotations("pagesift map", true))
}

pub fn crawl_def(caps: &Capabilities) -> ToolDef {
    let mut scrape_options = scrape_options_schema(caps);
    scrape_options["description"] = json!("Scrape options applied to every crawled page.");
    let schema = json!({
        "type": "object",
        "properties": {
            "url": { "type": "string", "description": "The URL to start crawling from." },
            "excludePaths": { "type": "array", "items": { "type": "string" }, "description": "Regular expressions for paths to skip." },
            "includePaths": { "type": "array", "items": { "type": "string" }, "description": "Regular expressions for paths to keep." },
            "maxDiscoveryDepth": { "type": "integer", "minimum": 0, "description": "How many links away from the seed to follow. Defaults to no limit." },
            "maxDepth": { "type": "integer", "minimum": 0, "description": "Firecrawl v1 name; only checked against the limit, maxDiscoveryDepth sets the depth." },
            "sitemap": {
                "type": "string",
                "enum": ["skip", "include", "only"],
                "description": "include (default): crawl sitemap URLs and follow page links; skip: ignore sitemaps; only: crawl sitemap URLs without following page links."
            },
            "limit": { "type": "integer", "minimum": 1, "description": "Maximum number of pages to crawl. Defaults to 10000." },
            "allowExternalLinks": { "type": "boolean", "description": "Follow links to other hosts. Defaults to false." },
            "allowSubdomains": { "type": "boolean", "description": "Follow links to other subdomains of the same site. Defaults to true only when query is set." },
            "crawlEntireDomain": { "type": "boolean", "description": "Crawl the whole host instead of the seed's path scope. Defaults to false." },
            "delay": { "type": "number", "description": "Seconds to wait between requests to the same host." },
            "maxConcurrency": { "type": "integer", "minimum": 1, "description": "Lower the job's share of fetch concurrency; it cannot exceed the server's per-host or global limit." },
            "webhook": { "type": ["string", "object"], "description": "URL, or {url, headers, metadata, events}." },
            "ignoreQueryParameters": { "type": "boolean", "description": "Treat URLs that differ only by query string as one. Defaults to false." },
            "scope": { "type": "string", "enum": ["auto", "exact", "parent", "domain"], "description": "How far the crawl may leave the seed's directory; auto widens one level when the seed looks like a leaf page." },
            "query": { "type": "string", "description": "Crawl the pages most relevant to this query first." },
            "adaptive": { "type": ["boolean", "object"], "description": "With query: stop once the collected pages cover it; an object sets coverageThreshold, evidencePerTerm, patience, minGain." },
            "attachments": { "type": "boolean", "description": "With query: also fetch PDF/Office documents linked from relevant pages." },
            "attachmentLimit": { "type": "integer", "minimum": 0, "description": "Maximum attachments to fetch; defaults to limit." },
            "graph": {
                "type": ["boolean", "object"],
                "properties": {
                    "enabled": { "type": "boolean", "description": "Enable DataGraph provenance, citations, version lineage, evidence, facts, and conflicts." },
                    "blocks": { "type": "boolean", "description": "Index stable block references for provenance and evidence alignment." },
                    "citations": { "type": "boolean", "description": "Extract document-to-document citation edges." },
                    "provenance": { "type": "boolean", "description": "Record redirect, canonical, alternate, duplicate, and attachment provenance." },
                    "versions": { "type": "boolean", "description": "Link consecutive versions of the same logical resource." },
                    "evidence": { "type": "boolean", "description": "Bind extracted fields to supporting block spans." },
                    "facts": { "type": "boolean", "description": "Normalize and corroborate claims across multiple sources." },
                    "conflicts": { "type": "boolean", "description": "Detect and record cross-source value conflicts." },
                    "subjectKeys": { "type": "array", "items": { "type": "string", "description": "Field name used to key entities." }, "description": "Entity identifier keys for cross-source matching, e.g. [\"sku\"]." }
                },
                "description": "Build a site-wide DataGraph summary across crawled pages: true or an object with toggles and subjectKeys."
            },
            "scrapeOptions": scrape_options
        },
        "required": ["url"],
        "additionalProperties": false
    });
    ToolDef::new(CRAWL, schema)
        .title("Crawl a site")
        .description(
            "Start a crawl job that follows links from a URL and scrapes every page found. Use \
             pagesift_scrape when you need one page and pagesift_map when you only need URLs. By default \
             the crawl includes sitemap URLs, follows links within the seed's path scope, has no depth \
             limit and uses per-host concurrency; maxDiscoveryDepth and maxConcurrency lower these. \
             Pages blocked by robots.txt or failing are reported in the job's errors and the crawl \
             continues. Returns a job id right away; poll it with pagesift_poll_job. Results can be \
             large, so set a conservative limit and use includePaths/excludePaths to stay on topic.",
        )
        .output_schema(output::job_started())
        .annotations(annotations("pagesift crawl", false))
}

pub fn poll_job_def() -> ToolDef {
    let schema = json!({
        "type": "object",
        "properties": { "id": { "type": "string", "description": "Job id returned by pagesift_crawl or pagesift_batch_scrape." } },
        "required": ["id"]
    });
    ToolDef::new(POLL_JOB, schema)
        .title("Poll a job")
        .description(
            "Get the status, progress and pages collected so far for a pagesift_crawl or \
             pagesift_batch_scrape job. status is scraping, completed, cancelled or failed; completed, \
             cancelled and failed are final. While scraping, poll again after a few seconds; stop when \
             the status is final. Pages collected so far are in data and remain available for 24 hours. \
             Use pagesift_cancel_job to stop a running job.",
        )
        .output_schema(output::job_status())
        .annotations(ToolAnnotations {
            open_world_hint: Some(false),
            idempotent_hint: Some(true),
            ..annotations("pagesift poll job", true)
        })
}

/// `pagesift_report_issue` 的输出 schema（定义在 `pagesift` crate，因为要写审计日志）。
pub fn report_issue_schema() -> Value {
    output::report_issue()
}

pub fn cancel_job_def() -> ToolDef {
    let schema = json!({
        "type": "object",
        "properties": { "id": { "type": "string", "description": "Job id returned by pagesift_crawl or pagesift_batch_scrape." } },
        "required": ["id"]
    });
    ToolDef::new(CANCEL_JOB, schema)
        .title("Cancel a job")
        .description(
            "Cancel a running pagesift_crawl or pagesift_batch_scrape job. Use pagesift_poll_job to read \
             a job and pagesift_cancel_job to stop it. No new pages are scheduled; pages already \
             collected stay available through pagesift_poll_job, and work that finishes after the \
             cancel is discarded. A completed, failed or already cancelled job is left unchanged and \
             its current status is returned, so repeated calls are safe. An unknown or expired id is an \
             error.",
        )
        .output_schema(output::job_cancelled())
        .annotations(ToolAnnotations {
            title: Some("pagesift cancel job".into()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(false),
        })
}

/// 只在配了 LLM 时注册，所以不在 [`definitions`] 里。
pub fn agent_def() -> ToolDef {
    let schema = json!({
        "type": "object",
        "properties": {
            "prompt": { "type": "string", "maxLength": 10000, "description": "What to find out." }, // [协议] Firecrawl 文档的上限
            "urls": { "type": "array", "items": { "type": "string", "format": "uri" }, "description": "Pages to start from. Needed when no search backend is configured." },
            "schema": { "type": "object", "description": "JSON Schema of the answer." },
            "effort": { "type": "string", "enum": ["low", "medium", "high"], "description": "Budget ceiling: pages, model calls and tokens. Default medium." },
            "strictConstrainToURLs": { "type": "boolean", "description": "Open only the pages in urls." },
            "maxCredits": { "type": "integer", "minimum": 1, "description": "Stop after this many pages, searches and maps." },
            "allowedHosts": { "type": "array", "items": { "type": "string" }, "description": "Open only pages on these hosts and their subdomains." },
        },
        "required": ["prompt"],
        "additionalProperties": false,
    });
    ToolDef::new(AGENT, schema)
        .title("Research agent")
        .description(
            "Answer a question from the web when you do not know the URLs: the configured LLM plans \
             searches and page reads, and the run stops when new pages add nothing or the budget \
             ends. Respects robots.txt. Returns a job id right away; poll it with \
             pagesift_agent_status. Uses model tokens; for one known page use pagesift_scrape.",
        )
        .output_schema(output::agent_started())
        .annotations(annotations("pagesift agent", true))
}

pub fn agent_status_def() -> ToolDef {
    let schema = json!({
        "type": "object",
        "properties": {
            "id": { "type": "string", "description": "Job id returned by pagesift_agent." },
            "includeEvidence": { "type": "boolean", "description": "Add the archived evidence text." },
        },
        "required": ["id"],
        "additionalProperties": false,
    });
    ToolDef::new(AGENT_STATUS, schema)
        .title("Agent status")
        .description(
            "Get the status of a pagesift_agent job: data (or partial and stopReason when it did not \
             finish), sources, fieldSources (which source each field came from) and usage.",
        )
        .output_schema(output::agent_status())
        .annotations(ToolAnnotations {
            open_world_hint: Some(false),
            idempotent_hint: Some(true),
            ..annotations("pagesift agent status", true)
        })
}

pub fn batch_scrape_def(caps: &Capabilities) -> ToolDef {
    let mut options = scrape_options_schema(caps);
    options["description"] =
        json!("Scrape options applied to every URL; there are no per-URL overrides.");
    let schema = json!({
        "type": "object",
        "properties": {
            "urls": {
                "type": "array",
                "items": { "type": "string", "format": "uri" },
                "minItems": 1,
                "maxItems": 10000,
                "description": "The URLs to scrape, 1 to 10000."
            },
            "options": options
        },
        "required": ["urls"],
        "additionalProperties": false
    });
    ToolDef::new(BATCH_SCRAPE, schema)
        .title("Batch scrape")
        .description(
            "Scrape a list of known URLs as one job with the same options. Use pagesift_scrape for one \
             URL and pagesift_crawl when the URLs are not known yet. Invalid URLs are returned in \
             invalidURLs instead of failing the job; a page that fails is recorded in the job's errors \
             while the other pages still return. All requests share the server's concurrency and \
             per-host rate limits. urls accepts up to 10000 entries and options applies to every URL \
             (no per-URL overrides). Returns a job id; poll it with pagesift_poll_job.",
        )
        .output_schema(output::job_started())
        .annotations(annotations("pagesift batch scrape", false))
}

pub fn graph_query_def() -> ToolDef {
    let schema = json!({
        "type": "object",
        "properties": {
            "jobId": { "type": "string", "description": "Crawl job id from pagesift_crawl to summarize its DataGraph." },
            "historyUrl": { "type": "string", "description": "URL or normalized resource key to query historical versions and diff edges." },
            "conflicts": { "type": "boolean", "description": "When true, return detected cross-source conflicts instead of a single job or URL." },
            "subject": { "type": "string", "description": "Optional subject key filter for conflicts (e.g. sku:PRO-1); implies conflicts=true when set alone." }
        },
        "additionalProperties": false
    });
    ToolDef::new(GRAPH_QUERY, schema)
        .title("Query DataGraph")
        .description(
            "Query PageSift DataGraph results produced during crawls or archived in the store: \
             pass jobId for a crawl's GraphSummary, historyUrl for cross-time version lineage, \
             or conflicts/subject for cross-source conflicts.",
        )
        .output_schema(output::graph_query())
        .annotations(ToolAnnotations {
            open_world_hint: Some(false),
            idempotent_hint: Some(true),
            ..annotations("pagesift graph query", true)
        })
}

/// 基础工具的定义，顺序固定。
pub fn definitions(caps: &Capabilities) -> Vec<ToolDef> {
    vec![
        scrape_def(caps),
        map_def(),
        crawl_def(caps),
        poll_job_def(),
        cancel_job_def(),
        batch_scrape_def(caps),
    ]
}

/// 返回固定示例数据的占位工具，用于测试和联调。
pub fn placeholder_tools() -> Vec<Arc<dyn ToolHandler>> {
    definitions(&Capabilities::basic())
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
                        "metadata": { "sourceURL": url, "url": url, "statusCode": 200, "proxyUsed": "basic", "title": "Example Domain" },
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
                CRAWL => {
                    json!({ "success": true, "id": "placeholder-crawl", "status": "scraping", "url": s("url") })
                }
                BATCH_SCRAPE => {
                    json!({ "success": true, "id": "placeholder-batch", "status": "scraping", "invalidURLs": [] })
                }
                POLL_JOB => {
                    let id = s("id");
                    if !id.starts_with("placeholder-") {
                        return Err(ToolError::new(format!("Unknown job id: {id}"))
                            .with_details(json!({ "code": "INVALID_REQUEST" })));
                    }
                    json!({ "status": "completed", "total": 0, "completed": 0, "expiresAt": "2099-01-01T00:00:00Z", "data": [] })
                }
                CANCEL_JOB => {
                    let id = s("id");
                    if !id.starts_with("placeholder-") {
                        return Err(ToolError::new(format!("Unknown job id: {id}"))
                            .with_details(json!({ "code": "INVALID_REQUEST" })));
                    }
                    json!({ "success": true, "status": "cancelled", "total": 0, "completed": 0 })
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
        let caps = Capabilities::basic();
        let defs = definitions(&caps);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            [SCRAPE, MAP, CRAWL, POLL_JOB, CANCEL_JOB, BATCH_SCRAPE]
        );
        for d in &defs {
            assert_eq!(d.input_schema["type"], "object", "{}", d.name);
            assert!(d.description.is_some());
            assert!(d.output_schema.is_some(), "{} has no outputSchema", d.name);
        }
        assert!(interact_def(&["click".into()]).output_schema.is_some());
        assert!(interact_stop_def().output_schema.is_some());
        assert!(agent_def().output_schema.is_some());
        assert!(agent_status_def().output_schema.is_some());
        assert!(graph_query_def().output_schema.is_some());
        for d in &defs {
            let out = d.output_schema.as_ref().unwrap();
            assert_eq!(out["type"], "object", "{} outputSchema", d.name);
        }
        assert_eq!(scrape_def(&caps).input_schema["required"], json!(["url"]));
        assert!(
            crawl_def(&caps).input_schema["properties"]["scrapeOptions"]["properties"]["url"]
                .is_null()
        );
    }

    /// 工具参数（含嵌套对象和数组元素）都有说明。
    #[test]
    fn every_tool_parameter_has_a_description() {
        fn walk(schema: &Value, path: &str) {
            if let Some(props) = schema.get("properties").and_then(Value::as_object) {
                for (k, v) in props {
                    let here = format!("{path}.{k}");
                    let described = v
                        .get("description")
                        .and_then(Value::as_str)
                        .is_some_and(|d| !d.trim().is_empty());
                    assert!(described, "{here} has no description");
                    walk(v, &here);
                }
            }
            if let Some(items) = schema.get("items") {
                walk(items, &format!("{path}[]"));
            }
            if let Some(alts) = schema.get("anyOf").and_then(Value::as_array) {
                for (i, alt) in alts.iter().enumerate() {
                    walk(alt, &format!("{path}<{i}>"));
                }
            }
        }
        let caps = Capabilities::basic();
        let mut defs = definitions(&caps);
        defs.push(interact_def(&["click".into()]));
        defs.push(interact_stop_def());
        defs.push(agent_def());
        defs.push(agent_status_def());
        defs.push(graph_query_def());
        for d in &defs {
            walk(&d.input_schema, &d.name);
        }
    }

    #[test]
    fn firecrawl_style_arguments_validate() {
        let caps = Capabilities::basic();
        let (scrape_def, crawl_def, batch_scrape_def) = (
            || scrape_def(&caps),
            || crawl_def(&caps),
            || batch_scrape_def(&caps),
        );
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
        assert!(validate(&poll_job_def().input_schema, &json!({})).is_err());
        assert!(validate(&cancel_job_def().input_schema, &json!({})).is_err());
    }

    /// crawl、map 的每个属性都是请求类型的真实字段。
    #[test]
    fn agent_properties_are_real_fields() {
        use pagesift_api::fields::names;
        use pagesift_api::types::AgentRequest;
        let def = agent_def();
        let props = def.input_schema["properties"].as_object().unwrap();
        for k in props.keys() {
            assert!(names::<AgentRequest>().contains(&k.as_str()), "{k}");
        }
    }

    #[test]
    fn crawl_and_map_properties_are_real_fields() {
        use pagesift_api::fields::names;
        use pagesift_api::types::{CrawlRequest, MapRequest};
        let caps = Capabilities::basic();
        for (schema, fields) in [
            (crawl_def(&caps).input_schema, names::<CrawlRequest>()),
            (map_def().input_schema, names::<MapRequest>()),
        ] {
            for k in schema["properties"].as_object().unwrap().keys() {
                assert!(fields.contains(&k.as_str()), "{k}");
            }
        }
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

    #[tokio::test]
    async fn placeholder_outputs_match_schemas() {
        let tools = placeholder_tools();
        for (name, args) in [
            (SCRAPE, json!({"url":"https://example.com"})),
            (MAP, json!({"url":"https://example.com"})),
            (CRAWL, json!({"url":"https://example.com"})),
            (BATCH_SCRAPE, json!({"urls":["https://example.com"]})),
            (POLL_JOB, json!({"id":"placeholder-crawl"})),
            (CANCEL_JOB, json!({"id":"placeholder-crawl"})),
        ] {
            let tool = tools.iter().find(|t| t.definition().name == name).unwrap();
            let output = tool
                .call(args.as_object().unwrap().clone(), ToolContext::default())
                .await
                .unwrap();
            validate(
                tool.definition().output_schema.as_ref().unwrap(),
                output.structured_content.as_ref().unwrap(),
            )
            .unwrap();
        }
    }
}
