//! MCP `pagesift_graph_query` handler backed by `pagesift_api::Jobs`.

use std::collections::BTreeSet;

use pagesift_api::types::{CrawlGraphResponse, GraphConflictsResponse, GraphHistoryResponse};
use pagesift_api::{ApiError, Jobs};
use pagesift_mcp::{
    BoxFuture, ToolContext, ToolDef, ToolError, ToolHandler, ToolOutput, ToolResult, tools,
};
use serde_json::{Map, Value, json};

pub(crate) struct GraphQueryTool(pub(crate) Jobs);

fn bad(e: ApiError) -> ToolError {
    ToolError::new(format!("{}: {}", e.code.as_str(), e.message))
}

impl ToolHandler for GraphQueryTool {
    fn definition(&self) -> ToolDef {
        tools::graph_query_def()
    }

    fn call(&self, args: Map<String, Value>, _ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            let allowed: BTreeSet<&str> = ["jobId", "historyUrl", "conflicts", "subject"]
                .into_iter()
                .collect();
            for k in args.keys() {
                if !allowed.contains(k.as_str()) {
                    return Err(ToolError::new(format!("unknown argument `{k}`")));
                }
            }

            if let Some(id) = args.get("jobId").and_then(Value::as_str) {
                let id = id.trim();
                if id.is_empty() {
                    return Err(ToolError::new("jobId must not be blank"));
                }
                let (state, summary) = self
                    .0
                    .crawl_graph(id)
                    .await
                    .map_err(bad)?
                    .ok_or_else(|| ToolError::new(format!("Unknown job id: {id}")))?;
                let resp = CrawlGraphResponse {
                    success: true,
                    id: id.to_string(),
                    status: state.as_str().into(),
                    summary,
                };
                return Ok(ToolOutput::json(
                    serde_json::to_value(resp).map_err(|e| ToolError::new(e.to_string()))?,
                ));
            }

            if let Some(url) = args.get("historyUrl").and_then(Value::as_str) {
                let target = url.trim();
                if target.is_empty() {
                    return Err(ToolError::new("historyUrl must not be blank"));
                }
                let res = self
                    .0
                    .graph_history_for(None, target, pagesift_store::DEFAULT_GRAPH_HISTORY_LIMIT)
                    .await
                    .map_err(bad)?;
                let resp = GraphHistoryResponse {
                    success: true,
                    url: target.to_string(),
                    resource_key: res.resource_key,
                    versions: res.versions,
                    edges: res.edges,
                    truncated: res.truncated,
                };
                return Ok(ToolOutput::json(
                    serde_json::to_value(resp).map_err(|e| ToolError::new(e.to_string()))?,
                ));
            }

            let subject = args
                .get("subject")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let want_conflicts =
                args.get("conflicts").and_then(Value::as_bool) == Some(true) || subject.is_some();
            if want_conflicts {
                let (total, conflicts, truncated) = self
                    .0
                    .graph_conflicts_for(
                        None,
                        subject.as_deref(),
                        pagesift_store::DEFAULT_GRAPH_CONFLICTS_LIMIT,
                        0,
                    )
                    .await
                    .map_err(bad)?;
                let resp = GraphConflictsResponse {
                    success: true,
                    subject,
                    total,
                    conflicts,
                    truncated,
                };
                return Ok(ToolOutput::json(
                    serde_json::to_value(resp).map_err(|e| ToolError::new(e.to_string()))?,
                ));
            }

            Err(ToolError::new(
                json!({
                    "message": "provide jobId, historyUrl, or conflicts/subject"
                })
                .to_string(),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Notify;

    use pagesift_api::types::{CrawlRequest, Document, ScrapeOptions};
    use pagesift_api::{BoxFuture, CrawlSink, Engine};
    use pagesift_graph::{DocumentInput, GraphOptions, SourceKind, StreamingGraphBuilder};
    use pagesift_store::Store;

    fn sample_doc(url: &str, price: &str, ts: u64) -> DocumentInput {
        DocumentInput {
            requested_url: url.into(),
            final_url: url.into(),
            canonical_url: Some("https://example.com/pricing".into()),
            canonical_from_header: false,
            source_kind: SourceKind::Html,
            observed_at_ms: ts,
            status_code: 200,
            engine: "http".into(),
            content_hash: Some(format!("hash-{ts}-{url}")),
            raw_hash: None,
            text_hash: None,
            etag: None,
            last_modified: None,
            title: Some("Pricing".into()),
            markdown: format!("Plan costs {price}"),
            redirect_chain: Vec::new(),
            links: Vec::new(),
            alternates: Vec::new(),
            duplicate_of: None,
            attachments: Vec::new(),
            structured_json_ld: Vec::new(),
            extracted_fields: Some(json!({"price": price})),
            raw_traces: Vec::new(),
            chunk_spans: Vec::new(),
            previous_version: None,
            version_diff_stats: None,
            changed_block_ids: Vec::new(),
        }
    }

    struct StubGraphEngine(Arc<Notify>);

    impl Engine for StubGraphEngine {
        fn scrape<'a>(
            &'a self,
            _url: &'a str,
            _opts: &'a ScrapeOptions,
        ) -> BoxFuture<'a, Result<Document, ApiError>> {
            Box::pin(async { Err(ApiError::unsupported("unused")) })
        }

        fn crawl<'a>(
            &'a self,
            _req: &'a CrawlRequest,
            sink: Arc<dyn CrawlSink>,
        ) -> BoxFuture<'a, Result<(), ApiError>> {
            Box::pin(async move {
                let mut builder = StreamingGraphBuilder::new(GraphOptions::enabled());
                let d1 = builder.ingest(sample_doc("https://example.com/pricing", "$20", 1_000));
                let d2 = builder.ingest(sample_doc(
                    "https://example.com/pricing-legacy",
                    "$30",
                    2_000,
                ));
                let summary = builder.finalize();
                sink.graph_delta(d1).await;
                sink.graph_delta(d2).await;
                sink.graph_summary(summary).await;
                self.0.notify_one();
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn graph_query_tool_handles_job_history_and_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_dir(dir.path()).unwrap();
        let jobs = Jobs::with_store(Some(store));

        let req: CrawlRequest = serde_json::from_value(json!({
            "url": "https://example.com/",
            "graph": true,
        }))
        .unwrap();
        let done = Arc::new(Notify::new());
        let job_id = jobs
            .start_crawl(Arc::new(StubGraphEngine(done.clone())), req)
            .await;
        done.notified().await;
        tokio::task::yield_now().await;

        let tool = GraphQueryTool(jobs);
        let ctx = ToolContext::default();

        let job_out = tool
            .call(
                json!({"jobId": job_id}).as_object().unwrap().clone(),
                ctx.clone(),
            )
            .await
            .unwrap();
        let job_json = job_out.structured_content.expect("structured json");
        assert_eq!(job_json["success"], true);
        assert!(job_json["nodes"].as_array().unwrap().len() >= 2);

        let hist_out = tool
            .call(
                json!({"historyUrl": "https://example.com/pricing"})
                    .as_object()
                    .unwrap()
                    .clone(),
                ctx.clone(),
            )
            .await
            .unwrap();
        let hist_json = hist_out.structured_content.expect("structured json");
        assert_eq!(hist_json["resourceKey"], "https://example.com/pricing");
        assert!(!hist_json["versions"].as_array().unwrap().is_empty());

        let conf_out = tool
            .call(json!({"conflicts": true}).as_object().unwrap().clone(), ctx)
            .await
            .unwrap();
        let conf_json = conf_out.structured_content.expect("structured json");
        assert_eq!(conf_json["success"], true);
        let n = conf_json
            .get("conflicts")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        assert_eq!(conf_json["total"], n);
        assert!(n >= 1);
    }
}
