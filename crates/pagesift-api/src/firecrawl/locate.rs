//! pagesift 扩展的 `POST /v2/observe`、`POST /v2/similar`：都要抓一页，配额按一页算。

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, State};

use crate::error::ApiError;
use crate::limits::Caller;
use crate::server::{AppState, lenient_body};
use crate::types::{
    Format, ObserveRequest, ObserveResponse, ScrapeOptions, SimilarRequest, SimilarResponse,
};

pub async fn observe(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    req: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<ObserveResponse>, ApiError> {
    let known = [
        crate::fields::names::<ObserveRequest>(),
        crate::fields::CLIENT_IDENTITY,
    ];
    let (req, ignored): (ObserveRequest, _) = lenient_body(req, &known)?;
    if req.instruction.trim().is_empty() {
        return Err(ApiError::invalid("`instruction` must not be empty"));
    }
    if req.instruction.chars().count() > crate::types::interact::MAX_PROMPT_CHARS {
        return Err(ApiError::invalid(format!(
            "`instruction` must be at most {} characters",
            crate::types::interact::MAX_PROMPT_CHARS
        )));
    }
    crate::validate::check_url(&req.url)?;
    if let Some(Extension(c)) = &caller {
        s.limiter.check_quota(c)?;
    }
    let (data, mut warnings) = s.engine.observe(&req.url, &req.instruction).await?;
    warnings.extend(crate::fields::ignored_warning(&ignored));
    if let Some(Extension(c)) = &caller {
        s.limiter.consume_pages(c, 1).await;
    }
    Ok(Json(ObserveResponse {
        success: true,
        data,
        warnings,
    }))
}

pub async fn similar(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    req: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<SimilarResponse>, ApiError> {
    let known = [
        crate::fields::names::<SimilarRequest>(),
        crate::fields::CLIENT_IDENTITY,
    ];
    let (req, ignored): (SimilarRequest, _) = lenient_body(req, &known)?;
    crate::validate::check_url(&req.url)?;
    let opts = ScrapeOptions {
        formats: vec![Format::new("rawHtml")],
        engine: req.engine.clone(),
        ..Default::default()
    };
    crate::check_scrape_options(&opts, s.engine.as_ref())?;
    if let Some(Extension(c)) = &caller {
        s.limiter.check_quota(c)?;
    }
    let data = s
        .engine
        .find_similar(&req.url, &req.selector, &opts)
        .await?;
    if let Some(Extension(c)) = &caller {
        s.limiter.consume_pages(c, 1).await;
    }
    Ok(Json(SimilarResponse {
        success: true,
        data,
        warnings: crate::fields::ignored_warning(&ignored)
            .into_iter()
            .collect(),
    }))
}
