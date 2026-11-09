//! 工具输出 schema（`outputSchema`）：描述 `structuredContent` 里实际会出现的字段。
//!
//! 只描述字段含义，不复制输入参数；客户端可以据此少猜一次，评分也按字段说明计分。

use serde_json::{Value, json};

fn object(properties: Value, required: &[&str]) -> Value {
    let mut schema = json!({ "type": "object", "properties": properties });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

fn string(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn integer(description: &str) -> Value {
    json!({ "type": "integer", "description": description })
}

fn boolean(description: &str) -> Value {
    json!({ "type": "boolean", "description": description })
}

fn array(items: Value, description: &str) -> Value {
    json!({ "type": "array", "items": items, "description": description })
}

/// 抓取到的页面（`pagesift_scrape` 和 `pagesift_interact` 的 `data`）。
pub fn document() -> Value {
    object(
        json!({
            "markdown": string("Page content as Markdown. Present when the markdown format was requested (the default)."),
            "pages": array(
                object(
                    json!({
                        "pageNumber": integer("1-based PDF page number."),
                        "markdown": string("Markdown for this page."),
                    }),
                    &["pageNumber", "markdown"],
                ),
                "Per-page Markdown, only for PDF page mode.",
            ),
            "html": string("Cleaned HTML when the html format was requested."),
            "rawHtml": string("Unmodified response body when the rawHtml format was requested."),
            "links": array(string("Absolute URL."), "Links found on the page when the links format was requested."),
            "images": array(string("Absolute image URL."), "Image URLs found on the page."),
            "screenshot": string("Screenshot URL or data URL when the screenshot format was requested."),
            "json": json!({ "description": "JSON extracted by the json format; its shape follows the requested schema." }),
            "extract": json!({ "description": "Items extracted by an extract schema." }),
            "summary": string("Summary produced when the summary format was requested."),
            "actions": json!({ "description": "Results of browser actions: screenshots, PDFs, scraped pages and JavaScript return values." }),
            "warning": string("Compatibility or deprecation warning from the scrape."),
            "changeTracking": object(
                json!({
                    "previousScrapeAt": { "type": ["string", "null"], "description": "Time of the previous scrape of this URL, or null on the first scrape." },
                    "changeStatus": { "type": "string", "enum": ["new", "same", "changed", "removed"], "description": "How the page changed since the previous scrape." },
                    "visibility": { "type": "string", "enum": ["visible", "hidden"], "description": "Whether the change is visible on the page." },
                    "diff": json!({ "description": "Structured diff when one was requested." }),
                }),
                &["changeStatus", "visibility"],
            ),
            "chunks": array(
                object(
                    json!({
                        "text": string("Chunk text."),
                        "headingPath": array(string("Heading"), "Headings above the chunk."),
                        "start": integer("Start byte offset in the Markdown."),
                        "end": integer("End byte offset in the Markdown."),
                        "tokens": integer("Estimated token count."),
                    }),
                    &["text", "headingPath", "start", "end", "tokens"],
                ),
                "Markdown chunks when chunking was requested.",
            ),
            "attachments": array(
                object(
                    json!({
                        "id": string("Attachment id; read the file with resources/read."),
                        "format": string("File format (extension)."),
                        "bytes": integer("Size in bytes."),
                        "hash": string("SHA-256 of the content."),
                        "source": { "type": ["string", "null"], "description": "Original URL or document part, when known." },
                    }),
                    &["id", "format", "bytes", "hash"],
                ),
                "Exported files when exportImages was requested.",
            ),
            "imageAssets": array(
                object(json!({
                    "source": string("Original image URL or document part."),
                    "status": { "type": "string", "enum": ["ok", "skipped-limit", "failed"] },
                    "id": string("Content-addressed attachment id when available."),
                    "mime": string("MIME type detected from content."),
                    "bytes": integer("Size in bytes."),
                    "reason": string("Reason for failure or skipping."),
                }), &["source", "status"]),
                "Per-image asset results when imageAssets was requested.",
            ),
            "metadata": metadata(),
        }),
        &["metadata"],
    )
}

/// 页面元数据。
pub fn metadata() -> Value {
    object(
        json!({
            "scrapeId": string("Id of the kept browser page when keepSession was used; pass it to pagesift_interact."),
            "title": string("Page title."),
            "description": string("Meta description."),
            "language": string("Detected or declared language."),
            "keywords": string("Meta keywords."),
            "robots": string("robots meta directive."),
            "favicon": string("Favicon URL."),
            "publishedTime": string("Published time declared by the page."),
            "author": string("Author declared by the page."),
            "modifiedTime": string("Modified time declared by the page."),
            "url": string("Final URL after redirects."),
            "sourceURL": string("URL that was requested."),
            "statusCode": integer("HTTP status code of the response."),
            "contentType": string("Response Content-Type."),
            "error": string("Error note when the page was returned with an error status."),
            "proxyUsed": string("Egress or proxy used for the request."),
            "cacheState": { "type": "string", "enum": ["hit", "miss"], "description": "Whether the result came from the result cache." },
            "cachedAt": string("Time the cached copy was stored."),
            "pagesift": pagesift_meta(),
        }),
        &["statusCode", "proxyUsed"],
    )
}

/// `metadata.pagesift`：PageSift 扩展字段。
fn pagesift_meta() -> Value {
    object(
        json!({
            "contentHash": string("Hash of the cleaned content."),
            "rawHash": string("Hash of the raw response."),
            "textHash": string("Hash of the normalized text used for change tracking."),
            "redirectChain": array(string("URL"), "URLs visited before the final page."),
            "etag": string("ETag from the response."),
            "lastModified": string("Last-Modified from the response."),
            "notModified": boolean("The origin answered 304 to a conditional request."),
            "engine": { "type": "string", "enum": ["http", "browser", "upload"], "description": "Engine that produced the result." },
            "browserEngine": string("Browser kernel used for a rendered page."),
            "browserFingerprint": string("TLS fingerprint profile used for the request."),
            "itna": json!({ "description": "Access-barrier recovery attempts and their outcomes, when itna is enabled." }),
            "warnings": array(string("Warning"), "Warnings that may mean the content is incomplete."),
            "timings": json!({ "type": "object", "description": "Time per phase in milliseconds." }),
            "ocr": json!({ "description": "OCR engine, device and page counts for scanned PDFs." }),
            "duplicateOf": string("Canonical URL when this page duplicates another."),
            "relevance": { "type": "number", "description": "Query relevance score from 0 to 1." },
            "crawl": json!({ "description": "How this page was discovered during a crawl." }),
            "queryFilter": json!({ "description": "How many blocks the query filter kept and token counts." }),
            "jsonLd": array(json!({ "description": "Parsed JSON-LD object." }), "JSON-LD blocks found on the page."),
            "plan": json!({ "description": "Render and merge decisions recorded by explain." }),
        }),
        &[],
    )
}

/// `pagesift_map`。
pub fn map() -> Value {
    object(
        json!({
            "links": array(
                object(
                    json!({
                        "url": string("Absolute URL on the site."),
                        "title": string("Link title when known."),
                        "description": string("Link description when known."),
                    }),
                    &["url"],
                ),
                "URLs found in sitemaps, robots.txt and the homepage, after filtering and the limit.",
            ),
        }),
        &["links"],
    )
}

/// 启动 crawl 或 batch scrape 后返回的任务句柄。
pub fn job_started() -> Value {
    object(
        json!({
            "id": string("Job id; pass it to pagesift_poll_job or pagesift_cancel_job."),
            "status": { "type": "string", "enum": ["scraping"], "description": "The job has started." },
            "next": string("Suggested next call."),
            "invalidURLs": array(string("Invalid URL"), "URLs that were rejected before the job started."),
        }),
        &["id", "status"],
    )
}

/// `pagesift_poll_job`。
pub fn job_status() -> Value {
    object(
        json!({
            "status": { "type": "string", "enum": ["scraping", "completed", "cancelled", "failed"], "description": "Job state. completed, cancelled and failed are final." },
            "completed": integer("Pages completed so far, including failed pages."),
            "total": integer("Pages known so far; grows during a crawl."),
            "expiresAt": string("When the job and its pages are removed (24 hours after creation)."),
            "errors": integer("Number of page-level errors recorded so far."),
            "robotsBlocked": array(string("URL"), "URLs skipped because robots.txt disallows them."),
            "data": array(document(), "Pages collected so far."),
        }),
        &["status", "completed", "total", "expiresAt", "data"],
    )
}

/// `pagesift_cancel_job`。
pub fn job_cancelled() -> Value {
    object(
        json!({
            "success": boolean("The job exists and the cancel request was accepted."),
            "status": { "type": "string", "enum": ["scraping", "completed", "cancelled", "failed"], "description": "State after the request; finished jobs keep their state." },
            "completed": integer("Pages completed when the request was handled."),
            "total": integer("Pages known when the request was handled."),
        }),
        &["success", "status", "completed", "total"],
    )
}

/// `pagesift_interact`。
pub fn interact() -> Value {
    object(
        json!({
            "success": boolean("The action or prompt completed."),
            "output": string("Answer to a prompt (only with prompt)."),
            "result": string("Last browser action result, such as a JavaScript return value."),
            "error": string("Failure reason when the action did not complete."),
            "stdout": string("Always empty; pagesift does not execute shell commands."),
            "stderr": string("Always empty; pagesift does not execute shell commands."),
            "exitCode": integer("Exit code of the action run."),
            "killed": boolean("The run was stopped at the time limit."),
            "data": document(),
            "actions": json!({ "description": "Outputs of the requested actions, keyed by action." }),
            "steps": array(json!({ "type": "object", "description": "One step the prompt executed." }), "Steps executed for a prompt."),
            "warnings": array(string("Warning"), "Warnings from the interaction."),
        }),
        &["success"],
    )
}

/// `pagesift_interact_stop`。
pub fn interact_stopped() -> Value {
    object(
        json!({
            "success": boolean("The page was closed."),
            "sessionDurationMs": integer("How long the page stayed open, in milliseconds."),
        }),
        &["success", "sessionDurationMs"],
    )
}

/// `pagesift_report_issue`。
pub fn report_issue() -> Value {
    object(
        json!({
            "recorded": boolean("The report was written to the local audit log."),
            "message": string("Human-readable confirmation."),
        }),
        &["recorded", "message"],
    )
}

/// 启动 agent 后返回的任务句柄。
pub fn agent_started() -> Value {
    object(
        json!({
            "id": string("Agent job id; pass it to pagesift_agent_status."),
            "status": { "type": "string", "enum": ["processing"], "description": "The run has started." },
            "next": string("Suggested next call."),
        }),
        &["id", "status"],
    )
}

/// `pagesift_agent_status`。
pub fn agent_status() -> Value {
    object(
        json!({
            "success": boolean("The run completed without an error."),
            "status": { "type": "string", "enum": ["processing", "completed", "failed"], "description": "Run state." },
            "creditsUsed": integer("Pages, searches and maps used."),
            "expiresAt": string("When the run and its evidence are removed."),
            "effort": { "type": "string", "enum": ["low", "medium", "high"], "description": "Budget ceiling requested for the run." },
            "data": json!({ "description": "Answer; shape follows the requested schema." }),
            "partial": json!({ "description": "Partial answer when the run did not finish." }),
            "sources": array(object(json!({
                "url": string("Source URL."),
                "title": string("Source title."),
                "evidenceIds": array(string("Evidence id"), "Evidence taken from this source."),
            }), &["url"]), "Sources used by the answer."),
            "fieldSources": json!({ "type": "object", "description": "Which sources support each field of data, keyed by JSON Pointer." }),
            "usage": json!({ "type": "object", "description": "Pages, searches, model calls and tokens used." }),
            "warnings": array(string("Warning"), "Warnings from the run."),
            "stopReason": string("Why the run stopped."),
            "error": json!({ "description": "Failure details when status is failed." }),
        }),
        &["success", "status"],
    )
}
