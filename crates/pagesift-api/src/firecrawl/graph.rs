//! Firecrawl v2 DataGraph HTTP query endpoints.

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::HeaderMap;
use serde::Deserialize;

use crate::error::{ApiError, ErrorCode};
use crate::limits::{Caller, caller_graph_tenant};
use crate::server::AppState;
use crate::types::{
    CrawlGraphResponse, GraphClaimsResponse, GraphConflictsResponse, GraphHistoryResponse,
};

use super::batch::authorize_job;

pub(crate) fn parse_as_of_ms(raw: Option<&str>) -> Result<Option<u64>, ApiError> {
    let Some(trimmed) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if let Ok(ms) = trimmed.parse::<u64>() {
        return Ok(Some(ms));
    }
    pagesift_graph::parse_timestamp_ms(trimmed)
        .map(Some)
        .ok_or_else(|| {
            ApiError::invalid(format!(
                "invalid asOf timestamp {trimmed:?}: expected Unix ms or RFC-3339 date"
            ))
        })
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    pub url: Option<String>,
    pub key: Option<String>,
    #[serde(rename = "asOf", alias = "as_of", default)]
    pub as_of: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
pub struct ConflictsQuery {
    pub subject: Option<String>,
    #[serde(rename = "asOf", alias = "as_of", default)]
    pub as_of: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

#[derive(Deserialize)]
pub struct ClaimsQuery {
    pub url: Option<String>,
    pub key: Option<String>,
    pub subject: Option<String>,
    #[serde(rename = "asOf", alias = "as_of", default)]
    pub as_of: Option<String>,
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
    headers: HeaderMap,
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
    let as_of_ms = parse_as_of_ms(q.as_of.as_deref())?;
    let tenant = caller_graph_tenant(caller.as_ref().map(|Extension(c)| c), &headers);
    let res = s
        .jobs
        .graph_history_as_of_for(Some(&tenant), &target, as_of_ms, limit)
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
    headers: HeaderMap,
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
    let as_of_ms = parse_as_of_ms(q.as_of.as_deref())?;
    let tenant = caller_graph_tenant(caller.as_ref().map(|Extension(c)| c), &headers);
    let (total, conflicts, truncated) = s
        .jobs
        .graph_conflicts_as_of_for(Some(&tenant), subject.as_deref(), as_of_ms, limit, offset)
        .await?;
    Ok(Json(GraphConflictsResponse {
        success: true,
        subject,
        total,
        conflicts,
        truncated,
    }))
}

/// `GET /v2/graph/claims?url=...&subject=...&asOf=...`: list tenant-isolated claim lifecycle records.
pub async fn graph_claims(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Query(q): Query<ClaimsQuery>,
) -> Result<Json<GraphClaimsResponse>, ApiError> {
    let target = q
        .url
        .as_deref()
        .or(q.key.as_deref())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
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
    let as_of_ms = parse_as_of_ms(q.as_of.as_deref())?;
    let tenant = caller_graph_tenant(caller.as_ref().map(|Extension(c)| c), &headers);
    let (total, claims, truncated) = s
        .jobs
        .graph_claims_for(
            Some(&tenant),
            target.as_deref(),
            subject.as_deref(),
            as_of_ms,
            limit,
            offset,
        )
        .await?;
    Ok(Json(GraphClaimsResponse {
        success: true,
        subject,
        url: target,
        as_of_ms,
        total,
        claims,
        truncated,
    }))
}
