//! Firecrawl v1 and v2 `/map` endpoint.

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;

use crate::error::ApiError;
use crate::server::{AppState, lenient_body};
use crate::types::{MapRequest, MapResponse};
use crate::validate::{MAX_MAP_LIMIT, check_url};

pub async fn map(
    State(s): State<AppState>,
    req: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<MapResponse>, ApiError> {
    use crate::fields;
    let known = [fields::names::<MapRequest>(), fields::CLIENT_IDENTITY];
    let (req, ignored): (MapRequest, _) = lenient_body(req, &known)?;
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
        warnings: fields::ignored_warning(&ignored).into_iter().collect(),
    }))
}
