//! MCP 工具：把 pagesift-mcp 定义的工具接到真正的抓取引擎上。
//! crawl 和 batch scrape 复用 API 层的任务管理，`pagesift_poll_job` 两种任务都能查，
//! `pagesift_cancel_job` 两种任务都能停。
//! 配置了审计日志时多一个 `pagesift_report_issue`，让调用方把看到的错误结果记下来。

use std::sync::Arc;

use pagesift_api::fields::names;
use pagesift_api::types::{
    BatchScrapeRequest, CrawlRequest, InteractRequest, MapRequest, ScrapeOptions, ScrapeRequest,
};
use pagesift_api::{ApiError, Engine, Jobs, check_scrape_options, check_url, rfc3339};
use pagesift_mcp::{
    BoxFuture, Capabilities, ToolAnnotations, ToolContext, ToolDef, ToolError, ToolHandler,
    ToolOutput, ToolResult, tools,
};
use serde_json::{Map, Value, json};

use crate::Scraper;

pub fn tools(scraper: Arc<Scraper>) -> Vec<Arc<dyn ToolHandler>> {
    let audit = scraper.audit().cloned();
    let store = scraper.store_ref().cloned();
    engine_tools_with(scraper, audit, store)
}

/// 同 [`tools`]，页面交给任意引擎执行：多服务器网关传 `ClusterEngine`，MCP 的抓取和任务走网关的任务系统。
pub fn engine_tools(
    engine: Arc<dyn Engine>,
    audit: Option<Arc<crate::audit::Audit>>,
) -> Vec<Arc<dyn ToolHandler>> {
    engine_tools_with(engine, audit, None)
}

fn engine_tools_with(
    engine: Arc<dyn Engine>,
    audit: Option<Arc<crate::audit::Audit>>,
    store: Option<pagesift_store::Store>,
) -> Vec<Arc<dyn ToolHandler>> {
    let jobs = Jobs::with_store(store);
    let caps = capabilities(engine.as_ref());
    let mut tools: Vec<Arc<dyn ToolHandler>> = vec![
        Arc::new(Scrape(engine.clone(), caps.clone())),
        Arc::new(MapTool(engine.clone())),
        Arc::new(Crawl(engine.clone(), jobs.clone(), caps.clone())),
        Arc::new(PollJob(jobs.clone())),
        Arc::new(CancelJob(jobs.clone())),
    ];
    if let Some(audit) = audit {
        tools.push(Arc::new(Report(audit)));
    }
    if caps.interact {
        tools.push(Arc::new(Interact(engine.clone())));
        tools.push(Arc::new(InteractStop(engine.clone())));
    }
    // agent 要模型：没配 LLM 时不列出来（`json` 格式只有配了 LLM 才有）
    if engine.supported_formats().contains(&"json") {
        let agents = pagesift_api::Agents::default();
        tools.push(Arc::new(AgentTool(engine.clone(), agents.clone())));
        tools.push(Arc::new(AgentStatus(agents)));
    }
    tools.push(Arc::new(Batch(engine, jobs.clone(), caps)));
    tools.push(Arc::new(crate::mcp_graph::GraphQueryTool(jobs)));
    tools
}

/// 参数表按引擎实际能力生成：格式取 `supported_formats`，动作只在有浏览器时出现。
pub fn capabilities(engine: &dyn Engine) -> Capabilities {
    let actions: &[&str] = if engine.supports_actions() {
        pagesift_cdp::SUPPORTED_ACTIONS
    } else {
        &[]
    };
    Capabilities::new(engine.supported_formats(), actions).with_interact(engine.supports_interact())
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
    serve_tools(tools(scraper), transport, port, shutdown).await
}

