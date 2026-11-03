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
    run(s, caller, req, false).await
}

/// `/v2/scrape`：引擎支持 `/interact` 时结果带 `metadata.scrapeId`（v1 没有这个接口，响应不变）。
pub async fn scrape_v2(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    req: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<ScrapeResponse>, ApiError> {
    run(s, caller, req, true).await
}

async fn run(
    s: AppState,
    caller: Option<Extension<Caller>>,
    req: Result<Json<serde_json::Value>, JsonRejection>,
    v2: bool,
) -> Result<Json<ScrapeResponse>, ApiError> {
    let (mut req, ignored): (ScrapeRequest, _) =
        lenient_body(req, &crate::fields::scrape_request())?;
    check_url(&req.url)?;
    // `keepSession` 只有这里认（校验拒收其余入口的）
    let keep = v2 && std::mem::take(&mut req.options.keep_session);
    check_scrape_options(&req.options, s.engine.as_ref())?;
    if let Some(Extension(c)) = &caller {
        s.limiter.check_quota(c)?;
    }
    let result = if v2 && (keep || s.engine.supports_interact()) {
        let owner = caller.as_ref().map(|Extension(c)| c.account.as_ref());
        s.engine
            .scrape_session(&req.url, &req.options, keep, owner)
            .await
    } else {
        s.engine.scrape(&req.url, &req.options).await
    };
    match result {
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
