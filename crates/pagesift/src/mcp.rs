//! MCP 工具：把 pagesift-mcp 定义的 5 个工具接到真正的抓取引擎上。
//! crawl 和 batch scrape 复用 API 层的任务管理，`pagesift_crawl_status` 两种任务都能查。
//! 配置了审计日志时多一个 `pagesift_report_issue`，让调用方把看到的错误结果记下来。

use std::sync::Arc;

use pagesift_api::types::{
    BatchScrapeRequest, CrawlRequest, MapRequest, ScrapeOptions, ScrapeRequest,
};
use pagesift_api::{ApiError, Engine, Jobs, check_scrape_options, check_url, rfc3339};
use pagesift_mcp::{
    BoxFuture, ToolAnnotations, ToolContext, ToolDef, ToolError, ToolHandler, ToolOutput,
    ToolResult, tools,
};
use serde_json::{Map, Value, json};

use crate::Scraper;

/// batch scrape 的默认并发上限；同一主机的请求仍然受礼貌限速约束。
const BATCH_CONCURRENCY: usize = 8;

pub fn tools(scraper: Arc<Scraper>) -> Vec<Arc<dyn ToolHandler>> {
    let jobs = Jobs::default();
    let mut tools: Vec<Arc<dyn ToolHandler>> = vec![
        Arc::new(Scrape(scraper.clone())),
        Arc::new(MapTool(scraper.clone())),
        Arc::new(Crawl(scraper.clone(), jobs.clone())),
        Arc::new(Status(jobs.clone())),
    ];
    if let Some(audit) = scraper.audit() {
        tools.push(Arc::new(Report(audit.clone())));
    }
    tools.push(Arc::new(Batch(scraper, jobs)));
    tools
}

