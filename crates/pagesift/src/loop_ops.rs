//! Shared 1.5 Loop helpers for FFI, Node, and Python bindings.

use pagesift_api::sources::{
    CreateSourceRequest, DeleteSourceResponse, MAX_REQUESTS_PER_TICK_CAP, SourceDeltaResponse,
    SourceDeltasResponse, SourceResponse, SourceStateResponse, SourcesListResponse,
    SyncTickResponse, build_source_spec, validate_source_id,
};
use pagesift_api::types::{GraphClaimsResponse, GraphConflictsResponse, GraphHistoryResponse};
use pagesift_api::{
    ApiError, Engine, ErrorCode, store_graph_claims, store_graph_conflicts, store_graph_history,
};
use pagesift_engine::Scraper;
use pagesift_store::{
    DEFAULT_GRAPH_CONFLICTS_LIMIT, DEFAULT_GRAPH_HISTORY_LIMIT, MAX_DELTAS_PER_SOURCE, Store,
};
use serde::Serialize;
use serde_json::{Map, Value};

const CREATE_KEYS: &[&str] = &[
    "action",
    "id",
    "sourceId",
    "name",
    "seeds",
    "includePaths",
    "excludePaths",
    "maxDepth",
    "allowSubdomains",
    "freshness",
    "budget",
    "maxRequestsPerTick",
    "maxBrowserRendersPerTick",
    "maxWallClockMs",
    "targetItems",
    "extractSchema",
    "extractPrompt",
    "requiredFields",
    "allowBrowser",
];

const BUDGET_KEYS: &[&str] = &[
    "maxRequestsPerTick",
    "maxBrowserRendersPerTick",
    "maxWallClockMs",
    "targetItems",
];

const FRESHNESS_KEYS: &[&str] = &[
    "minIntervalSecs",
    "maxIntervalSecs",
    "defaultIntervalSecs",
    "confirmGoneAfter404s",
];

const GRAPH_KEYS: &[&str] = &[
    "historyUrl",
    "url",
    "claims",
    "conflicts",
    "subject",
    "asOf",
    "limit",
    "offset",
];

/// Runs a persistent Loop sync source operation (`create`, `list`, `get`, `delete`, `run`, `state`, `deltas`).
pub async fn sync(scraper: &Scraper, request: Value) -> Result<Value, ApiError> {
    let mut args = normalize_object(request, &["freshness", "budget"])?;
    let action = require_non_empty_str(&args, "action")?;
    let store = require_store(scraper, "sync")?;

    match action.as_str() {
        "create" => sync_create(store, &mut args).await,
        "list" => {
            reject_unknown(&args, &["action"])?;
            let sources = store
                .blocking(|st| st.sync().list_sources())
                .await
                .map_err(|e| internal("list sources", e))?;
            to_json(SourcesListResponse {
                success: true,
                total: sources.len(),
                sources,
            })
        }
        "get" => {
            reject_unknown(&args, &["action", "id", "sourceId"])?;
            let id = require_id(&args)?;
            let lookup = id.clone();
            let source = store
                .blocking(move |st| st.sync().get_source(&lookup))
                .await
                .map_err(|e| internal("get source", e))?
                .ok_or_else(|| not_found_source(&id))?;
            to_json(SourceResponse {
                success: true,
                source,
            })
        }
        "delete" => {
            reject_unknown(&args, &["action", "id", "sourceId"])?;
            let id = require_id(&args)?;
            let del_id = id.clone();
            let deleted = store
                .blocking(move |st| st.sync().delete_source(&del_id))
                .await
                .map_err(|e| internal("delete source", e))?;
            if !deleted {
                return Err(not_found_source(&id));
            }
            to_json(DeleteSourceResponse {
                success: true,
                id,
                deleted: true,
            })
        }
        "run" => {
            reject_unknown(
                &args,
                &["action", "id", "sourceId", "forceAll", "maxRequests"],
            )?;
            let id = require_id(&args)?;
            let force_all = optional_bool(&args, "forceAll")?.unwrap_or(false);
            let max_requests = optional_bound(&args, "maxRequests", 1, MAX_REQUESTS_PER_TICK_CAP)?;
            let delta =
                <Scraper as Engine>::sync_source(scraper, &id, None, force_all, max_requests)
                    .await?;
            to_json(SyncTickResponse {
                success: true,
                delta,
            })
        }
        "state" => {
            reject_unknown(&args, &["action", "id", "sourceId", "asOf"])?;
            let id = require_id(&args)?;
            let as_of_ms = parse_as_of(args.get("asOf"))?;
            let lookup = id.clone();
            let state = store
                .blocking(move |st| st.sync().build_state_view(&lookup, as_of_ms))
                .await
                .map_err(|e| internal("build source state", e))?
                .ok_or_else(|| not_found_source(&id))?;
            to_json(SourceStateResponse {
                success: true,
                state,
            })
        }
        "deltas" => sync_deltas(store, &args).await,
        other => Err(ApiError::invalid(format!(
            "unsupported sync action {other:?}"
        ))),
    }
}

