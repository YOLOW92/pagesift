//! Firecrawl v1 and v2 `/search` and `/extract` endpoints.

use crate::limits::Caller;
use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};

use crate::error::{ApiError, ErrorCode};
use crate::fields;
use crate::jobs::{JOB_TTL, rfc3339};
use crate::server::{AppState, ExtractJob, lenient_body};
use crate::types::{ExtractRequest, SearchRequest};
use crate::validate::{check_scrape_options, check_url, check_url_count};
use std::sync::Arc;

struct AccountBudget {
    limiter: Arc<crate::limits::Limiter>,
    caller: Caller,
}

impl crate::PageBudget for AccountBudget {
    fn acquire(&self) -> crate::BoxFuture<'_, Result<(), ApiError>> {
        Box::pin(async move {
            let hold = self.limiter.reserve_page(&self.caller).await?;
            hold.commit().await;
            Ok(())
        })
    }
}

pub async fn search(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    req: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let known = [fields::names::<SearchRequest>(), fields::CLIENT_IDENTITY];
    let (mut req, ignored): (SearchRequest, _) = lenient_body(req, &known)?;
    if req.query.trim().is_empty() {
        return Err(ApiError::invalid("query must not be empty"));
    }
    if !(1..=100).contains(&req.limit) {
        return Err(ApiError::invalid("limit must be between 1 and 100"));
    }
    if let Some(o) = &mut req.scrape_options {
        o.owner = caller
            .as_ref()
            .map(|Extension(c)| c.account.to_string())
            .filter(|s| s != "anonymous");
        check_scrape_options(o, s.engine.as_ref())?;
    }
    let data = if req
        .scrape_options
        .as_ref()
        .is_some_and(|o| !o.formats.is_empty())
        && let Some(Extension(c)) = &caller
        && c.limits.pages_per_day.is_some()
    {
        s.limiter.check_quota(c)?;
        s.engine
            .search_budgeted(
                &req,
                Arc::new(AccountBudget {
                    limiter: s.limiter.clone(),
                    caller: c.clone(),
                }),
            )
            .await?
    } else {
        s.engine.search(&req).await?
    };
    let credits = data["web"].as_array().map_or(0, Vec::len);
    let mut out = serde_json::json!({
        "success": true,
        "data": data,
        "creditsUsed": credits,
        "id": uuid::Uuid::new_v4().to_string(),
    });
    if let Some(w) = fields::ignored_warning(&ignored) {
        out["warnings"] = serde_json::json!([w]);
    }
    Ok(Json(out))
}

pub async fn extract_start(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    req: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if let Some(Extension(c)) = &caller {
        s.limiter.check_quota(c)?;
    }
    let known = [fields::names::<ExtractRequest>(), fields::CLIENT_IDENTITY];
    let (mut req, ignored): (ExtractRequest, _) = lenient_body(req, &known)?;
    let opts = req.scrape_options.get_or_insert_with(Default::default);
    opts.owner = caller
        .as_ref()
        .map(|Extension(c)| c.account.to_string())
        .filter(|s| s != "anonymous");
    check_scrape_options(opts, s.engine.as_ref())?;
    if req.urls.is_empty() {
        return Err(ApiError::invalid("urls must not be empty"));
    }
    check_url_count(req.urls.len())?;
    if req.prompt.is_none() && req.schema.is_none() {
        return Err(ApiError::invalid("extract needs a prompt or a schema"));
    }
    if req.schema.as_ref().is_some_and(|v| !v.is_object()) {
        return Err(ApiError::invalid("schema must be a JSON object"));
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
                owner: caller.as_ref().map_or_else(
                    || "anonymous".to_string(),
                    |Extension(c)| c.account.to_string(),
                ),
                data: None,
                error: None,
                created: now,
            },
        );
    }
    let (engine, extracts, job_id) = (s.engine.clone(), s.extracts.clone(), id.clone());
    let budget = caller.and_then(|Extension(c)| {
        c.limits.pages_per_day.map(|_| {
            Arc::new(AccountBudget {
                limiter: s.limiter.clone(),
                caller: c,
            }) as Arc<dyn crate::PageBudget>
        })
    });
    tokio::spawn(async move {
        let result = match budget {
            Some(budget) => engine.extract_budgeted(&req, budget).await,
            None => engine.extract(&req).await,
        };
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
    let mut out = serde_json::json!({ "success": true, "id": id });
    if let Some(w) = fields::ignored_warning(&ignored) {
        out["warnings"] = serde_json::json!([w]);
    }
    Ok(Json(out))
}

pub async fn extract_status(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut map = s.extracts.lock().await;
    map.retain(|_, job| job.created + JOB_TTL > std::time::SystemTime::now());
    let job = map
        .get(&id)
        .filter(|j| {
            caller
                .as_ref()
                .map_or("anonymous", |Extension(c)| c.account.as_ref())
                == j.owner
        })
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
