//! MCP `pagesift_sync` handler backed by `Engine` and `pagesift_store::Store`.

use std::collections::BTreeSet;
use std::sync::Arc;

use pagesift_api::sources::{CreateSourceRequest, build_source_spec, validate_source_id};
use pagesift_api::{ApiError, Engine};
use pagesift_mcp::{
    BoxFuture, ToolContext, ToolDef, ToolError, ToolHandler, ToolOutput, ToolResult, tools,
};
use pagesift_store::{FreshnessPolicy, MAX_DELTAS_PER_SOURCE, Store, SyncBudget};
use serde_json::{Map, Value, json};

pub(crate) struct SyncTool {
    pub(crate) engine: Arc<dyn Engine>,
    pub(crate) store: Option<Store>,
}

fn bad(e: ApiError) -> ToolError {
    ToolError::new(format!("{}: {}", e.code.as_str(), e.message))
}

fn parse_as_of_arg(args: &Map<String, Value>) -> Result<Option<u64>, ToolError> {
    let Some(v) = args.get("asOf") else {
        return Ok(None);
    };
    let Some(raw) = v.as_str() else {
        return Err(ToolError::new("asOf must be a string"));
    };
    let s = raw.trim();
    if s.is_empty() {
        return Ok(None);
    }
    if let Ok(ms) = s.parse::<u64>() {
        return Ok(Some(ms));
    }
    pagesift_graph::parse_timestamp_ms(s)
        .map(Some)
        .ok_or_else(|| ToolError::new(format!("invalid asOf timestamp {s:?}")))
}

fn string_vec(args: &Map<String, Value>, key: &str) -> Result<Vec<String>, ToolError> {
    let Some(val) = args.get(key) else {
        return Ok(Vec::new());
    };
    let Some(arr) = val.as_array() else {
        return Err(ToolError::new(format!("{key} must be an array of strings")));
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(s) = item.as_str() else {
            return Err(ToolError::new(format!("{key} entries must be strings")));
        };
        out.push(s.to_string());
    }
    Ok(out)
}

fn require_source_id(args: &Map<String, Value>) -> Result<String, ToolError> {
    let raw = args
        .get("sourceId")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::new("sourceId is required for this action"))?;
    validate_source_id(raw).map_err(bad)
}

impl ToolHandler for SyncTool {
    fn definition(&self) -> ToolDef {
        tools::sync_def()
    }

    fn call(&self, args: Map<String, Value>, _ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            let allowed: BTreeSet<&str> = [
                "action",
                "sourceId",
                "name",
                "seeds",
                "includePaths",
                "excludePaths",
                "maxDepth",
                "allowSubdomains",
                "requiredFields",
                "allowBrowser",
                "maxRequestsPerTick",
                "targetItems",
                "forceAll",
                "maxRequests",
                "asOf",
                "tickId",
                "limit",
            ]
            .into_iter()
            .collect();
            for k in args.keys() {
                if !allowed.contains(k.as_str()) {
                    return Err(ToolError::new(format!("unknown argument `{k}`")));
                }
            }

            let action = args
                .get("action")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| ToolError::new("action is required"))?
                .to_string();

            let store = self.store.clone().ok_or_else(|| {
                ToolError::new("pagesift_sync requires persistent storage enabled on the server")
            })?;

