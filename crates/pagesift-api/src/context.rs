//! 1.6 Context HTTP routes (`/v2/context` and sub-endpoints) with tenant isolation.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Extension, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use crate::error::ApiError;
use crate::limits::{Caller, QuotaBudget, caller_graph_tenant};
use crate::server::AppState;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/v2/context", post(handle_generic))
        .route("/v2/context/ingest", post(handle_ingest))
        .route("/v2/context/sync", post(handle_sync))
        .route("/v2/context/search", post(handle_search))
        .route("/v2/context/pack", post(handle_pack))
        .route("/v2/context/action", post(handle_action))
        .route(
            "/v2/context/units",
            get(handle_units_get).post(handle_units),
        )
        .route(
            "/v2/context/deltas",
            get(handle_deltas_get).post(handle_deltas_poll),
        )
        .route("/v2/context/deltas/poll", post(handle_deltas_poll))
        .route("/v2/context/deltas/ack", post(handle_deltas_ack))
        .route(
            "/v2/context/learn",
            get(handle_learn_get).post(handle_learn),
        )
}

fn fixed_route_accepts_op(fixed_op: &str, supplied: &str) -> bool {
    match fixed_op {
        "ingest" => supplied == "ingest",
        "sync" => supplied == "sync",
        "search" => supplied == "search",
        "pack" => supplied == "pack",
        "action" => matches!(
            supplied,
            "action" | "research" | "verify" | "watch" | "changes"
        ),
        "units" => supplied == "units",
        "deltas" => matches!(supplied, "deltas" | "deltas_poll" | "deltaspoll"),
        "ack" => matches!(supplied, "ack" | "deltas_ack" | "deltasack"),
        "learn" => matches!(supplied, "learn" | "learn_status" | "learnstatus"),
        "learn_status" => matches!(supplied, "learn" | "learn_status" | "learnstatus"),
        _ => supplied == fixed_op,
    }
}

fn extract_opt_op(map: &Map<String, Value>, key: &str) -> Result<Option<String>, ApiError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let norm = s.trim().to_ascii_lowercase();
            if norm.is_empty() {
                return Err(ApiError::invalid(format!("'{key}' must not be empty")));
            }
            Ok(Some(norm))
        }
        Some(_) => Err(ApiError::invalid(format!("'{key}' must be a string"))),
    }
}

async fn run_with_op(
    s: AppState,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    mut body: Value,
    fixed_op: Option<&'static str>,
) -> Result<Json<Value>, ApiError> {
    if let Some(op) = fixed_op {
        match &mut body {
            Value::Object(m) => {
                let supplied_action = extract_opt_op(m, "action")?;
                let supplied_op = extract_opt_op(m, "op")?;
                if let (Some(a), Some(o)) = (&supplied_action, &supplied_op)
                    && a != o
                {
                    return Err(ApiError::invalid(format!(
                        "conflicting 'action' ('{a}') and 'op' ('{o}') in context request"
                    )));
                }
                let effective = match supplied_action.or(supplied_op) {
                    Some(sup) => {
                        if !fixed_route_accepts_op(op, &sup) {
                            return Err(ApiError::invalid(format!(
                                "fixed context route '{op}' rejects conflicting operation '{sup}'"
                            )));
                        }
                        if op == "action" { sup } else { op.to_owned() }
                    }
                    None => op.to_owned(),
                };
                m.remove("op");
                m.insert("action".to_owned(), Value::String(effective));
            }
            Value::Null => {
                let mut m = Map::new();
                m.insert("action".to_owned(), Value::String(op.to_owned()));
                body = Value::Object(m);
            }
            _ => {
                return Err(ApiError::invalid(
                    "context request body must be a JSON object",
                ));
            }
        }
    }
    let caller_ref = caller.as_ref().map(|Extension(c)| c);
    let tenant = caller_graph_tenant(caller_ref, &headers);
    let out = if let Some(c) = caller_ref
        && c.limits.pages_per_day.is_some()
    {
        let budget = Arc::new(QuotaBudget::new(s.limiter.clone(), c.clone()));
        s.engine
            .context_budgeted(Some(&tenant), body, budget)
            .await?
    } else {
        s.engine.context(Some(&tenant), body).await?
    };
    Ok(Json(out))
}