/// 用给定的工具集起 MCP 服务（stdio 或 http，http 只监听本机）。
pub async fn serve_tools(
    tools: Vec<Arc<dyn ToolHandler>>,
    transport: &str,
    port: u16,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let server = pagesift_mcp::McpServer::builder("pagesift", env!("CARGO_PKG_VERSION"))
        .tools(tools)
        .resources(Arc::new(Attachments))
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

/// 导出图片的资源 URI 前缀：`pagesift://attachments/<id>`。
const ATTACHMENT_URI: &str = "pagesift://attachments/";

/// `resources/read`：从附件存储里取（保留期与结果相同）。
struct Attachments;

impl pagesift_mcp::ResourceReader for Attachments {
    fn read(&self, uri: &str) -> Option<(String, Vec<u8>)> {
        let id = uri.strip_prefix(ATTACHMENT_URI)?;
        pagesift_api::attachments::get(id).map(|(data, mime)| (mime.to_string(), data.to_vec()))
    }
}

/// pagesift 自己的入口：参数表外的字段直接拒收（MCP 服务器按 inputSchema 先校验一遍，这里再按
/// 类型的真实字段核一遍，`known` 为空时只解析）。
fn parse<T: serde::de::DeserializeOwned>(args: Value, known: &[&[&str]]) -> Result<T, ToolError> {
    if let (Some(obj), false) = (args.as_object(), known.is_empty()) {
        pagesift_api::fields::reject_unknown(obj, known).map_err(tool_error)?;
    }
    serde_json::from_value(args).map_err(|e| ToolError::new(format!("invalid arguments: {e}")))
}

fn tool_error(e: ApiError) -> ToolError {
    let mut details = json!({ "code": e.code.as_str() });
    if let Some(d) = e.details {
        details["details"] = d;
    }
    ToolError::new(format!("{}: {}", e.code.as_str(), e.message)).with_details(details)
}

/// MCP 暴露给调用方的抓取选项字段（`ScrapeOptions` 的字段减去不暴露的那几个）。
fn scrape_fields() -> &'static [&'static str] {
    static FIELDS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    FIELDS.get_or_init(|| {
        names::<ScrapeOptions>()
            .iter()
            .copied()
            .filter(|f| {
                !pagesift_mcp::options::NOT_EXPOSED
                    .iter()
                    .any(|(n, _)| n == f)
            })
            .collect()
    })
}

struct Scrape(Arc<dyn Engine>, Capabilities);

impl ToolHandler for Scrape {
    fn definition(&self) -> ToolDef {
        tools::scrape_def(&self.1)
    }

