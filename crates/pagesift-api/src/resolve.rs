//! 1.7 Resolve HTTP routes (`/v2/resolve` and sub-endpoints) with tenant isolation,
//! fixed-route operation conflict validation, read-only GET `/learn`, and daily page quota enforcement.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Extension, Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use crate::error::ApiError;
use crate::limits::{Caller, QuotaBudget, caller_graph_tenant};
use crate::server::AppState;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/v2/resolve", post(handle_generic))
        .route("/v2/resolve/check", post(handle_check))
        .route(
            "/v2/resolve/conclusions",
            get(handle_conclusions_get).post(handle_conclusions_post),
        )
        .route(
            "/v2/resolve/conclusions/{id}",
            get(handle_conclusion_get).delete(handle_conclusion_delete),
        )
        .route(
            "/v2/resolve/conclusions/{id}/history",
            get(handle_conclusion_history_get),
        )
        .route(
            "/v2/resolve/graph",
            get(handle_graph_get).post(handle_graph_post),
        )
        .route("/v2/resolve/reconcile", post(handle_reconcile))
        .route("/v2/resolve/revalidate", post(handle_revalidate))
        .route(
            "/v2/resolve/events",
            get(handle_events_get).post(handle_events_post),
        )
        .route(
            "/v2/resolve/learn",
            get(handle_learn_get).post(handle_learn_post),
        )
}

fn fixed_route_accepts_op(fixed_op: &str, supplied: &str) -> bool {
    match fixed_op {
        "check" => matches!(supplied, "check" | "verify"),
        "conclusions" => matches!(supplied, "conclusions" | "list"),
        "get" => supplied == "get",
        "history" => supplied == "history",
        "graph" => supplied == "graph",
        "reconcile" => supplied == "reconcile",
        "revalidate" => supplied == "revalidate",
        "events" => supplied == "events",
        "learn" | "learn_status" => matches!(supplied, "learn" | "learn_status" | "learnstatus"),
        "delete" => supplied == "delete",
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
                        "conflicting 'action' ('{a}') and 'op' ('{o}') in resolve request"
                    )));
                }
                if let Some(sup) = supplied_action.or(supplied_op)
                    && !fixed_route_accepts_op(op, &sup)
                {
                    return Err(ApiError::invalid(format!(
                        "fixed resolve route '{op}' rejects conflicting operation '{sup}'"
                    )));
                }
                m.remove("op");
                m.insert("action".to_owned(), Value::String(op.to_owned()));
            }
            Value::Null => {
                let mut m = Map::new();
                m.insert("action".to_owned(), Value::String(op.to_owned()));
                body = Value::Object(m);
            }
            _ => {
                return Err(ApiError::invalid(
                    "resolve request body must be a JSON object",
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
            .resolve_budgeted(Some(&tenant), body, budget)
            .await?
    } else {
        s.engine.resolve(Some(&tenant), body).await?
    };
    Ok(Json(out))
}

fn is_int_query_param(key: &str) -> bool {
    matches!(
        key,
        "limit"
            | "version"
            | "afterSeq"
            | "after_seq"
            | "sinceEventSeq"
            | "since_event_seq"
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
            | "reset"
            | "persist"
            | "includeUnverified"
            | "include_unverified"
            | "learnEnabled"
            | "learn_enabled"
            | "allowNetworkAcquisition"
            | "allow_network_acquisition"
            | "allowExternalSearch"
            | "allow_external_search"
    )
}

fn query_map_to_value(params: HashMap<String, String>) -> Value {
    let mut m = Map::with_capacity(params.len());
    for (k, v) in params {
        if is_int_query_param(&k) {
            if let Ok(n) = v.trim().parse::<u64>() {
                m.insert(k, json!(n));
            } else {
                m.insert(k, Value::String(v));
            }
        } else if is_bool_query_param(&k) {
            if let Ok(b) = v.trim().parse::<bool>() {
                m.insert(k, json!(b));
            } else {
                m.insert(k, Value::String(v));
            }
        } else {
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

async fn handle_check(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(s, caller, headers, body, Some("check")).await
}

async fn handle_conclusions_get(
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
        Some("conclusions"),
    )
    .await
}

async fn handle_conclusions_post(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(s, caller, headers, body, Some("conclusions")).await
}

async fn handle_conclusion_get(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let mut val = query_map_to_value(params);
    if let Value::Object(m) = &mut val {
        m.insert("conclusionId".to_owned(), Value::String(id));
    }
    run_with_op(s, caller, headers, val, Some("get")).await
}

async fn handle_conclusion_delete(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let mut val = query_map_to_value(params);
    if let Value::Object(m) = &mut val {
        m.insert("conclusionId".to_owned(), Value::String(id));
    }
    run_with_op(s, caller, headers, val, Some("delete")).await
}

async fn handle_conclusion_history_get(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let mut val = query_map_to_value(params);
    if let Value::Object(m) = &mut val {
        m.insert("conclusionId".to_owned(), Value::String(id));
    }
    run_with_op(s, caller, headers, val, Some("history")).await
}

async fn handle_graph_get(
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
        Some("graph"),
    )
    .await
}

async fn handle_graph_post(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(s, caller, headers, body, Some("graph")).await
}

async fn handle_reconcile(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(s, caller, headers, body, Some("reconcile")).await
}

async fn handle_revalidate(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(s, caller, headers, body, Some("revalidate")).await
}

async fn handle_events_get(
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
        Some("events"),
    )
    .await
}

async fn handle_events_post(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(s, caller, headers, body, Some("events")).await
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
            "enabled" | "frozen" | "reset" | "feedback" | "verifierversion" | "verifier_version"
        ) {
            return Err(ApiError::invalid(format!(
                "GET /v2/resolve/learn is read-only and rejects mutating parameter '{k}'; use POST /v2/resolve/learn to update learn state"
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

async fn handle_learn_post(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    run_with_op(s, caller, headers, body, Some("learn")).await
}