            match action.as_str() {
                "create" => {
                    let seeds = string_vec(&args, "seeds")?;
                    let mut budget = SyncBudget::default();
                    if let Some(n) = args.get("maxRequestsPerTick").and_then(Value::as_u64) {
                        budget.max_requests_per_tick = usize::try_from(n)
                            .map_err(|_| ToolError::new("maxRequestsPerTick is out of range"))?;
                    }
                    if let Some(n) = args.get("targetItems").and_then(Value::as_u64) {
                        budget.target_items = Some(
                            usize::try_from(n)
                                .map_err(|_| ToolError::new("targetItems is out of range"))?,
                        );
                    }
                    let max_depth = args
                        .get("maxDepth")
                        .and_then(Value::as_u64)
                        .map(|n| usize::try_from(n).unwrap_or(usize::MAX))
                        .unwrap_or(2);
                    let req = CreateSourceRequest {
                        id: args
                            .get("sourceId")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        name: args.get("name").and_then(Value::as_str).map(str::to_string),
                        seeds,
                        include_paths: string_vec(&args, "includePaths")?,
                        exclude_paths: string_vec(&args, "excludePaths")?,
                        max_depth,
                        allow_subdomains: args
                            .get("allowSubdomains")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        freshness: FreshnessPolicy::default(),
                        budget,
                        extract_schema: None,
                        extract_prompt: None,
                        required_fields: string_vec(&args, "requiredFields")?,
                        allow_browser: args
                            .get("allowBrowser")
                            .and_then(Value::as_bool)
                            .unwrap_or(true),
                    };
                    let mut spec = build_source_spec(req, None).map_err(bad)?;
                    let id = spec.id.clone();
                    let saved = store
                        .blocking(move |st| {
                            let sync = st.sync();
                            if let Some(prev) = sync.get_source(&id)? {
                                spec.created_at_ms = prev.created_at_ms;
                            }
                            sync.put_source(&spec)?;
                            Ok::<_, pagesift_store::Error>(spec)
                        })
                        .await
                        .map_err(|e| ToolError::new(format!("save source failed: {e}")))?;
                    Ok(ToolOutput::json(
                        json!({ "success": true, "action": "create", "source": saved }),
                    ))
                }
                "list" => {
                    let sources = store
                        .blocking(|st| st.sync().list_sources())
                        .await
                        .map_err(|e| ToolError::new(format!("list sources failed: {e}")))?;
                    Ok(ToolOutput::json(json!({
                        "success": true,
                        "action": "list",
                        "total": sources.len(),
                        "sources": sources,
                    })))
                }
                "get" => {
                    let id = require_source_id(&args)?;
                    let lookup = id.clone();
                    let source = store
                        .blocking(move |st| st.sync().get_source(&lookup))
                        .await
                        .map_err(|e| ToolError::new(format!("get source failed: {e}")))?
                        .ok_or_else(|| ToolError::new(format!("data source '{id}' not found")))?;
                    Ok(ToolOutput::json(json!({
                        "success": true,
                        "action": "get",
                        "source": source,
                    })))
                }
                "delete" => {
                    let id = require_source_id(&args)?;
                    let del_id = id.clone();
                    let deleted = store
                        .blocking(move |st| st.sync().delete_source(&del_id))
                        .await
                        .map_err(|e| ToolError::new(format!("delete source failed: {e}")))?;
                    Ok(ToolOutput::json(json!({
                        "success": true,
                        "action": "delete",
                        "id": id,
                        "deleted": deleted,
                    })))
                }
                "run" => {
                    let id = require_source_id(&args)?;
                    let force_all = args
                        .get("forceAll")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let max_requests = match args.get("maxRequests").and_then(Value::as_u64) {
                        Some(0) => return Err(ToolError::new("maxRequests must be >= 1")),
                        Some(n) => Some(usize::try_from(n).unwrap_or(usize::MAX)),
                        None => None,
                    };
                    let delta = self
                        .engine
                        .sync_source(&id, None, force_all, max_requests)
                        .await
                        .map_err(bad)?;
                    Ok(ToolOutput::json(json!({
                        "success": true,
                        "action": "run",
                        "delta": delta,
                    })))
                }
                "state" => {
                    let id = require_source_id(&args)?;
                    let as_of_ms = parse_as_of_arg(&args)?;
                    let lookup = id.clone();
                    let state = store
                        .blocking(move |st| st.sync().build_state_view(&lookup, as_of_ms))
                        .await
                        .map_err(|e| ToolError::new(format!("build state view failed: {e}")))?
                        .ok_or_else(|| ToolError::new(format!("data source '{id}' not found")))?;
                    Ok(ToolOutput::json(json!({
                        "success": true,
                        "action": "state",
                        "state": state,
                    })))
                }
                "deltas" => {
                    let id = require_source_id(&args)?;
                    let tick_id = args
                        .get("tickId")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    if let Some(tick) = tick_id {
                        let lookup = id.clone();
                        let lookup_tick = tick.clone();
                        let delta = store
                            .blocking(move |st| st.sync().get_delta(&lookup, &lookup_tick))
                            .await
                            .map_err(|e| ToolError::new(format!("get delta failed: {e}")))?
                            .ok_or_else(|| {
                                ToolError::new(format!("delta '{tick}' for '{id}' not found"))
                            })?;
                        return Ok(ToolOutput::json(json!({
                            "success": true,
                            "action": "deltas",
                            "delta": delta,
                        })));
                    }
                    let limit = args
                        .get("limit")
                        .and_then(Value::as_u64)
                        .map(|n| usize::try_from(n).unwrap_or(MAX_DELTAS_PER_SOURCE))
                        .unwrap_or(20)
                        .clamp(1, MAX_DELTAS_PER_SOURCE);
                    let lookup = id.clone();
                    let deltas = store
                        .blocking(move |st| st.sync().list_deltas(&lookup, limit))
                        .await
                        .map_err(|e| ToolError::new(format!("list deltas failed: {e}")))?;
                    Ok(ToolOutput::json(json!({
                        "success": true,
                        "action": "deltas",
                        "total": deltas.len(),
                        "deltas": deltas,
                    })))
                }
                other => Err(ToolError::new(format!("unsupported sync action {other:?}"))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_api::BoxFuture;
    use pagesift_api::types::{Document, ScrapeOptions};

    struct DummyEngine;

    impl Engine for DummyEngine {
        fn scrape<'a>(
            &'a self,
            _url: &'a str,
            _opts: &'a ScrapeOptions,
        ) -> BoxFuture<'a, Result<Document, ApiError>> {
            Box::pin(async { Err(ApiError::unsupported("unused")) })
        }
    }

    #[tokio::test]
    async fn sync_tool_manages_sources_and_queries_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_dir(dir.path()).unwrap();
        let tool = SyncTool {
            engine: Arc::new(DummyEngine),
            store: Some(store),
        };
        let ctx = ToolContext::default();

        let created = tool
            .call(
                json!({
                    "action": "create",
                    "sourceId": "docs-src",
                    "seeds": ["https://example.com/docs"],
                    "requiredFields": ["price"]
                })
                .as_object()
                .unwrap()
                .clone(),
                ctx.clone(),
            )
            .await
            .unwrap();
        let created_json = created.structured_content.unwrap();
        assert_eq!(created_json["source"]["id"], "docs-src");

        let listed = tool
            .call(
                json!({"action": "list"}).as_object().unwrap().clone(),
                ctx.clone(),
            )
            .await
            .unwrap();
        assert_eq!(listed.structured_content.unwrap()["total"], 1);

        let state = tool
            .call(
                json!({"action": "state", "sourceId": "docs-src"})
                    .as_object()
                    .unwrap()
                    .clone(),
                ctx,
            )
            .await
            .unwrap();
        assert_eq!(
            state.structured_content.unwrap()["state"]["spec"]["id"],
            "docs-src"
        );
    }
}
