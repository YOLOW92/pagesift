//! agent 任务：提交后异步跑，结果留 [`JOB_TTL`]。配了存储时写进 `agent/<id>`，重启后结束的任务照样能查；
//! 重启时还没跑完的任务不续跑（模型的对话没有存），如实报 `failed`。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pagesift_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::engine::{AgentSink, BoxFuture, Engine};
use crate::error::{ApiError, ErrorCode};
use crate::jobs::{JOB_TTL, rfc3339};
use crate::limits::{Caller, Limiter, PageHold};
use crate::types::{AgentFailure, AgentOutcome, AgentRequest, AgentStep};
use crate::validate::{check_url, check_url_count};
use crate::webhook::{WebhookConfig, Webhooks};

/// [协议] Firecrawl 文档：`prompt` 最长 10,000 个字符。
const MAX_PROMPT_CHARS: usize = 10_000;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentJob {
    id: String,
    owner: String,
    /// `processing`、`completed`、`failed`。
    status: String,
    created_ms: u64,
    effort: Option<String>,
    /// 提交时的警告（`ignored_fields`）。
    warnings: Vec<String>,
    credits: u64,
    webhook: Option<WebhookConfig>,
    outcome: Option<AgentOutcome>,
    #[serde(skip)]
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct Inner {
    map: Mutex<HashMap<String, AgentJob>>,
    store: Option<Store>,
    webhooks: Option<Webhooks>,
    limiter: Option<Arc<Limiter>>,
}

/// agent 任务表。HTTP 服务和 MCP 各有一份（MCP 的没有存储、webhook 和配额）。
#[derive(Clone, Default)]
pub struct Agents(Arc<Inner>);

fn key(id: &str) -> String {
    format!("agent/{id}")
}

fn millis(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn created_at(job: &AgentJob) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(job.created_ms)
}

/// 校验请求体（和具体入口无关）。
pub fn check_agent_request(req: &AgentRequest) -> Result<(), ApiError> {
    if req.prompt.trim().is_empty() {
        return Err(ApiError::invalid("prompt must not be empty"));
    }
    if req.prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(ApiError::invalid(format!(
            "prompt is longer than {MAX_PROMPT_CHARS} characters"
        )));
    }
    let urls = req.urls.as_deref().unwrap_or_default();
    check_url_count(urls.len())?;
    for u in urls {
        check_url(u)?;
    }
    if req.strict_constrain_to_urls && urls.is_empty() {
        return Err(ApiError::invalid(
            "strictConstrainToURLs needs a non-empty urls list",
        ));
    }
    if req.schema.as_ref().is_some_and(|v| !v.is_object()) {
        return Err(ApiError::invalid("schema must be a JSON object"));
    }
    if req.max_credits == Some(0) {
        return Err(ApiError::invalid("maxCredits must be at least 1"));
    }
    Ok(())
}

impl Agents {
    pub(crate) fn new(
        store: Option<Store>,
        webhooks: Option<Webhooks>,
        limiter: Option<Arc<Limiter>>,
    ) -> Self {
        let mut map = HashMap::new();
        if let Some(s) = &store
            && let Ok(keys) = s.tasks().kv_keys("agent/")
        {
            for k in keys {
                let Ok(Some(v)) = s.tasks().kv_get(&k) else {
                    continue;
                };
                let Ok(mut job) = serde_json::from_slice::<AgentJob>(&v) else {
                    continue;
                };
                if created_at(&job) + JOB_TTL <= SystemTime::now() {
                    let _ = s.tasks().kv_delete(&k);
                    continue;
                }
                if job.status == "processing" {
                    job.status = "failed".into();
                    job.outcome = Some(AgentOutcome {
                        finish_reason: "error".into(),
                        error: Some(AgentFailure {
                            code: ErrorCode::Internal.as_str().into(),
                            message: "the server restarted while this agent run was in progress; \
                                      the run was not resumed"
                                .into(),
                        }),
                        ..Default::default()
                    });
                    if let Ok(v) = serde_json::to_vec(&job) {
                        let _ = s.tasks().kv_put(&k, &v);
                    }
                }
                map.insert(job.id.clone(), job);
            }
        }
        Self(Arc::new(Inner {
            map: Mutex::new(map),
            store,
            webhooks,
            limiter,
        }))
    }

    fn persist(&self, job: &AgentJob) {
        if let (Some(store), Ok(v)) = (&self.0.store, serde_json::to_vec(job))
            && let Err(e) = store.tasks().kv_put(key(&job.id), &v)
        {
            tracing::warn!(job = %job.id, error = %e, "could not save the agent run");
        }
    }

    /// 没有调用方身份、webhook 和配额的入口（MCP）。
    pub async fn start_local(
        &self,
        engine: Arc<dyn Engine>,
        req: AgentRequest,
    ) -> Result<String, ApiError> {
        self.start(engine, req, Vec::new(), None, None).await
    }