/// Queries the persistent DataGraph store for version history, entity conflicts, or claim lifecycles.
pub async fn query_graph(scraper: &Scraper, request: Value) -> Result<Value, ApiError> {
    let args = normalize_object(request, &[])?;
    reject_unknown(&args, GRAPH_KEYS)?;
    let store = require_store(scraper, "query_graph")?;

    let as_of_ms = parse_as_of(args.get("asOf"))?;
    let history_url =
        optional_trimmed_str(&args, "historyUrl")?.or(optional_trimmed_str(&args, "url")?);
    let subject = optional_trimmed_str(&args, "subject")?;

    if optional_bool(&args, "claims")?.unwrap_or(false) {
        let limit = optional_bound(&args, "limit", 1, DEFAULT_GRAPH_CONFLICTS_LIMIT)?
            .unwrap_or(DEFAULT_GRAPH_CONFLICTS_LIMIT);
        let offset = optional_bound(&args, "offset", 0, usize::MAX)?.unwrap_or(0);
        let (total, claims, truncated) = store_graph_claims(
            store,
            None,
            history_url.as_deref(),
            subject.as_deref(),
            as_of_ms,
            limit,
            offset,
        )
        .await?;
        return to_json(GraphClaimsResponse {
            success: true,
            subject,
            url: history_url,
            as_of_ms,
            total,
            claims,
            truncated,
        });
    }

    if let Some(target) = history_url {
        let limit = optional_bound(&args, "limit", 1, DEFAULT_GRAPH_HISTORY_LIMIT)?
            .unwrap_or(DEFAULT_GRAPH_HISTORY_LIMIT);
        let res = store_graph_history(store, None, &target, as_of_ms, limit).await?;
        return to_json(GraphHistoryResponse {
            success: true,
            url: target,
            resource_key: res.resource_key,
            versions: res.versions,
            edges: res.edges,
            truncated: res.truncated,
        });
    }

    let want_conflicts = optional_bool(&args, "conflicts")?.unwrap_or(false) || subject.is_some();
    if want_conflicts {
        let limit = optional_bound(&args, "limit", 1, DEFAULT_GRAPH_CONFLICTS_LIMIT)?
            .unwrap_or(DEFAULT_GRAPH_CONFLICTS_LIMIT);
        let offset = optional_bound(&args, "offset", 0, usize::MAX)?.unwrap_or(0);
        let (total, conflicts, truncated) =
            store_graph_conflicts(store, None, subject.as_deref(), as_of_ms, limit, offset).await?;
        return to_json(GraphConflictsResponse {
            success: true,
            subject,
            total,
            conflicts,
            truncated,
        });
    }

    Err(ApiError::invalid(
        "provide historyUrl (or url), claims, or conflicts/subject",
    ))
}

async fn sync_create(store: &Store, args: &mut Map<String, Value>) -> Result<Value, ApiError> {
    reject_unknown(args, CREATE_KEYS)?;
    args.remove("action");
    if let Some(source_id) = args.remove("sourceId") {
        args.entry("id").or_insert(source_id);
    }
    if let Some(Value::Object(freshness)) = args.get("freshness") {
        reject_unknown(freshness, FRESHNESS_KEYS)?;
    }
    merge_budget_shortcuts(args)?;

    let req: CreateSourceRequest = serde_json::from_value(Value::Object(std::mem::take(args)))
        .map_err(|e| ApiError::invalid(format!("invalid create source payload: {e}")))?;
    let spec = build_source_spec(req, None)?;
    let saved = store
        .blocking(move |st| st.sync().upsert_source(spec))
        .await
        .map_err(|e| internal("save source", e))?;
    to_json(SourceResponse {
        success: true,
        source: saved,
    })
}

async fn sync_deltas(store: &Store, args: &Map<String, Value>) -> Result<Value, ApiError> {
    reject_unknown(args, &["action", "id", "sourceId", "tickId", "limit"])?;
    let id = require_id(args)?;
    if let Some(tick) = optional_trimmed_str(args, "tickId")? {
        let lookup = id.clone();
        let lookup_tick = tick.clone();
        let delta = store
            .blocking(move |st| st.sync().get_delta(&lookup, &lookup_tick))
            .await
            .map_err(|e| internal("get delta", e))?
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::NotFound,
                    format!("delta '{tick}' for source '{id}' not found"),
                )
            })?;
        return to_json(SourceDeltaResponse {
            success: true,
            delta,
        });
    }

    let limit = optional_bound(args, "limit", 1, MAX_DELTAS_PER_SOURCE)?
        .unwrap_or(20.min(MAX_DELTAS_PER_SOURCE));
    let lookup = id.clone();
    let deltas = store
        .blocking(move |st| {
            let sync = st.sync();
            if sync.get_source(&lookup)?.is_none() {
                return Ok::<_, pagesift_store::Error>(None);
            }
            Ok(Some(sync.list_deltas(&lookup, limit)?))
        })
        .await
        .map_err(|e| internal("list deltas", e))?
        .ok_or_else(|| not_found_source(&id))?;

    to_json(SourceDeltasResponse {
        success: true,
        source_id: id,
        total: deltas.len(),
        deltas,
    })
}

