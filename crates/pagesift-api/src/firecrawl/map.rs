//! Firecrawl v1 and v2 `/map` endpoint.

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;

use crate::error::ApiError;
use crate::server::{AppState, body};
use crate::types::{MapRequest, MapResponse};
use crate::validate::{MAX_MAP_LIMIT, check_url};

pub async fn map(
    State(s): State<AppState>,
    req: Result<Json<MapRequest>, JsonRejection>,
) -> Result<Json<MapResponse>, ApiError> {
    let req = body(req)?;
    check_url(&req.url)?;
    if req.limit > MAX_MAP_LIMIT {
        return Err(ApiError::invalid(format!(
            "limit must not exceed {MAX_MAP_LIMIT}"
        )));
    }
    let links = s.engine.map(&req).await?;
    Ok(Json(MapResponse {
        success: true,
        links,
    }))
}