    /// 校验、登记、开跑，返回任务 id。没配 LLM 等问题在这里就报错。
    pub(crate) async fn start(
        &self,
        engine: Arc<dyn Engine>,
        req: AgentRequest,
        warnings: Vec<String>,
        caller: Option<Caller>,
        webhook: Option<WebhookConfig>,
    ) -> Result<String, ApiError> {
        check_agent_request(&req)?;
        engine.agent_check(&req)?;
        let id = uuid::Uuid::new_v4().to_string();
        let owner = caller
            .as_ref()
            .map_or_else(|| "anonymous".to_string(), |c| c.account.to_string());
        let cancel = Arc::new(AtomicBool::new(false));
        let job = AgentJob {
            id: id.clone(),
            owner: owner.clone(),
            status: "processing".into(),
            created_ms: millis(SystemTime::now()),
            effort: req.effort.map(|e| e.as_str().to_string()),
            warnings,
            credits: 0,
            webhook,
            outcome: None,
            cancel: cancel.clone(),
        };
        {
            let mut map = self.0.map.lock().await;
            let now = SystemTime::now();
            map.retain(|k, j| {
                let live = created_at(j) + JOB_TTL > now;
                if !live && let Some(s) = &self.0.store {
                    let _ = s.tasks().kv_delete(key(k));
                }
                live
            });
            if let (Some(c), Some(l)) = (&caller, &self.0.limiter) {
                if c.limits.max_concurrent_jobs.is_some() {
                    let active = map
                        .values()
                        .filter(|j| j.status == "processing" && j.owner == owner)
                        .count();
                    l.check_jobs(c, active)?;
                }
                l.check_quota(c)?;
            }
            map.insert(id.clone(), job.clone());
        }
        self.persist(&job);
        self.notify(&job, "started", true, json!([]), None).await;

        let sink: Arc<dyn AgentSink> = Arc::new(JobSink {
            agents: self.clone(),
            id: id.clone(),
            cancel,
            caller,
            webhook: job.webhook.clone(),
            metadata: job.clone(),
            holds: Mutex::new(HashMap::new()),
        });
        let agents = self.clone();
        let job_id = id.clone();
        tokio::spawn(async move {
            let outcome = engine.agent(&req, sink).await;
            agents.finish(&job_id, outcome).await;
        });
        Ok(id)
    }

    async fn finish(&self, id: &str, outcome: AgentOutcome) {
        let status = status_of(&outcome);
        let job = {
            let mut map = self.0.map.lock().await;
            let Some(job) = map.get_mut(id) else { return };
            job.status = status.into();
            job.credits = outcome.credits();
            job.outcome = Some(outcome);
            job.clone()
        };
        self.persist(&job);
        let o = job.outcome.as_ref().expect("just set");
        let event = match (status, o.finish_reason.as_str()) {
            ("completed", _) => "completed",
            (_, "cancelled") => "cancelled",
            _ => "failed",
        };
        let body = agent_body(o);
        let error = body
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string);
        self.notify(&job, event, status == "completed", json!([body]), error)
            .await;
    }

    /// 发一条 webhook（`agent.<event>`），载荷照 Firecrawl。
    async fn notify(
        &self,
        job: &AgentJob,
        event: &str,
        success: bool,
        data: Value,
        error: Option<String>,
    ) {
        let (Some(cfg), Some(hooks)) = (&job.webhook, &self.0.webhooks) else {
            return;
        };
        if !cfg.events.iter().any(|e| e == event) {
            return;
        }
        let mut body = json!({
            "success": success,
            "type": format!("agent.{event}"),
            "id": job.id,
            "data": data,
            "metadata": cfg.metadata,
        });
        if let Some(e) = error {
            body["error"] = json!(e);
        }
        let _ = hooks.deliver(cfg, &body).await;
    }

    /// 查询：只有提交者能看。`evidence` 为真时带上存档的证据全文。
    pub async fn status(&self, id: &str, owner: &str, evidence: bool) -> Result<Value, ApiError> {
        let mut map = self.0.map.lock().await;
        if map
            .get(id)
            .is_some_and(|job| created_at(job) + JOB_TTL <= SystemTime::now())
        {
            map.remove(id);
            if let Some(store) = &self.0.store {
                let _ = store.tasks().kv_delete(key(id));
            }
        }
        let job = map
            .get(id)
            .filter(|j| j.owner == owner && created_at(j) + JOB_TTL > SystemTime::now())
            .ok_or_else(|| not_found(id))?;
        let mut body = match &job.outcome {
            Some(o) if evidence => agent_body_with_evidence(o),
            Some(o) => agent_body(o),
            None => json!({
                "success": true,
                "status": job.status,
                "creditsUsed": job.credits,
            }),
        };
        body["expiresAt"] = json!(rfc3339(created_at(job) + JOB_TTL));
        if let Some(e) = &job.effort {
            body["effort"] = json!(e);
        }
        if !job.warnings.is_empty() {
            let mut all = job.warnings.clone();
            if let Some(w) = body["warnings"].as_array() {
                all.extend(w.iter().filter_map(|v| v.as_str().map(str::to_string)));
            }
            body["warnings"] = json!(all);
        }
        Ok(body)
    }

    /// 取消：在两步之间生效，已经发出的模型调用或抓取会先跑完。
    pub async fn cancel(&self, id: &str, owner: &str) -> Result<(), ApiError> {
        let map = self.0.map.lock().await;
        let job = map
            .get(id)
            .filter(|j| j.owner == owner && created_at(j) + JOB_TTL > SystemTime::now())
            .ok_or_else(|| not_found(id))?;
        job.cancel.store(true, Ordering::SeqCst);
        Ok(())
    }
}

