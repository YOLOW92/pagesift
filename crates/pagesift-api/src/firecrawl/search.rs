//! Firecrawl v1 and v2 `/search` and `/extract` endpoints.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::Json;

use crate::error::{ApiError, ErrorCode};
use crate::jobs::{JOB_TTL, rfc3339};
use crate::server::{AppState, ExtractJob, body};
use crate::types::{ExtractRequest, SearchRequest};
use crate::validate::{check_scrape_options, check_url};

pub async fn search(
    State(s): State<AppState>,
    req: Result<Json<SearchRequest>, JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let req = body(req)?;
    if req.query.trim().is_empty() {
        return Err(ApiError::invalid("query must not be empty"));
    }
    if !(1..=100).contains(&req.limit) {
        return Err(ApiError::invalid("limit must be between 1 and 100"));
    }
    if let Some(o) = &req.scrape_options {
        check_scrape_options(o, s.engine.as_ref())?;
    }
    let data = s.engine.search(&req).await?;
    let credits = data["web"].as_array().map_or(0, Vec::len);
    Ok(Json(serde_json::json!({
        "success": true,
        "data": data,
        "creditsUsed": credits,
        "id": uuid::Uuid::new_v4().to_string(),
    })))
}

pub async fn extract_start(
    State(s): State<AppState>,
    req: Result<Json<ExtractRequest>, JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let req = body(req)?;
    if req.urls.is_empty() {
        return Err(ApiError::invalid("urls must not be empty"));
    }
    if req.prompt.is_none() && req.schema.is_none() {
        return Err(ApiError::invalid("extract needs a prompt or a schema"));
    }
    if req.enable_web_search {
        return Err(ApiError::unsupported(
            "enableWebSearch is not available in this build",
        ));
    }
    for u in &req.urls {
        if let Err(e) = check_url(u.trim_end_matches("/*"))
            && !req.ignore_invalid_urls
        {
            return Err(e);
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    {
        let mut map = s.extracts.lock().await;
        let now = std::time::SystemTime::now();
        map.retain(|_, j| j.created + JOB_TTL > now);
        map.insert(
            id.clone(),
            ExtractJob {
                status: "processing",
                data: None,
                error: None,
                created: now,
            },
        );
    }
    let (engine, extracts, job_id) = (s.engine.clone(), s.extracts.clone(), id.clone());
    tokio::spawn(async move {
        let result = engine.extract(&req).await;
        if let Some(job) = extracts.lock().await.get_mut(&job_id) {
            match result {
                Ok(v) => {
                    job.status = "completed";
                    job.data = Some(v);
                }
                Err(e) => {
                    job.status = "failed";
                    job.error = Some(e);
                }
            }
        }
    });
    Ok(Json(serde_json::json!({ "success": true, "id": id })))
}

pub async fn extract_status(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let map = s.extracts.lock().await;
    let job = map
        .get(&id)
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("extract job {id} not found")))?;
    let mut body = serde_json::json!({
        "success": job.error.is_none(),
        "status": job.status,
        "expiresAt": rfc3339(job.created + JOB_TTL),
    });
    if let Some(d) = &job.data {
        body["data"] = d.clone();
    }
    if let Some(e) = &job.error {
        body["error"] = format!("{}: {}", e.code.as_str(), e.message).into();
    }
    Ok(Json(body))
}