    fn call(&self, args: Map<String, Value>, ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            let mut req: ScrapeRequest = parse(Value::Object(args), &[&["url"], scrape_fields()])?;
            check_url(&req.url).map_err(tool_error)?;
            let keep = std::mem::take(&mut req.options.keep_session);
            check_scrape_options(&req.options, self.0.as_ref()).map_err(tool_error)?;
            let scraping = async {
                if keep {
                    self.0
                        .scrape_session(&req.url, &req.options, true, None)
                        .await
                } else {
                    self.0.scrape(&req.url, &req.options).await
                }
            };
            let doc = crate::audit::CLIENT
                .scope(client_name(&ctx), scraping)
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
                None if req.options.image_assets.is_some() => {
                    let mut metadata = doc.clone();
                    for attachment in &mut metadata.attachments {
                        attachment.inline_data = false;
                    }
                    serde_json::to_string_pretty(&metadata).unwrap_or_default()
                }
                None => serde_json::to_string_pretty(&doc).unwrap_or_default(),
            };
            // 保留了页面：调用方要靠这个 id 继续，放在最前面
            let text = match &doc.metadata.scrape_id {
                Some(id) => {
                    format!("scrapeId: {id} (pagesift_interact, pagesift_interact_stop)\n{text}")
                }
                None => text,
            };
            let mut out = ToolOutput::text(text);
            let native_images = req.options.image_assets.is_some();
            if native_images {
                // The image content blocks below carry the bytes; avoid also
                // embedding the same bytes in structuredContent.
                let mut structured = doc.clone();
                for a in &mut structured.attachments {
                    a.inline_data = false;
                }
                if req.options.image_assets == Some(pagesift_api::types::ImageAssetMode::Embed) {
                    // The text block already carries self-contained Markdown.
                    // Avoid a second copy of its data URLs in structuredContent.
                    if structured
                        .markdown
                        .as_deref()
                        .is_some_and(|m| m.contains("](data:image/"))
                    {
                        structured.markdown = None;
                    }
                    if structured.pages.as_ref().is_some_and(|pages| {
                        pages.iter().any(|p| p.markdown.contains("](data:image/"))
                    }) {
                        structured.pages = None;
                    }
                }
                out.structured_content = serde_json::to_value(&structured).ok();
            } else {
                out.structured_content = serde_json::to_value(&doc).ok();
            }
            // 导出的图片：内容存起来，结果里给资源链接（`resources/read` 取）
            pagesift_api::attachments::keep(&doc, None);
            for a in &doc.attachments {
                if native_images
                    && let Some(bytes) = a.data.as_deref()
                    && let Ok(data) = pagesift_base64::STANDARD.encode_to_string(bytes)
                {
                    out.content.push(pagesift_mcp::Content::Image {
                        data,
                        mime_type: a.mime().into(),
                    });
                } else {
                    out.content.push(pagesift_mcp::Content::ResourceLink {
                        uri: format!("{ATTACHMENT_URI}{}", a.id),
                        name: a.id.clone(),
                        mime_type: a.mime().into(),
                        size: Some(a.bytes),
                    });
                }
            }
            if native_images
                && req.options.image_assets == Some(pagesift_api::types::ImageAssetMode::Embed)
                && let Some(md) = &doc.markdown
            {
                let mut emitted = std::collections::HashSet::new();
                for part in md.split("](data:image/").skip(1) {
                    let Some(end) = part.find(')') else { continue };
                    let encoded = part[..end].split_ascii_whitespace().next().unwrap_or("");
                    let Some((mime, data)) = encoded.split_once(";base64,") else {
                        continue;
                    };
                    if mime == "svg+xml"
                        || !mime
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
                    {
                        continue;
                    }
                    let Ok(bytes) = pagesift_base64::STANDARD.decode_to_vec(data.as_bytes()) else {
                        continue;
                    };
                    let Some(format) = pagesift_api::types::attachment::sniff_image_format(&bytes)
                    else {
                        continue;
                    };
                    let a = pagesift_api::types::Attachment::new(bytes, format, None);
                    if a.mime() == format!("image/{mime}")
                        && doc
                            .image_assets
                            .iter()
                            .any(|asset| asset.id.as_deref() == Some(&a.id))
                        && emitted.insert(a.hash)
                    {
                        out.content.push(pagesift_mcp::Content::Image {
                            data: data.into(),
                            mime_type: format!("image/{mime}"),
                        });
                    }
                }
            }
            Ok(out)
        })
    }
}

/// `pagesift_interact`：在 `keepSession` 保留的页面上继续操作。
struct Interact(Arc<dyn Engine>);

impl ToolHandler for Interact {
    fn definition(&self) -> ToolDef {
        let actions: Vec<String> = pagesift_cdp::SUPPORTED_ACTIONS
            .iter()
            .map(|s| s.to_string())
            .collect();
        tools::interact_def(&actions)
    }

    fn call(&self, mut args: Map<String, Value>, _ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            let id = args
                .remove("scrapeId")
                .and_then(|v| v.as_str().map(str::to_string))
                .ok_or_else(|| ToolError::new("missing scrapeId"))?;
            let known: &[&str] = &["prompt", "actions", "timeout"];
            let req: InteractRequest = parse(Value::Object(args), &[known])?;
            req.check().map_err(tool_error)?;
            let resp = self.0.interact(&id, &req, None).await.map_err(tool_error)?;
            // 智能体最需要的是回答和动作后的正文：放在文本内容里，完整结果放在结构化结果里
            let mut parts = Vec::new();
            if let Some(e) = &resp.error {
                parts.push(format!("Failed: {e}"));
            }
            if let Some(o) = &resp.output {
                parts.push(o.clone());
            }
            if let Some(md) = resp.data.as_ref().and_then(|d| d.markdown.as_ref()) {
                parts.push(md.clone());
            }
            let mut out = ToolOutput::text(parts.join("\n\n"));
            out.structured_content = serde_json::to_value(&resp).ok();
            Ok(out)
        })
    }
}

/// `pagesift_interact_stop`：关掉保留的页面。
struct InteractStop(Arc<dyn Engine>);

impl ToolHandler for InteractStop {
    fn definition(&self) -> ToolDef {
        tools::interact_stop_def()
    }

