//! MCP 工具：把 pagesift-mcp 定义的 5 个工具接到真正的抓取引擎上。
//! crawl 和 batch scrape 复用 API 层的任务管理，`pagesift_crawl_status` 两种任务都能查。

use std::sync::Arc;

use pagesift_api::types::{
    BatchScrapeRequest, CrawlRequest, MapRequest, ScrapeOptions, ScrapeRequest,
};
use pagesift_api::{ApiError, Engine, Jobs, check_scrape_options, check_url, rfc3339};
use pagesift_mcp::{
    BoxFuture, ToolContext, ToolDef, ToolError, ToolHandler, ToolOutput, ToolResult, tools,
};
use serde_json::{Map, Value, json};

use crate::Scraper;

/// batch scrape 的默认并发上限；同一主机的请求仍然受礼貌限速约束。
const BATCH_CONCURRENCY: usize = 8;

pub fn tools(scraper: Arc<Scraper>) -> Vec<Arc<dyn ToolHandler>> {
    let jobs = Jobs::default();
    vec![
        Arc::new(Scrape(scraper.clone())),
        Arc::new(MapTool(scraper.clone())),
        Arc::new(Crawl(scraper.clone(), jobs.clone())),
        Arc::new(Status(jobs.clone())),
        Arc::new(Batch(scraper, jobs)),
    ]
}

pub async fn run_server(scraper: Arc<Scraper>, transport: &str, port: u16) -> anyhow::Result<()> {
    let server = pagesift_mcp::McpServer::builder("pagesift", env!("CARGO_PKG_VERSION"))
        .tools(tools(scraper))
        .build()
        .map_err(|e| anyhow::anyhow!("building the MCP server: {e:?}"))?;
    match transport {
        "stdio" => pagesift_mcp::serve_stdio(server).await.map_err(|e| anyhow::anyhow!("{e}"))?,
        "http" => {
            let config = pagesift_mcp::HttpConfig {
                bind: std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                ..Default::default()
            };
            eprintln!(
                "pagesift MCP listening on http://127.0.0.1:{port}{}",
                config.path
            );
            pagesift_mcp::serve_http(server, config).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        other => anyhow::bail!("unknown transport `{other}`; use stdio or http"),
    }
    Ok(())
}

fn parse<T: serde::de::DeserializeOwned>(args: Value) -> Result<T, ToolError> {
    serde_json::from_value(args).map_err(|e| ToolError::new(format!("invalid arguments: {e}")))
}

fn tool_error(e: ApiError) -> ToolError {
    let mut details = json!({ "code": e.code.as_str() });
    if let Some(d) = e.details {
        details["details"] = d;
    }
    ToolError::new(format!("{}: {}", e.code.as_str(), e.message)).with_details(details)
}

struct Scrape(Arc<Scraper>);

impl ToolHandler for Scrape {
    fn definition(&self) -> ToolDef {
        tools::scrape_def()
    }

    fn call(&self, args: Map<String, Value>, _ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            let req: ScrapeRequest = parse(Value::Object(args))?;
            check_url(&req.url).map_err(tool_error)?;
            check_scrape_options(&req.options, self.0.as_ref()).map_err(tool_error)?;
            let doc = self
                .0
                .scrape(&req.url, &req.options)
                .await
                .map_err(tool_error)?;
            // 智能体最需要的是正文：Markdown 放在文本内容里，完整文档放在结构化结果里
            let text = match &doc.markdown {
                Some(md) => {
                    let title = doc.metadata.title.as_deref().unwrap_or_default();
                    let url = doc.metadata.url.as_deref().unwrap_or(&req.url);
                    format!("# {title}\nURL: {url}\n\n{md}")
                }
                None => serde_json::to_string_pretty(&doc).unwrap_or_default(),
            };
            let mut out = ToolOutput::text(text);
            out.structured_content = serde_json::to_value(&doc).ok();
            Ok(out)
        })
    }
}

struct MapTool(Arc<Scraper>);

impl ToolHandler for MapTool {
    fn definition(&self) -> ToolDef {
        tools::map_def()
    }

    fn call(&self, args: Map<String, Value>, _ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            let req: MapRequest = parse(Value::Object(args))?;
            check_url(&req.url).map_err(tool_error)?;
            let links = self.0.map(&req).await.map_err(tool_error)?;
            Ok(ToolOutput::json(json!({ "links": links })))
        })
    }
}

struct Crawl(Arc<Scraper>, Jobs);

impl ToolHandler for Crawl {
    fn definition(&self) -> ToolDef {
        tools::crawl_def()
    }

    fn call(&self, args: Map<String, Value>, _ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            let req: CrawlRequest = parse(Value::Object(args))?;
            check_url(&req.url).map_err(tool_error)?;
            check_scrape_options(&req.scrape_options, self.0.as_ref()).map_err(tool_error)?;
            if req.webhook.is_some() {
                return Err(tool_error(ApiError::unsupported(
                    "webhooks are not available in this build",
                )));
            }
            let id = self
                .1
                .start_crawl(self.0.clone() as Arc<dyn Engine>, req)
                .await;
            Ok(ToolOutput::json(json!({
                "id": id,
                "status": "scraping",
                "next": "call pagesift_crawl_status with this id to get progress and pages",
            })))
        })
    }
}

struct Batch(Arc<Scraper>, Jobs);

impl ToolHandler for Batch {
    fn definition(&self) -> ToolDef {
        tools::batch_scrape_def()
    }

    fn call(&self, mut args: Map<String, Value>, _ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            // firecrawl-mcp 把抓取选项放在 options 里；展开成 batch 请求的顶层字段
            if let Some(Value::Object(opts)) = args.remove("options") {
                for (k, v) in opts {
                    args.entry(k).or_insert(v);
                }
            }
            let req: BatchScrapeRequest = parse(Value::Object(args))?;
            check_scrape_options(&req.options, self.0.as_ref()).map_err(tool_error)?;
            let mut urls = Vec::new();
            let mut invalid = Vec::new();
            for u in req.urls {
                match check_url(&u) {
                    Ok(()) => urls.push(u),
                    Err(_) => invalid.push(u),
                }
            }
            if urls.is_empty() {
                return Err(ToolError::new("no valid URLs"));
            }
            let concurrency = req
                .max_concurrency
                .unwrap_or(BATCH_CONCURRENCY)
                .min(BATCH_CONCURRENCY);
            let options: ScrapeOptions = req.options;
            let id = self
                .1
                .start_batch(
                    self.0.clone() as Arc<dyn Engine>,
                    urls,
                    options,
                    concurrency,
                )
                .await;
            Ok(ToolOutput::json(
                json!({ "id": id, "status": "scraping", "invalidURLs": invalid }),
            ))
        })
    }
}

struct Status(Jobs);

impl ToolHandler for Status {
    fn definition(&self) -> ToolDef {
        tools::crawl_status_def()
    }

    fn call(&self, args: Map<String, Value>, _ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::new("missing id"))?
                .to_string();
            self.0
                .get(&id, |job| {
                    ToolOutput::json(json!({
                        "status": job.state.as_str(),
                        "completed": job.completed,
                        "total": job.total,
                        "expiresAt": rfc3339(job.expires_at()),
                        "errors": job.errors.len(),
                        "robotsBlocked": job.robots_blocked,
                        "data": job.data,
                    }))
                })
                .await
                .ok_or_else(|| ToolError::new(format!("job {id} not found or expired")))
        })
    }
}
