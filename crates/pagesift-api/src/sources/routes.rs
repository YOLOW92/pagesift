//! 1.5 Loop `/v2/sources` HTTP 路由处理。

use axum::body::Bytes;
use axum::extract::{Extension, Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::error::{ApiError, ErrorCode};
use crate::firecrawl::graph::parse_as_of_ms;
use crate::limits::{Caller, caller_graph_tenant};
use crate::server::AppState;

use super::{
    CreateSourceRequest, DeleteSourceResponse, SourceDeltaResponse, SourceDeltasQuery,
    SourceDeltasResponse, SourceResponse, SourceStateQuery, SourceStateResponse,
    SourcesListResponse, SyncTickResponse, TriggerSyncRequest, build_source_spec,
    validate_source_id,
};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/v2/sources", post(create_source).get(list_sources))
        .route("/v2/sources/{id}", get(get_source).delete(delete_source))
        .route("/v2/sources/{id}/sync", post(trigger_sync))
        .route("/v2/sources/{id}/state", get(get_source_state))
        .route("/v2/sources/{id}/deltas", get(list_source_deltas))
        .route("/v2/sources/{id}/deltas/{tick_id}", get(get_source_delta))
}

fn require_store(state: &AppState) -> Result<pagesift_store::Store, ApiError> {
    state.jobs.store().cloned().ok_or_else(|| {
        ApiError::invalid("data sources require persistent storage configured on server")
    })
}

async fn create_source(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Json(req): Json<CreateSourceRequest>,
) -> Result<Json<SourceResponse>, ApiError> {
    let store = require_store(&s)?;
    let tenant = caller_graph_tenant(caller.as_ref().map(|Extension(c)| c), &headers);
    let spec = build_source_spec(req, None)?;

    let saved = store
        .blocking(move |st| st.sync_for(Some(&tenant)).upsert_source(spec))
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("save source failed: {e}")))?;

    Ok(Json(SourceResponse {
        success: true,
        source: saved,
    }))
}

async fn list_sources(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
) -> Result<Json<SourcesListResponse>, ApiError> {
    let store = require_store(&s)?;
    let tenant = caller_graph_tenant(caller.as_ref().map(|Extension(c)| c), &headers);
    let sources = store
        .blocking(move |st| st.sync_for(Some(&tenant)).list_sources())
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("list sources failed: {e}")))?;

    Ok(Json(SourcesListResponse {
        success: true,
        total: sources.len(),
        sources,
    }))
}

async fn get_source(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<SourceResponse>, ApiError> {
    let id = validate_source_id(&id)?;
    let store = require_store(&s)?;
    let tenant = caller_graph_tenant(caller.as_ref().map(|Extension(c)| c), &headers);
    let lookup_id = id.clone();
    let spec = store
        .blocking(move |st| st.sync_for(Some(&tenant)).get_source(&lookup_id))
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("get source failed: {e}")))?
        .ok_or_else(|| {
            ApiError::new(ErrorCode::NotFound, format!("data source '{id}' not found"))
        })?;

    Ok(Json(SourceResponse {
        success: true,
        source: spec,
    }))
}

async fn delete_source(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<DeleteSourceResponse>, ApiError> {
    let id = validate_source_id(&id)?;
    let store = require_store(&s)?;
    let tenant = caller_graph_tenant(caller.as_ref().map(|Extension(c)| c), &headers);
    let del_id = id.clone();
    let deleted = store
        .blocking(move |st| st.sync_for(Some(&tenant)).delete_source(&del_id))
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("delete source failed: {e}")))?;

    if !deleted {
        return Err(ApiError::new(
            ErrorCode::NotFound,
            format!("data source '{id}' not found"),
        ));
    }
    Ok(Json(DeleteSourceResponse {
        success: true,
        id,
        deleted: true,
    }))
}

async fn trigger_sync(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<SyncTickResponse>, ApiError> {
    let id = validate_source_id(&id)?;
    let _store = require_store(&s)?;
    let req: TriggerSyncRequest = if body.iter().all(|b| b.is_ascii_whitespace()) {
        TriggerSyncRequest::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| ApiError::invalid(format!("invalid sync trigger payload: {e}")))?
    };
    super::validate_max_requests(req.max_requests)?;

    let tenant = caller_graph_tenant(caller.as_ref().map(|Extension(c)| c), &headers);
    let delta = s
        .engine
        .sync_source(&id, Some(&tenant), req.force_all, req.max_requests)
        .await?;

    Ok(Json(SyncTickResponse {
        success: true,
        delta,
    }))
}

async fn get_source_state(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<SourceStateQuery>,
) -> Result<Json<SourceStateResponse>, ApiError> {
    let id = validate_source_id(&id)?;
    let as_of_ms = parse_as_of_ms(q.as_of.as_deref())?;
    let store = require_store(&s)?;
    let tenant = caller_graph_tenant(caller.as_ref().map(|Extension(c)| c), &headers);
    let lookup_id = id.clone();
    let state = store
        .blocking(move |st| {
            st.sync_for(Some(&tenant))
                .build_state_view(&lookup_id, as_of_ms)
        })
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("get source state failed: {e}")))?
        .ok_or_else(|| {
            ApiError::new(ErrorCode::NotFound, format!("data source '{id}' not found"))
        })?;

    Ok(Json(SourceStateResponse {
        success: true,
        state,
    }))
}

async fn list_source_deltas(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<SourceDeltasQuery>,
) -> Result<Json<SourceDeltasResponse>, ApiError> {
    let id = validate_source_id(&id)?;
    let max_limit = pagesift_store::MAX_DELTAS_PER_SOURCE;
    let limit = match q.limit {
        Some(0) => return Err(ApiError::invalid("limit must be at least 1")),
        Some(n) if n > max_limit => {
            return Err(ApiError::invalid(format!(
                "limit must not exceed {max_limit}"
            )));
        }
        Some(n) => n,
        None => 20.min(max_limit),
    };
    let store = require_store(&s)?;
    let tenant = caller_graph_tenant(caller.as_ref().map(|Extension(c)| c), &headers);
    let lookup_id = id.clone();
    let deltas = store
        .blocking(move |st| {
            let sync = st.sync_for(Some(&tenant));
            if sync.get_source(&lookup_id)?.is_none() {
                return Ok::<_, pagesift_store::Error>(None);
            }
            Ok(Some(sync.list_deltas(&lookup_id, limit)?))
        })
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("list deltas failed: {e}")))?
        .ok_or_else(|| {
            ApiError::new(ErrorCode::NotFound, format!("data source '{id}' not found"))
        })?;

    Ok(Json(SourceDeltasResponse {
        success: true,
        source_id: id,
        total: deltas.len(),
        deltas,
    }))
}

async fn get_source_delta(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Path((id, tick_id)): Path<(String, String)>,
) -> Result<Json<SourceDeltaResponse>, ApiError> {
    let id = validate_source_id(&id)?;
    let tick = tick_id.trim().to_string();
    if tick.is_empty() {
        return Err(ApiError::invalid("tick_id must not be empty"));
    }
    let store = require_store(&s)?;
    let tenant = caller_graph_tenant(caller.as_ref().map(|Extension(c)| c), &headers);
    let lookup_id = id.clone();
    let lookup_tick = tick.clone();
    let delta = store
        .blocking(move |st| {
            st.sync_for(Some(&tenant))
                .get_delta(&lookup_id, &lookup_tick)
        })
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("get delta failed: {e}")))?
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::NotFound,
                format!("delta '{tick}' for source '{id}' not found"),
            )
        })?;

    Ok(Json(SourceDeltaResponse {
        success: true,
        delta,
    }))
}