    fn call(&self, args: Map<String, Value>, _ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            let id = args
                .get("scrapeId")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::new("missing scrapeId"))?;
            let stopped = self.0.stop_interact(id, None).await.map_err(tool_error)?;
            Ok(ToolOutput::json(json!({
                "success": stopped.success,
                "sessionDurationMs": stopped.session_duration_ms,
            })))
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
            .output_schema(tools::report_issue_schema())
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
            Ok(ToolOutput::json(json!({
                "recorded": true,
                "message": "Recorded. Thank you.",
            })))
        })
    }
}

struct MapTool(Arc<dyn Engine>);

impl ToolHandler for MapTool {
    fn definition(&self) -> ToolDef {
        tools::map_def()
    }

    fn call(&self, args: Map<String, Value>, _ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            let req: MapRequest = parse(Value::Object(args), &[names::<MapRequest>()])?;
            check_url(&req.url).map_err(tool_error)?;
            let links = self.0.map(&req).await.map_err(tool_error)?;
            Ok(ToolOutput::json(json!({ "links": links })))
        })
    }
}

struct Crawl(Arc<dyn Engine>, Jobs, Capabilities);

impl ToolHandler for Crawl {
    fn definition(&self) -> ToolDef {
        tools::crawl_def(&self.2)
    }

    fn call(&self, args: Map<String, Value>, _ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            if let Some(Value::Object(o)) = args.get("scrapeOptions") {
                pagesift_api::fields::reject_unknown(o, &[scrape_fields()]).map_err(tool_error)?;
            }
            let req: CrawlRequest = parse(Value::Object(args), &[names::<CrawlRequest>()])?;
            check_url(&req.url).map_err(tool_error)?;
            check_scrape_options(&req.scrape_options, self.0.as_ref()).map_err(tool_error)?;
            if let Some(g) = &req.graph {
                pagesift_api::types::graph::check_graph_options(g).map_err(tool_error)?;
            }
            if req.webhook.is_some() {
                return Err(tool_error(ApiError::unsupported(
                    "webhooks are not available in this build",
                )));
            }
            let id = self.1.start_crawl(self.0.clone(), req).await;
            Ok(ToolOutput::json(json!({
                "id": id,
                "status": "scraping",
                "next": "call pagesift_poll_job with this id to get progress and pages",
            })))
        })
    }
}

struct Batch(Arc<dyn Engine>, Jobs, Capabilities);

impl ToolHandler for Batch {
    fn definition(&self) -> ToolDef {
        tools::batch_scrape_def(&self.2)
    }

    fn call(&self, mut args: Map<String, Value>, _ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            // firecrawl-mcp 把抓取选项放在 options 里；展开成 batch 请求的顶层字段
            if let Some(Value::Object(opts)) = args.remove("options") {
                pagesift_api::fields::reject_unknown(&opts, &[scrape_fields()])
                    .map_err(tool_error)?;
                for (k, v) in opts {
                    args.entry(k).or_insert(v);
                }
            }
            let req: BatchScrapeRequest = parse(
                Value::Object(args),
                &[BatchScrapeRequest::OWN_FIELDS, scrape_fields()],
            )?;
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
            // 统一准入：批量里的抓取都要过限速器的全局名额和每站点名额，
            // 批量自己的并发取当前全局在途数目标（USL 控制器），调用方给的值只能更小。
            let admitted = self.0.admitted_concurrency().unwrap_or(1).max(1);
            let concurrency = req.max_concurrency.unwrap_or(admitted).clamp(1, admitted);
            let options: ScrapeOptions = req.options;
            let id = self
                .1
                .start_batch(self.0.clone(), urls, options, concurrency)
                .await;
            Ok(ToolOutput::json(json!({
                "id": id,
                "status": "scraping",
                "invalidURLs": invalid,
                "next": "call pagesift_poll_job with this id to get progress and pages",
            })))
        })
    }
}

struct AgentTool(Arc<dyn Engine>, pagesift_api::Agents);

impl ToolHandler for AgentTool {
    fn definition(&self) -> ToolDef {
        tools::agent_def()
    }

