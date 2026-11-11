//! Firecrawl v2 的 `POST` / `DELETE /v2/scrape/{id}/interact`。

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};

use crate::error::ApiError;
use crate::limits::Caller;
use crate::server::{AppState, lenient_body};
use crate::types::{InteractRequest, InteractResponse, InteractStopped};

fn owner(caller: &Option<Extension<Caller>>) -> Option<&str> {
    caller.as_ref().map(|Extension(c)| c.account.as_ref())
}

pub async fn interact(
    State(s): State<AppState>,
    Path(id): Path<String>,
    caller: Option<Extension<Caller>>,
    req: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<InteractResponse>, ApiError> {
    let known = [
        crate::fields::names::<InteractRequest>(),
        crate::fields::CLIENT_IDENTITY,
    ];
    let (req, ignored): (InteractRequest, _) = lenient_body(req, &known)?;
    req.check()?;
    if let Some(u) = &req.url {
        crate::validate::check_url(u)?;
    }
    let page_hold = match &caller {
        Some(Extension(c)) => Some(s.limiter.reserve_page(c).await?),
        None => None,
    };
    let mut resp = s.engine.interact(&id, &req, owner(&caller)).await?;
    if let Some(w) = crate::fields::ignored_warning(&ignored) {
        resp.warnings.push(w);
    }
    // 动作之后取回了页面：和 scrape 一样算一页；没取回页面就退还预留
    if resp.data.is_some()
        && let Some(hold) = page_hold
    {
        hold.commit().await;
    }
    Ok(Json(resp))
}

pub async fn stop(
    State(s): State<AppState>,
    Path(id): Path<String>,
    caller: Option<Extension<Caller>>,
) -> Result<Json<InteractStopped>, ApiError> {
    Ok(Json(s.engine.stop_interact(&id, owner(&caller)).await?))
}
