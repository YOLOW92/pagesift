//! Firecrawl v1 and v2 `/scrape` endpoint handler.

use std::sync::atomic::Ordering;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, State};
use axum::Json;

use crate::error::ApiError;
use crate::limits::Caller;
use crate::metrics::PageSource;
use crate::server::{AppState, body};
use crate::types::{ScrapeRequest, ScrapeResponse};
use crate::validate::{check_scrape_options, check_url};

pub async fn scrape(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    req: Result<Json<ScrapeRequest>, JsonRejection>,
) -> Result<Json<ScrapeResponse>, ApiError> {
    let req = body(req)?;
    check_url(&req.url)?;
    check_scrape_options(&req.options, s.engine.as_ref())?;
    if let Some(Extension(c)) = &caller {
        s.limiter.check_quota(c)?;
    }
    match s.engine.scrape(&req.url, &req.options).await {
        Ok(data) => {
            s.metrics.scrape_ok.fetch_add(1, Ordering::Relaxed);
            s.metrics.page(PageSource::Scrape);
            if let Some(Extension(c)) = &caller {
                s.limiter.consume_pages(c, 1).await;
            }
            Ok(Json(ScrapeResponse {
                success: true,
                warning: data.warning.clone(),
                data,
            }))
        }
        Err(e) => {
            s.metrics.scrape_err.fetch_add(1, Ordering::Relaxed);
            Err(e)
        }
    }
}