    fn call(&self, args: Map<String, Value>, _ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            let req: pagesift_api::types::AgentRequest = parse(
                Value::Object(args),
                &[names::<pagesift_api::types::AgentRequest>()],
            )?;
            if req.webhook.is_some() {
                return Err(tool_error(ApiError::unsupported(
                    "webhooks are not available in this build",
                )));
            }
            let id = self
                .1
                .start_local(self.0.clone(), req)
                .await
                .map_err(tool_error)?;
            Ok(ToolOutput::json(json!({
                "id": id,
                "status": "processing",
                "next": "call pagesift_agent_status with this id to get the result",
            })))
        })
    }
}

struct AgentStatus(pagesift_api::Agents);

impl ToolHandler for AgentStatus {
    fn definition(&self) -> ToolDef {
        tools::agent_status_def()
    }

    fn call(&self, args: Map<String, Value>, _ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            pagesift_api::fields::reject_unknown(&args, &[&["id", "includeEvidence"]])
                .map_err(tool_error)?;
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::new("missing id"))?;
            let evidence = args
                .get("includeEvidence")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let body = self
                .0
                .status(id, "anonymous", evidence)
                .await
                .map_err(tool_error)?;
            Ok(ToolOutput::json(body))
        })
    }
}

struct PollJob(Jobs);

impl ToolHandler for PollJob {
    fn definition(&self) -> ToolDef {
        tools::poll_job_def()
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

struct CancelJob(Jobs);

impl ToolHandler for CancelJob {
    fn definition(&self) -> ToolDef {
        tools::cancel_job_def()
    }

    fn call(&self, args: Map<String, Value>, _ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::new("missing id"))?
                .to_string();
            if !self.0.cancel(&id).await {
                return Err(ToolError::new(format!("job {id} not found or expired")));
            }
            let body = self
                .0
                .get(&id, |job| {
                    json!({
                        "success": true,
                        "status": job.state.as_str(),
                        "completed": job.completed,
                        "total": job.total,
                    })
                })
                .await
                .ok_or_else(|| ToolError::new(format!("job {id} not found or expired")))?;
            Ok(ToolOutput::json(body))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ImageEngine;

    impl pagesift_api::Engine for ImageEngine {
        fn scrape<'a>(
            &'a self,
            _url: &'a str,
            opts: &'a ScrapeOptions,
        ) -> pagesift_api::BoxFuture<'a, Result<pagesift_api::types::Document, ApiError>> {
            Box::pin(async {
                let mut a = pagesift_api::types::Attachment::new(
                    b"\x89PNG\r\n\x1a\nimage".to_vec(),
                    "png",
                    Some("https://example.com/a.png".into()),
                );
                if opts.image_assets == Some(pagesift_api::types::ImageAssetMode::Embed) {
                    let data =
                        pagesift_base64::encode_data_url(a.mime(), a.data.as_deref().unwrap())
                            .unwrap();
                    return Ok(pagesift_api::types::Document {
                        markdown: Some(format!("![a]({data}) ![duplicate]({data})")),
                        image_assets: vec![pagesift_api::types::ImageAssetResult {
                            source: "https://example.com/a.png".into(),
                            status: pagesift_api::types::ImageAssetStatus::Ok,
                            id: Some(a.id.clone()),
                            mime: Some(a.mime().into()),
                            bytes: Some(a.bytes),
                            reason: None,
                        }],
                        ..Default::default()
                    });
                }
                a.inline_data = true;
                Ok(pagesift_api::types::Document {
                    markdown: Some("![a](https://example.com/a.png)".into()),
                    attachments: vec![a],
                    ..Default::default()
                })
            })
        }
    }

    #[tokio::test]
    async fn image_assets_use_native_mcp_content_without_structured_base64() {
        let handlers = engine_tools(Arc::new(ImageEngine), None);
        let scrape = handlers
            .iter()
            .find(|h| h.definition().name == "pagesift_scrape")
            .unwrap();
        let args = json!({"url":"https://example.com/", "imageAssets":"attach"})
            .as_object()
            .unwrap()
            .clone();
        let output = scrape.call(args, ToolContext::default()).await.unwrap();
        let image = output
            .content
            .iter()
            .find_map(|c| match c {
                pagesift_mcp::Content::Image { data, mime_type } => Some((data, mime_type)),
                _ => None,
            })
            .unwrap();
        assert_eq!(image.1, "image/png");
        assert_eq!(
            pagesift_base64::STANDARD
                .decode_to_vec(image.0.as_bytes())
                .unwrap(),
            b"\x89PNG\r\n\x1a\nimage"
        );
        let structured = output.structured_content.unwrap();
        assert!(structured["attachments"][0].get("data").is_none());
    }