fn is_int_query_param(key: &str) -> bool {
    matches!(
        key,
        "limit"
            | "ackedDeltaSeq"
            | "acked_delta_seq"
            | "sinceDeltaSeq"
            | "since_delta_seq"
            | "budgetTokens"
            | "budget_tokens"
            | "maxTokens"
            | "max_tokens"
            | "maxStalenessMs"
            | "max_staleness_ms"
            | "minIndependentSources"
            | "min_independent_sources"
            | "maxAcquisitionRequests"
            | "max_acquisition_requests"
    )
}

fn is_bool_query_param(key: &str) -> bool {
    matches!(
        key,
        "enabled"
            | "frozen"
            | "persist"
            | "includeUnverified"
            | "include_unverified"
            | "includeConflicts"
            | "include_conflicts"
            | "enableSemanticSketch"
            | "enable_semantic_sketch"
            | "learnEnabled"
            | "learn_enabled"
            | "allowNetworkAcquisition"
            | "allow_network_acquisition"
            | "allowExternalSearch"
            | "allow_external_search"
            | "strictConstrainToUrls"
            | "strict_constrain_to_urls"
    )
}

fn query_map_to_value(params: HashMap<String, String>) -> Value {
    let mut m = Map::with_capacity(params.len());
    for (k, v) in params {
        if is_int_query_param(&k) {
            if let Ok(n) = v.trim().parse::<u64>() {
                m.insert(k, json!(n));
            } else {
                // Keep raw string so downstream validator rejects malformed integer with 400 INVALID_REQUEST
                m.insert(k, Value::String(v));
            }
        } else if is_bool_query_param(&k) {
            if let Ok(b) = v.trim().parse::<bool>() {
                m.insert(k, json!(b));
            } else {
                m.insert(k, Value::String(v));
            }
        } else {
            // Identifiers (`sourceId`, `consumerId`, `instanceId`), URLs, queries, and timestamps stay strings!
            m.insert(k, Value::String(v));
        }
    }
    Value::Object(m)
}

async fn handle_generic(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(s, caller, headers, body, None).await
}

async fn handle_ingest(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(s, caller, headers, body, Some("ingest")).await
}

async fn handle_sync(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(s, caller, headers, body, Some("sync")).await
}

async fn handle_search(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(s, caller, headers, body, Some("search")).await
}

async fn handle_pack(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(s, caller, headers, body, Some("pack")).await
}

async fn handle_action(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(s, caller, headers, body, Some("action")).await
}

async fn handle_units_get(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(
        s,
        caller,
        headers,
        query_map_to_value(params),
        Some("units"),
    )
    .await
}

async fn handle_units(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(s, caller, headers, body, Some("units")).await
}

async fn handle_deltas_get(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(
        s,
        caller,
        headers,
        query_map_to_value(params),
        Some("deltas"),
    )
    .await
}

async fn handle_deltas_poll(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(s, caller, headers, body, Some("deltas")).await
}

async fn handle_deltas_ack(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(s, caller, headers, body, Some("ack")).await
}

async fn handle_learn_get(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    for k in params.keys() {
        let low = k.to_ascii_lowercase();
        if matches!(
            low.as_str(),
            "enabled" | "frozen" | "driftguard" | "drift_guard" | "feedback"
        ) {
            return Err(ApiError::invalid(format!(
                "GET /v2/context/learn is read-only and rejects mutating parameter '{k}'; use POST /v2/context/learn to update learn state"
            )));
        }
    }
    run_with_op(
        s,
        caller,
        headers,
        query_map_to_value(params),
        Some("learn_status"),
    )
    .await
}

async fn handle_learn(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(s, caller, headers, body, Some("learn")).await
}
