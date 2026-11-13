//! API request deserialization, option validation, and response types for PageSift DataGraph.

use pagesift_graph::{ConflictRecord, GraphOptions, GraphSummary, VersionEdge, VersionRecord};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::error::ApiError;

/// Deserialize `"graph": true | false | null | { ... }`.
///
/// An object without an explicit `"enabled": false` enables DataGraph while applying defaults for
/// omitted fields so callers can pass e.g. `{"subjectKeys": ["sku"]}`.
fn parse_graph_raw(raw: Value) -> Result<Option<GraphOptions>, String> {
    match raw {
        Value::Null | Value::Bool(false) => Ok(None),
        Value::Bool(true) => Ok(Some(GraphOptions::enabled())),
        Value::Object(map) => {
            if map.get("enabled").and_then(Value::as_bool) == Some(false) {
                return Ok(None);
            }
            let mut opts: GraphOptions =
                serde_json::from_value(Value::Object(map)).map_err(|e| e.to_string())?;
            opts.enabled = true;
            Ok(Some(opts))
        }
        other => Err(format!("graph must be a boolean or object, got {other}")),
    }
}

pub fn parse_graph_options<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<GraphOptions>, D::Error> {
    let Some(raw) = Option::<Value>::deserialize(d)? else {
        return Ok(None);
    };
    parse_graph_raw(raw).map_err(serde::de::Error::custom)
}

/// Parse and validate a binding or CLI JSON value into `Option<GraphOptions>`.
pub fn parse_graph_value(raw: Option<Value>) -> Result<Option<GraphOptions>, ApiError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let opts = parse_graph_raw(raw).map_err(ApiError::invalid)?;
    if let Some(g) = &opts {
        check_graph_options(g)?;
    }
    Ok(opts)
}

/// Validate configured DataGraph limits and subject keys at request submission time.
pub fn check_graph_options(opts: &GraphOptions) -> Result<(), ApiError> {
    const MAX_EDGES_PER_DOC: usize = 4096; // [策略] 单文档边界上限
    const MAX_ANCHOR_SAMPLES: usize = 32; // [策略] 锚文本采样上限
    const MAX_CLAIMS_PER_BUCKET: usize = 1024; // [策略] 单分桶声明上限
    const MAX_BLOCKS_PER_DOC: usize = 65536; // [策略] 单文档块数上限
    const MAX_EXCERPT_CHARS: usize = 2048; // [策略] 摘录截断字符上限
    const MAX_SUBJECT_KEYS: usize = 32; // [策略] 实体键数量上限

    let l = &opts.limits;
    if l.max_edges_per_doc == 0 || l.max_edges_per_doc > MAX_EDGES_PER_DOC {
        return Err(ApiError::invalid(
            "graph.limits.maxEdgesPerDoc must be between 1 and 4096",
        ));
    }
    if l.max_anchor_samples > MAX_ANCHOR_SAMPLES {
        return Err(ApiError::invalid(
            "graph.limits.maxAnchorSamples must not exceed 32",
        ));
    }
    if l.max_claims_per_bucket == 0 || l.max_claims_per_bucket > MAX_CLAIMS_PER_BUCKET {
        return Err(ApiError::invalid(
            "graph.limits.maxClaimsPerBucket must be between 1 and 1024",
        ));
    }
    if l.max_blocks_per_doc == 0 || l.max_blocks_per_doc > MAX_BLOCKS_PER_DOC {
        return Err(ApiError::invalid(
            "graph.limits.maxBlocksPerDoc must be between 1 and 65536",
        ));
    }
    if l.max_excerpt_chars == 0 || l.max_excerpt_chars > MAX_EXCERPT_CHARS {
        return Err(ApiError::invalid(
            "graph.limits.maxExcerptChars must be between 1 and 2048",
        ));
    }
    if opts.subject_keys.len() > MAX_SUBJECT_KEYS {
        return Err(ApiError::invalid(
            "graph.subjectKeys must contain at most 32 keys",
        ));
    }
    if opts.subject_keys.iter().any(|k| k.trim().is_empty()) {
        return Err(ApiError::invalid(
            "graph.subjectKeys must not contain blank entries",
        ));
    }
    Ok(())
}

/// Response payload for `GET /v2/crawl/{id}/graph`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrawlGraphResponse {
    pub success: bool,
    pub id: String,
    pub status: String,
    #[serde(flatten)]
    pub summary: GraphSummary,
}

/// Response payload for `GET /v2/graph/history?url=...`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphHistoryResponse {
    pub success: bool,
    pub url: String,
    pub resource_key: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<VersionRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<VersionEdge>,
}

/// Response payload for `GET /v2/graph/conflicts?subject=...`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphConflictsResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub total: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<ConflictRecord>,
}