    #[tokio::test]
    async fn embedded_mcp_images_are_native_without_extra_structured_copy() {
        let handlers = engine_tools(Arc::new(ImageEngine), None);
        let scrape = handlers
            .iter()
            .find(|h| h.definition().name == "pagesift_scrape")
            .unwrap();
        let args = json!({"url":"https://example.com/", "imageAssets":"embed"})
            .as_object()
            .unwrap()
            .clone();
        let output = scrape.call(args, ToolContext::default()).await.unwrap();
        assert_eq!(
            output
                .content
                .iter()
                .filter(|c| matches!(c, pagesift_mcp::Content::Image { .. }))
                .count(),
            1
        );
        assert!(output.content.iter().any(
            |c| matches!(c, pagesift_mcp::Content::Text(t) if t.contains("data:image/png;base64,"))
        ));
        assert!(output.structured_content.unwrap().get("markdown").is_none());
    }

    #[test]
    fn base_tools_include_poll_and_cancel() {
        let scraper = Arc::new(Scraper::new(crate::ScraperConfig::default()).unwrap());
        let names: Vec<String> = tools(scraper).iter().map(|t| t.definition().name).collect();
        assert!(
            names.contains(&"pagesift_poll_job".to_string()),
            "{names:?}"
        );
        assert!(
            names.contains(&"pagesift_cancel_job".to_string()),
            "{names:?}"
        );
        assert!(
            !names.contains(&"pagesift_crawl_status".to_string()),
            "{names:?}"
        );
    }

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
        let audit =
            Arc::new(crate::audit::Audit::open(crate::audit::AuditConfig::new(&path)).unwrap());
        let config = crate::ScraperConfig {
            audit: Some(audit.clone()),
            ..Default::default()
        };
        let all = tools(Arc::new(Scraper::new(config).unwrap()));
        let report = all
            .iter()
            .find(|h| h.definition().name == "pagesift_report_issue")
            .expect("report tool");
        assert!(report.definition().output_schema.is_some());
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
        audit.flush();
        let log = std::fs::read_to_string(&path).unwrap();
        let rec: Value = serde_json::from_str(log.trim()).unwrap();
        assert_eq!(rec["kind"], "report");
        assert_eq!(rec["actual"], "nav only");
    }

    /// 共享 MCP 实测里 client 恒为 unknown：旧版客户端只在 initialize 里报名字。
    #[tokio::test]
    async fn audit_client_comes_from_legacy_initialize() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let audit =
            Arc::new(crate::audit::Audit::open(crate::audit::AuditConfig::new(&path)).unwrap());
        let config = crate::ScraperConfig {
            audit: Some(audit.clone()),
            ..Default::default()
        };
        let server = pagesift_mcp::McpServer::builder("pagesift", "test")
            .tools(tools(Arc::new(Scraper::new(config).unwrap())))
            .build()
            .unwrap();
        let (mut w, sr) = tokio::io::duplex(1 << 16);
        let (sw, r) = tokio::io::duplex(1 << 16);
        tokio::spawn(pagesift_mcp::serve_stream(server, sr, sw));
        let mut lines = tokio::io::BufReader::new(r).lines();
        for msg in [
            json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": { "name": "aiseek-test", "version": "1" } } }),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "pagesift_report_issue",
                "arguments": { "url": "https://example.com/a", "expected": "body", "actual": "nav only" } } }),
        ] {
            w.write_all(format!("{msg}\n").as_bytes()).await.unwrap();
            lines.next_line().await.unwrap().unwrap();
        }
        audit.flush();
        let log = std::fs::read_to_string(&path).unwrap();
        let rec: Value = serde_json::from_str(log.trim()).unwrap();
        assert_eq!(rec["client"], "aiseek-test");
    }
}