fn merge_budget_shortcuts(args: &mut Map<String, Value>) -> Result<(), ApiError> {
    let mut budget = match args.remove("budget") {
        Some(Value::Null) | None => Map::new(),
        Some(Value::Object(m)) => {
            reject_unknown(&m, BUDGET_KEYS)?;
            m
        }
        Some(_) => return Err(ApiError::invalid("budget must be a JSON object")),
    };
    for &key in BUDGET_KEYS {
        if let Some(val) = args.remove(key) {
            budget.entry(key).or_insert(val);
        }
    }
    if !budget.is_empty() {
        args.insert("budget".into(), Value::Object(budget));
    }
    Ok(())
}

fn require_store<'a>(scraper: &'a Scraper, op: &str) -> Result<&'a Store, ApiError> {
    scraper.store_ref().ok_or_else(|| {
        ApiError::invalid(format!(
            "{op} requires persistent storage (configure storePath / store_path)"
        ))
    })
}

fn not_found_source(id: &str) -> ApiError {
    ApiError::new(ErrorCode::NotFound, format!("data source '{id}' not found"))
}

fn internal(what: &str, err: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::Internal, format!("{what} failed: {err}"))
}

fn to_json(val: impl Serialize) -> Result<Value, ApiError> {
    serde_json::to_value(val).map_err(|e| internal("serialize response", e))
}

fn normalize_object(v: Value, nested_object_keys: &[&str]) -> Result<Map<String, Value>, ApiError> {
    let Value::Object(map) = v else {
        return Err(ApiError::invalid("request must be a JSON object"));
    };
    let mut out = Map::with_capacity(map.len());
    for (k, val) in map {
        let camel = snake_to_camel(&k);
        let normalized = if nested_object_keys.contains(&camel.as_str()) {
            match val {
                Value::Object(_) => Value::Object(normalize_object(val, &[])?),
                other => other,
            }
        } else {
            val
        };
        if out.insert(camel.clone(), normalized).is_some() {
            return Err(ApiError::invalid(format!("duplicate option key `{camel}`")));
        }
    }
    Ok(out)
}

fn snake_to_camel(s: &str) -> String {
    if !s.contains('_') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut upper = false;
    for ch in s.chars() {
        if ch == '_' {
            upper = !out.is_empty();
        } else if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

fn reject_unknown(map: &Map<String, Value>, allowed: &[&str]) -> Result<(), ApiError> {
    for k in map.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(ApiError::invalid(format!("unknown argument `{k}`")));
        }
    }
    Ok(())
}

fn require_non_empty_str(map: &Map<String, Value>, key: &str) -> Result<String, ApiError> {
    optional_trimmed_str(map, key)?.ok_or_else(|| ApiError::invalid(format!("{key} is required")))
}

fn require_id(map: &Map<String, Value>) -> Result<String, ApiError> {
    let raw = optional_trimmed_str(map, "id")?
        .or(optional_trimmed_str(map, "sourceId")?)
        .ok_or_else(|| ApiError::invalid("source id (id or sourceId) is required"))?;
    validate_source_id(&raw)
}

fn optional_trimmed_str(map: &Map<String, Value>, key: &str) -> Result<Option<String>, ApiError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim().to_string()).filter(|t| !t.is_empty())),
        Some(_) => Err(ApiError::invalid(format!("{key} must be a string"))),
    }
}

fn optional_bool(map: &Map<String, Value>, key: &str) -> Result<Option<bool>, ApiError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(ApiError::invalid(format!("{key} must be a boolean"))),
    }
}

fn optional_bound(
    map: &Map<String, Value>,
    key: &str,
    min: usize,
    max: usize,
) -> Result<Option<usize>, ApiError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let n = v.as_u64().ok_or_else(|| {
                ApiError::invalid(format!("{key} must be a non-negative integer"))
            })?;
            let val = usize::try_from(n).unwrap_or(usize::MAX);
            if val < min || val > max {
                return Err(ApiError::invalid(format!("{key} must be in {min}..={max}")));
            }
            Ok(Some(val))
        }
    }
}

fn parse_as_of(value: Option<&Value>) -> Result<Option<u64>, ApiError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n.as_u64().map(Some).ok_or_else(|| {
            ApiError::invalid("asOf must be a non-negative integer or RFC-3339 string")
        }),
        Some(Value::String(raw)) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if let Ok(ms) = trimmed.parse::<u64>() {
                return Ok(Some(ms));
            }
            pagesift_graph::parse_timestamp_ms(trimmed)
                .map(Some)
                .ok_or_else(|| ApiError::invalid(format!("invalid asOf timestamp {trimmed:?}")))
        }
        Some(_) => Err(ApiError::invalid(
            "asOf must be a Unix millisecond integer or RFC-3339 timestamp string",
        )),
    }
}

#[cfg(test)]
#[path = "loop_ops_tests.rs"]
mod tests;
