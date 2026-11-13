//! Firecrawl v2 DataGraph HTTP query endpoints.

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use serde::Deserialize;

use crate::error::{ApiError, ErrorCode};
use crate::limits::Caller;
use crate::server::AppState;
use crate::types::{CrawlGraphResponse, GraphConflictsResponse, GraphHistoryResponse};

use super::batch::authorize_job;

#[derive(Deserialize)]
pub struct HistoryQuery {
    pub url: Option<String>,
    pub key: Option<String>,
}

#[derive(Deserialize)]
pub struct ConflictsQuery {
    pub subject: Option<String>,
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
        .await
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("job {id} not found")))?;
    Ok(Json(CrawlGraphResponse {
        success: true,
        id,
        status: state.as_str().into(),
        summary,
    }))
}

/// `GET /v2/graph/history?url=...`: return historical versions and version diff edges for a URL/key.
pub async fn graph_history(
    State(s): State<AppState>,
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
    let (resource_key, versions, edges) = s.jobs.graph_history(&target).await?;
    Ok(Json(GraphHistoryResponse {
        success: true,
        url: target,
        resource_key,
        versions,
        edges,
    }))
}

/// `GET /v2/graph/conflicts?subject=...`: list cross-source conflicts detected by DataGraph.
pub async fn graph_conflicts(
    State(s): State<AppState>,
    Query(q): Query<ConflictsQuery>,
) -> Result<Json<GraphConflictsResponse>, ApiError> {
    let subject = q
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let conflicts = s.jobs.graph_conflicts(subject.as_deref()).await?;
    Ok(Json(GraphConflictsResponse {
        success: true,
        subject,
        total: conflicts.len(),
        conflicts,
    }))
}
