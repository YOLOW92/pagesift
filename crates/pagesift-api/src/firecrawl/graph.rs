//! Firecrawl v2 DataGraph HTTP query endpoints.

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use serde::Deserialize;

use crate::error::{ApiError, ErrorCode};
use crate::limits::{Caller, caller_tenant};
use crate::server::AppState;
use crate::types::{CrawlGraphResponse, GraphConflictsResponse, GraphHistoryResponse};

use super::batch::authorize_job;

#[derive(Deserialize)]
pub struct HistoryQuery {
    pub url: Option<String>,
    pub key: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
pub struct ConflictsQuery {
    pub subject: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

/// `GET /v2/crawl/{id}/graph`: return job-level DataGraph summary, stats, clusters, and conflicts.
pub async fn crawl_graph(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    Path(id): Path<String>,
) -> Result<Json<CrawlGraphResponse>, ApiError> {
    authorize_job(&s, &id, caller.as_ref().map(|Extension(c)| c)).await?;
    let (state, summary) = s
        .jobs
        .crawl_graph(&id)
        .await?
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("job {id} not found")))?;
    Ok(Json(CrawlGraphResponse {
        success: true,
        id,
        status: state.as_str().into(),
        summary,
    }))
}

/// `GET /v2/graph/history?url=...`: return bounded tenant-isolated historical versions and diff edges.
pub async fn graph_history(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<GraphHistoryResponse>, ApiError> {
    let target = q
        .url
        .as_deref()
        .or(q.key.as_deref())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ApiError::invalid("url query parameter is required"))?
        .to_string();
    let max_limit = pagesift_store::DEFAULT_GRAPH_HISTORY_LIMIT;
    let limit = match q.limit {
        Some(0) => return Err(ApiError::invalid("limit must be at least 1")),
        Some(n) if n > max_limit => {
            return Err(ApiError::invalid(format!(
                "limit must not exceed {max_limit}"
            )));
        }
        Some(n) => n,
        None => max_limit,
    };
    let tenant = caller_tenant(caller.as_ref().map(|Extension(c)| c));
    let res = s
        .jobs
        .graph_history_for(Some(&tenant), &target, limit)
        .await?;
    Ok(Json(GraphHistoryResponse {
        success: true,
        url: target,
        resource_key: res.resource_key,
        versions: res.versions,
        edges: res.edges,
        truncated: res.truncated,
    }))
}

/// `GET /v2/graph/conflicts?subject=...`: list tenant-isolated cross-source conflicts with pagination.
pub async fn graph_conflicts(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    Query(q): Query<ConflictsQuery>,
) -> Result<Json<GraphConflictsResponse>, ApiError> {
    let subject = q
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let max_limit = pagesift_store::DEFAULT_GRAPH_CONFLICTS_LIMIT;
    let limit = match q.limit {
        Some(0) => return Err(ApiError::invalid("limit must be at least 1")),
        Some(n) if n > max_limit => {
            return Err(ApiError::invalid(format!(
                "limit must not exceed {max_limit}"
            )));
        }
        Some(n) => n,
        None => max_limit,
    };
    let offset = q.offset.unwrap_or(0);
    let tenant = caller_tenant(caller.as_ref().map(|Extension(c)| c));
    let (total, conflicts, truncated) = s
        .jobs
        .graph_conflicts_for(Some(&tenant), subject.as_deref(), limit, offset)
        .await?;
    Ok(Json(GraphConflictsResponse {
        success: true,
        subject,
        total,
        conflicts,
        truncated,
    }))
}
