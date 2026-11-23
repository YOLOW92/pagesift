//! MCP `pagesift_resolve` handler delegating to `Engine::resolve`.

use std::sync::Arc;

use pagesift_api::Engine;
use pagesift_mcp::{
    BoxFuture, ToolContext, ToolDef, ToolError, ToolHandler, ToolOutput, ToolResult, tools,
};
use serde_json::{Map, Value, json};

pub(crate) struct ResolveTool {
    pub(crate) engine: Arc<dyn Engine>,
}

impl ToolHandler for ResolveTool {
    fn definition(&self) -> ToolDef {
        tools::resolve_def()
    }

    fn call(&self, args: Map<String, Value>, _ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            let out = self
                .engine
                .resolve(None, Value::Object(args))
                .await
                .map_err(|e| {
                    let mut details = json!({ "code": e.code.as_str() });
                    if let Some(d) = e.details {
                        details["details"] = d;
                    }
                    ToolError::new(format!("{}: {}", e.code.as_str(), e.message))
                        .with_details(details)
                })?;
            Ok(ToolOutput::json(out))
        })
    }
}
