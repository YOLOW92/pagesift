//! MCP `pagesift_context` handler delegating to `Engine::context`.

use std::sync::Arc;

use pagesift_api::Engine;
use pagesift_mcp::{
    BoxFuture, ToolContext, ToolDef, ToolError, ToolHandler, ToolOutput, ToolResult, tools,
};
use serde_json::{Map, Value, json};

pub(crate) struct ContextTool {
    pub(crate) engine: Arc<dyn Engine>,
}

impl ToolHandler for ContextTool {
    fn definition(&self) -> ToolDef {
        tools::context_def()
    }

    fn call(&self, args: Map<String, Value>, _ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
        Box::pin(async move {
            let out = self
                .engine
                .context(None, Value::Object(args))
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
