//! Firecrawl v2 `/agent`：`POST` 提交，`GET /{id}` 查询，`DELETE /{id}` 取消。

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, Query, State};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ApiError;
use crate::fields;
use crate::limits::Caller;
use crate::server::{AppState, lenient_body};
use crate::types::AgentRequest;
use crate::webhook::WebhookConfig;

fn owner(caller: &Option<Extension<Caller>>) -> String {
    caller.as_ref().map_or_else(
        || "anonymous".to_string(),
        |Extension(c)| c.account.to_string(),
    )
}

pub async fn start(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    req: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let known = [fields::names::<AgentRequest>(), fields::CLIENT_IDENTITY];
    let (req, ignored): (AgentRequest, _) = lenient_body(req, &known)?;
    let webhook = req
        .webhook
        .as_ref()
        .filter(|v| !v.is_null())
        .map(|v| WebhookConfig::parse_agent(v, s.webhooks.ssrf()))
        .transpose()?;
    let warnings: Vec<String> = fields::ignored_warning(&ignored).into_iter().collect();
    let id = s
        .agents
        .start(
            s.engine.clone(),
            req,
            warnings.clone(),
            caller.map(|Extension(c)| c),
            webhook,
        )
        .await?;
    let mut out = json!({ "success": true, "id": id });
    if !warnings.is_empty() {
        out["warnings"] = json!(warnings);
    }
    Ok(Json(out))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct StatusQuery {
    /// pagesift 扩展：带上存档的证据全文。
    include_evidence: bool,
}

pub async fn status(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    Path(id): Path<String>,
    Query(q): Query<StatusQuery>,
) -> Result<Json<Value>, ApiError> {
    let body = s
        .agents
        .status(&id, &owner(&caller), q.include_evidence)
        .await?;
    Ok(Json(body))
}

pub async fn cancel(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    s.agents.cancel(&id, &owner(&caller)).await?;
    Ok(Json(json!({ "success": true })))
}
