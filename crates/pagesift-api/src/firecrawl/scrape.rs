//! Firecrawl v1 and v2 `/scrape` endpoint handler.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, State};
use std::sync::atomic::Ordering;

use crate::error::ApiError;
use crate::limits::Caller;
use crate::metrics::PageSource;
use crate::server::{AppState, lenient_body};
use crate::types::{ScrapeRequest, ScrapeResponse};
use crate::validate::{check_scrape_options, check_url};

pub async fn scrape(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    req: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<ScrapeResponse>, ApiError> {
    let (req, ignored): (ScrapeRequest, _) = lenient_body(req, &crate::fields::scrape_request())?;
    check_url(&req.url)?;
    check_scrape_options(&req.options, s.engine.as_ref())?;
    if let Some(Extension(c)) = &caller {
        s.limiter.check_quota(c)?;
    }
    match s.engine.scrape(&req.url, &req.options).await {
        Ok(mut data) => {
            if let Some(w) = crate::fields::ignored_warning(&ignored) {
                data.metadata
                    .pagesift
                    .get_or_insert_with(Default::default)
                    .warnings
                    .push(w);
            }
            crate::attachments::keep_in(&data, s.jobs.disk().as_ref());
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
