//! 1.5 Loop MCP 工具定义：`pagesift_sync`（长期数据源同步）与扩展版 `pagesift_graph_query`。

use serde_json::json;

use crate::output;
use crate::tool::{ToolAnnotations, ToolDef};

pub const GRAPH_QUERY: &str = "pagesift_graph_query";
pub const SYNC: &str = "pagesift_sync";

pub fn graph_query_def() -> ToolDef {
    let schema = json!({
        "type": "object",
        "properties": {
            "jobId": { "type": "string", "description": "Crawl job id from pagesift_crawl to summarize its DataGraph." },
            "historyUrl": { "type": "string", "description": "URL or normalized resource key to query historical versions and diff edges." },
            "conflicts": { "type": "boolean", "description": "When true, return detected cross-source conflicts instead of a single job or URL." },
            "claims": { "type": "boolean", "description": "When true, return persisted claim lifecycle records instead of conflicts." },
            "subject": { "type": "string", "description": "Optional subject key filter for conflicts or claims (e.g. sku:PRO-1); implies conflicts=true when set alone." },
            "asOf": { "type": "string", "description": "Optional historical timestamp cutoff (Unix milliseconds string or RFC-3339 date) for time-travel queries." },
            "limit": { "type": "integer", "minimum": 1, "description": "Maximum number of history, conflict, or claim records to return." },
            "offset": { "type": "integer", "minimum": 0, "description": "Pagination offset for conflict or claim queries." }
        },
        "additionalProperties": false
    });
    ToolDef::new(GRAPH_QUERY, schema)
        .title("Query DataGraph")
        .description(
            "Query PageSift DataGraph results produced during crawls or archived in the store: \
             pass jobId for a crawl's GraphSummary, historyUrl for cross-time version lineage, \
             conflicts/subject for cross-source conflicts, or claims=true with optional asOf for claim lifecycle state.",
        )
        .output_schema(output::graph_query())
        .annotations(ToolAnnotations {
            title: Some("pagesift graph query".into()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(false),
        })
}

pub fn sync_def() -> ToolDef {
    let schema = json!({
        "type": "object",
        "required": ["action"],
        "properties": {
            "action": {
                "type": "string",
                "enum": ["create", "list", "get", "delete", "run", "state", "deltas"],
                "description": "Operation to perform on persistent Loop data sources: create, list, get, delete, run (trigger one sync tick), state (view resource inventory and active claims), or deltas (inspect tick deltas)."
            },
            "sourceId": { "type": "string", "description": "Identifier of the data source (required for get, delete, run, state, deltas; optional for create)." },
            "name": { "type": "string", "description": "Optional human-readable label when action='create'." },
            "seeds": {
                "type": "array",
                "items": { "type": "string", "format": "uri", "description": "Seed HTTP(S) URL for this data source." },
                "description": "Seed URLs defining the starting points and scope hosts for action='create'."
            },
            "includePaths": {
                "type": "array",
                "items": { "type": "string", "description": "URL path prefix or pattern to include." },
                "description": "Optional path prefixes to keep in scope for action='create'."
            },
            "excludePaths": {
                "type": "array",
                "items": { "type": "string", "description": "URL path prefix or pattern to exclude." },
                "description": "Optional path prefixes to exclude from scope for action='create'."
            },
            "maxDepth": { "type": "integer", "minimum": 0, "description": "Maximum discovery depth from seeds for action='create'." },
            "allowSubdomains": { "type": "boolean", "description": "Whether subdomains of seed hosts are allowed in scope for action='create'." },
            "requiredFields": {
                "type": "array",
                "items": { "type": "string", "description": "Required structured field name." },
                "description": "Structured fields required for capture sufficiency checks and adaptive browser escalation."
            },
            "allowBrowser": { "type": "boolean", "description": "Allow adaptive escalation to headless browser rendering during sync ticks." },
            "maxRequestsPerTick": { "type": "integer", "minimum": 1, "description": "Maximum HTTP/browser requests allowed per sync tick when action='create'." },
            "targetItems": { "type": "integer", "minimum": 1, "description": "Optional target count of substantive items for target_goal_satisfied coverage evaluation." },
            "forceAll": { "type": "boolean", "description": "When action='run', refresh discovered resources even if freshness timer has not elapsed." },
            "maxRequests": { "type": "integer", "minimum": 1, "description": "Override request budget for this single action='run' tick." },
            "asOf": { "type": "string", "description": "When action='state', evaluate resource inventory, claims, and conflicts as of a Unix ms string or RFC-3339 date." },
            "tickId": { "type": "string", "description": "When action='deltas', return one specific tick delta instead of the recent list." },
            "limit": { "type": "integer", "minimum": 1, "description": "Maximum number of deltas to list when action='deltas'." }
        },
        "additionalProperties": false
    });
    ToolDef::new(SYNC, schema)
        .title("Manage Loop Sync Sources")
        .description(
            "Create, run, inspect, and manage long-lived PageSift Loop data sources: schedule adaptive \
             incremental sync ticks, inspect bounded sync deltas, and query resource/claim state views.",
        )
        .output_schema(output::sync_result())
        .annotations(ToolAnnotations {
            title: Some("pagesift sync".into()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(false),
            open_world_hint: Some(true),
        })
}