fn not_found(id: &str) -> ApiError {
    ApiError::new(ErrorCode::NotFound, format!("agent job {id} not found"))
}

/// 对外的任务状态：出错、取消、预算用尽而结果不完整都是 `failed`（和 Firecrawl 一样，`failed` 时没有 `data`）。
fn status_of(o: &AgentOutcome) -> &'static str {
    if o.error.is_some() || o.budget.exhausted.is_some() || o.finish_reason == "cancelled" {
        "failed"
    } else {
        "completed"
    }
}

/// 结束后的响应体：`GET /v2/agent/{id}` 和 CLI 输出的共同部分。
pub fn agent_body(o: &AgentOutcome) -> Value {
    let status = status_of(o);
    let mut b = json!({
        "success": status == "completed",
        "status": status,
        "creditsUsed": o.credits(),
        "finishReason": o.finish_reason,
        "complete": o.complete,
        "missing": o.missing,
        "sources": o.sources,
        "fieldSources": o.field_sources,
        "usage": o.usage,
        "budget": o.budget,
        "steps": o.steps,
    });
    if let Some(m) = &o.message {
        b["message"] = json!(m);
    }
    if !o.warnings.is_empty() {
        b["warnings"] = json!(o.warnings);
    }
    if status == "completed" {
        b["data"] = o.data.clone().unwrap_or(Value::Null);
        return b;
    }
    if let Some(partial) = o.data.as_ref().filter(|d| !d.is_null()) {
        b["partial"] = partial.clone();
        if let Some(valid) = o.schema_valid {
            b["partialSchemaValid"] = json!(valid);
        }
    }
    if let Some(kind) = &o.budget.exhausted {
        b["stopReason"] = json!("credit_limit_reached");
        b["error"] = json!(format!("the run reached its budget ({kind})"));
    } else if let Some(e) = &o.error {
        b["error"] = json!(format!("{}: {}", e.code, e.message));
    } else {
        b["error"] = json!("the agent run was cancelled");
    }
    b
}

/// [`agent_body`] 加上存档的证据全文。
pub fn agent_body_with_evidence(o: &AgentOutcome) -> Value {
    let mut b = agent_body(o);
    b["evidence"] = json!(o.evidence);
    b
}

struct JobSink {
    agents: Agents,
    id: String,
    cancel: Arc<AtomicBool>,
    caller: Option<Caller>,
    webhook: Option<WebhookConfig>,
    /// 发 webhook 用的任务快照。
    metadata: AgentJob,
    /// 已预留的页面配额，按 URL 索引。
    holds: Mutex<HashMap<String, PageHold>>,
}

impl AgentSink for JobSink {
    fn step<'a>(&'a self, step: &'a AgentStep, credits: u64) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Some(j) = self.agents.0.map.lock().await.get_mut(&self.id) {
                j.credits = credits;
            }
            if self.webhook.is_some() {
                let (agents, job, data) = (
                    self.agents.clone(),
                    self.metadata.clone(),
                    json!([{ "creditsUsed": credits, "action": step }]),
                );
                // 收件方慢或者不通时不能拖住这次运行
                tokio::spawn(async move {
                    agents.notify(&job, "action", true, data, None).await;
                });
            }
        })
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn reserve_page<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<(), ApiError>> {
        Box::pin(async move {
            let mut holds = self.holds.lock().await;
            if holds.contains_key(url) {
                return Ok(());
            }
            let (Some(c), Some(l)) = (&self.caller, &self.agents.0.limiter) else {
                return Ok(());
            };
            let hold = l.reserve_page(c).await?;
            holds.insert(url.to_string(), hold);
            Ok(())
        })
    }

    fn charge_page<'a>(&'a self, url: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let hold = { self.holds.lock().await.remove(url) };
            if let Some(hold) = hold {
                hold.commit().await;
            }
        })
    }

    fn release_page<'a>(&'a self, url: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            drop(self.holds.lock().await.remove(url));
        })
    }

    fn pages_left(&self) -> Option<u64> {
        let (c, l) = (self.caller.as_ref()?, self.agents.0.limiter.as_ref()?);
        l.remaining_pages(c)
    }
}