/// 客户端自报的名字，写进审计日志区分是哪个会话发起的抓取。
fn client_name(ctx: &ToolContext) -> String {
    ctx.client_info()
        .and_then(|c| c.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .chars()
        .take(64)
        .collect()
}

/// 文本内容里提醒调用方的警告：正文可能不完整的那几类。完整列表在结构化结果里。
fn warning_line(doc: &pagesift_api::types::Document) -> Option<String> {
    let warnings = &doc.metadata.pagesift.as_ref()?.warnings;
    let shown: Vec<&str> = warnings
        .iter()
        .map(|w| w.split(':').next().unwrap_or(w))
        .collect();
    (!shown.is_empty()).then(|| format!("Warnings: {}", shown.join(", ")))
}

pub async fn run_server(scraper: Arc<Scraper>, transport: &str, port: u16) -> anyhow::Result<()> {
    run_server_until(scraper, transport, port, std::future::pending()).await
}

/// 同 [`run_server`]，`shutdown` 完成时 http 传输停止接收新请求并退出（调用方随后关掉浏览器池）。
pub async fn run_server_until(
    scraper: Arc<Scraper>,
    transport: &str,
    port: u16,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let server = pagesift_mcp::McpServer::builder("pagesift", env!("CARGO_PKG_VERSION"))
        .tools(tools(scraper))
        .build()
        .map_err(|e| anyhow::anyhow!("building the MCP server: {e:?}"))?;
    match transport {
        "stdio" => pagesift_mcp::serve_stdio(server)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?,
        "http" => {
            let config = pagesift_mcp::HttpConfig {
                bind: std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                ..Default::default()
            };
            eprintln!(
                "pagesift MCP listening on http://127.0.0.1:{port}{}",
                config.path
            );
            let listener = tokio::net::TcpListener::bind(config.bind).await?;
            // 收到 SIGTERM（systemd 停止服务）先停止接新请求，调用方再关掉浏览器池
            pagesift_mcp::serve_listener(server, &config, listener, shutdown)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
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

    fn call(&self, args: Map<String, Value>, ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            let req: ScrapeRequest = parse(Value::Object(args))?;
            check_url(&req.url).map_err(tool_error)?;
            check_scrape_options(&req.options, self.0.as_ref()).map_err(tool_error)?;
            let doc = crate::audit::CLIENT
                .scope(client_name(&ctx), self.0.scrape(&req.url, &req.options))
                .await
                .map_err(tool_error)?;
            // 智能体最需要的是正文：Markdown 放在文本内容里，完整文档放在结构化结果里。
            // 警告也放进文本：调用方多半只读文本，看不到结构化结果里的"正文可能不完整"
            let text = match &doc.markdown {
                Some(md) => {
                    let title = doc.metadata.title.as_deref().unwrap_or_default();
                    let url = doc.metadata.url.as_deref().unwrap_or(&req.url);
                    match warning_line(&doc) {
                        Some(w) => format!("# {title}\nURL: {url}\n{w}\n\n{md}"),
                        None => format!("# {title}\nURL: {url}\n\n{md}"),
                    }
                }
                None => serde_json::to_string_pretty(&doc).unwrap_or_default(),
            };
            let mut out = ToolOutput::text(text);
            out.structured_content = serde_json::to_value(&doc).ok();
            Ok(out)
        })
    }
}

/// `pagesift_report_issue`：调用方发现结果不对时记一条（URL、参数、期望、实际），写进审计日志。
struct Report(Arc<crate::audit::Audit>);

impl ToolHandler for Report {
    fn definition(&self) -> ToolDef {
        let schema = json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "The page whose result looked wrong." },
                "params": { "type": "object", "description": "Arguments used for the scrape, if any." },
                "expected": { "type": "string", "description": "What the page actually contains (e.g. the article body, a table, headings)." },
                "actual": { "type": "string", "description": "What pagesift returned instead (missing body, empty table, wrong text...)." },
                "note": { "type": "string" }
            },
            "required": ["url", "expected", "actual"]
        });
        ToolDef::new("pagesift_report_issue", schema)
            .title("Report a wrong result")
            .description(
                "Record that a pagesift result looked wrong (missing body text, empty table, lost \
                 headings, garbage) so it can be investigated later. Writes one line to the local \
                 audit log; nothing is sent anywhere. Fall back to another fetch tool afterwards.",
            )
            .annotations(ToolAnnotations {
                title: Some("pagesift report issue".into()),
                read_only_hint: Some(false),
                destructive_hint: Some(false),
                idempotent_hint: Some(false),
                open_world_hint: Some(false),
            })
    }

    fn call(&self, args: Map<String, Value>, ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            for key in ["url", "expected", "actual"] {
                if !args.get(key).is_some_and(Value::is_string) {
                    return Err(ToolError::new(format!("missing {key}")));
                }
            }
            let audit = self.0.clone();
            crate::audit::CLIENT
                .scope(client_name(&ctx), async move {
                    audit.record_report(Value::Object(args));
                })
                .await;
            Ok(ToolOutput::text("Recorded. Thank you."))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warnings_reach_the_text_content() {
        let mut doc = pagesift_api::types::Document::default();
        assert_eq!(warning_line(&doc), None);
        doc.metadata.pagesift = Some(pagesift_api::types::PagesiftMeta {
            warnings: vec![
                "thin_content: only 120 characters from 80 KB of HTML".into(),
                "render_unavailable: no browser".into(),
            ],
            ..Default::default()
        });
        assert_eq!(
            warning_line(&doc).as_deref(),
            Some("Warnings: thin_content, render_unavailable")
        );
    }

    #[tokio::test]
    async fn report_issue_is_offered_only_with_an_audit_log_and_writes_to_it() {
        let names = |t: &[Arc<dyn ToolHandler>]| -> Vec<String> {
            t.iter().map(|h| h.definition().name.clone()).collect()
        };
        let plain = Arc::new(Scraper::new(crate::ScraperConfig::default()).unwrap());
        assert!(!names(&tools(plain)).contains(&"pagesift_report_issue".to_string()));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let audit = crate::audit::Audit::open(crate::audit::AuditConfig::new(&path)).unwrap();
        let config = crate::ScraperConfig {
            audit: Some(Arc::new(audit)),
            ..Default::default()
        };
        let all = tools(Arc::new(Scraper::new(config).unwrap()));
        let report = all
            .iter()
            .find(|h| h.definition().name == "pagesift_report_issue")
            .expect("report tool");
        let args =
            json!({ "url": "https://example.com/a", "expected": "body", "actual": "nav only" });
        let Value::Object(args) = args else {
            unreachable!()
        };
        assert!(
            report
                .call(args.clone(), ToolContext::default())
                .await
                .is_ok()
        );
        let mut missing = args;
        missing.remove("actual");
        assert!(report.call(missing, ToolContext::default()).await.is_err());
        let log = std::fs::read_to_string(&path).unwrap();
        let rec: Value = serde_json::from_str(log.trim()).unwrap();
        assert_eq!(rec["kind"], "report");
        assert_eq!(rec["actual"], "nav only");
    }
}
