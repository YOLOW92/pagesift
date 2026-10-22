//! Firecrawl v1 and v2 `/map` endpoint.

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::Json;

use crate::error::ApiError;
use crate::server::{AppState, body};
use crate::types::{MapRequest, MapResponse};
use crate::validate::check_url;

pub async fn map(
    State(s): State<AppState>,
    req: Result<Json<MapRequest>, JsonRejection>,
) -> Result<Json<MapResponse>, ApiError> {
    let req = body(req)?;
    check_url(&req.url)?;
    let links = s.engine.map(&req).await?;
    Ok(Json(MapResponse {
        success: true,
        links,
    }))
}
