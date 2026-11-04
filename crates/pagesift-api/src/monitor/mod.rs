//! 监控（Firecrawl 的 `/v2/monitor` 形状）：按计划重复检查网址，发现变化时通知。
//!
//! - `POST /v2/monitor` 创建，`GET /v2/monitor` 列出，`GET/DELETE /v2/monitor/{id}`，
//!   `GET /v2/monitor/{id}/checks`、`GET /v2/monitor/{id}/checks/{checkId}` 看检查结果；
//!   `/v2/monitors` 是同样的别名。
//! - 目标：`{"type": "scrape", "urls": [...], "scrapeOptions": {...}}`，或
//!   `{"type": "crawl", "url": "...", "crawlOptions": {...}}`（每次检查爬一遍，比较新增、变化、消失的页面）。
//!   `search` 目标不支持（如实报错）。
//! - 计划：给了 `schedule.cron` 或 `schedule.text` 就按那个周期（和 Firecrawl 一样）；pagesift 扩展
//!   `schedule.minIntervalMs`、`schedule.maxIntervalMs`（毫秒）或不给计划时，每个网址的间隔按变化率的后验
//!   自适应（`pagesift_stat::poisson`），下界 5 分钟（Firecrawl 的最短间隔）。
//! - 检查带 `If-None-Match` / `If-Modified-Since`；变化用块级、区分噪声的 diff 判断，默认只有实质变化
//!   才算 `changed`（pagesift 扩展 `notify: "any"` 时噪声变化也算）；配了 `extract` 格式时附字段级变化。
//! - 通知：`webhook`（`monitor.page`、`monitor.check.completed`，载荷照 Firecrawl）。邮件、Slack、
//!   `goal` 的 LLM 判断不支持：`notification` 报错，`goal` 照存但在 `warnings` 里说明没有判断。
//! - 服务端调度；多服务器时只有持有网关租约的机器调度（`Config::monitor_leader`）。

mod crawl;
mod run;
pub mod schedule;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify};

use crate::error::{ApiError, ErrorCode};
use crate::limits::{Caller, KeyLimits};
use crate::server::{AppState, lenient_body};
use crate::types::ScrapeOptions;
use crate::webhook::WebhookConfig;

pub(crate) use run::start_scheduler;

/// 监控能做什么判断：多服务器时由网关租约决定本机是否调度。
pub type Leader = Arc<dyn Fn() -> bool + Send + Sync>;

/// 请求体（Firecrawl 字段加 pagesift 扩展）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub schedule: Option<Schedule>,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub judge_enabled: Option<bool>,
    pub targets: Vec<Target>,
    #[serde(default)]
    pub webhook: Option<Value>,
    #[serde(default)]
    pub notification: Option<Value>,
    #[serde(default)]
    pub retention_days: Option<u32>,
    /// pagesift 扩展：`substantive`（默认，噪声变化不算）或 `any`。
    #[serde(default)]
    pub notify: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// pagesift 扩展：最短、最长检查间隔（毫秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_interval_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_interval_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrape_options: Option<ScrapeOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crawl_options: Option<Value>,
}

/// 一个网址的记录：上次的内容、条件请求用的校验值、变化率的观测、下次到期时间。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PageState {
    pub markdown: Option<String>,
    pub extract: Option<Value>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// 上次拿到结果的检查（Unix 秒）：变化率的观测区间从这里算，抓取失败不动它。
    pub last_check: Option<f64>,
    /// 上次尝试（Unix 秒，失败也算）：下次到期时间从这里算。
    #[serde(default)]
    pub last_attempt: Option<f64>,
    /// 已经报过 `removed`：还是 404 就不再报。
    #[serde(default)]
    pub removed: bool,
    /// 变化率观测（`pagesift_stat::poisson::ChangeRate` 的字段）：没变的总时长、各个变了的区间。
    pub exposure: Option<f64>,
    #[serde(default)]
    pub changed_intervals: Vec<f64>,
    /// 缓存：变化率后验均值（每秒）和 δ 规则的间隔（秒），重新分配预算时用。
    #[serde(default)]
    pub rate_mean: Option<f64>,
    #[serde(default)]
    pub delta_interval: Option<f64>,
    /// 爬取目标里的页面跟着目标检查，是 INFINITY；JSON 里写成 null，读回时还原。
    #[serde(deserialize_with = "due_or_never")]
    pub next_due: f64,
    /// crawl 目标：这个网址来自哪个目标（消失时报 removed）。
    #[serde(default)]
    pub target: String,
}

fn due_or_never<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(d)?.unwrap_or(f64::INFINITY))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PageResult {
    pub url: String,
    /// `same`、`new`、`changed`、`removed`、`error`。
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 只有噪声变化（日期、计数等），按 `notify` 不算 changed。
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub noise_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub field_changes: Vec<Value>,
    /// 用的是条件请求的 304。
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub not_modified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Check {
    pub id: String,
    pub started_at: String,
    pub finished_at: String,
    pub status: String,
    pub pages: Vec<PageResult>,
}

impl Check {
    fn summary(&self) -> Value {
        let n = |s: &str| self.pages.iter().filter(|p| p.status == s).count();
        json!({
            "totalPages": self.pages.len(), "same": n("same"), "changed": n("changed"),
            "new": n("new"), "removed": n("removed"), "error": n("error"),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MonitorState {
    pub id: String,
    pub owner: String,
    pub req: MonitorRequest,
    pub created_at: String,
    pub status: String,
    /// 固定周期（秒）：给了 cron 或文本时；自适应时为 `None`。
    pub period: Option<u64>,
    pub min_interval: u64,
    pub max_interval: Option<u64>,
    pub webhook: Option<WebhookConfig>,
    pub pages: BTreeMap<String, PageState>,
    pub checks: VecDeque<Check>,
    pub last_run_at: Option<String>,
    pub warnings: Vec<String>,
}

impl MonitorState {
    pub(crate) fn next_due(&self) -> Option<f64> {
        self.pages
            .values()
            .map(|p| p.next_due)
            .fold(None, |a, b| Some(a.map_or(b, |a: f64| a.min(b))))
    }

    fn view(&self) -> Value {
        let schedule = match self.period {
            Some(p) => {
                // cron 原样回显；时刻和时区不生效时 `warnings` 里有说明，实际按 UTC 的周期算
                let given = self.req.schedule.as_ref().and_then(|s| s.cron.clone());
                json!({ "cron": given.unwrap_or_else(|| schedule::normalized_cron(p)), "timezone": "UTC" })
            }
            None => {
                json!({ "adaptive": true, "minIntervalMs": self.min_interval * 1000, "maxIntervalMs": self.max_interval.map(|m| m * 1000) })
            }
        };
        let next = self.next_due().map(run::rfc3339_secs);
        let mut v = json!({
            "id": self.id, "name": self.req.name, "status": self.status, "schedule": schedule,
            "nextRunAt": next, "lastRunAt": self.last_run_at, "goal": self.req.goal,
            "judgeEnabled": false, "targets": self.req.targets, "webhook": self.webhook,
            "retentionDays": self.req.retention_days.unwrap_or(DEFAULT_RETENTION_DAYS),
            "createdAt": self.created_at,
        });
        if !self.warnings.is_empty() {
            v["warnings"] = json!(self.warnings);
        }
        v
    }
}

/// [协议] Firecrawl 文档：`retentionDays` 默认 30，最多 365。
const DEFAULT_RETENTION_DAYS: u32 = 30;
const MAX_RETENTION_DAYS: u32 = 365; // [协议] 同上
/// [协议] Firecrawl 文档：每个监控 1–50 个目标。
const MAX_TARGETS: usize = 50;
/// [策略] 一个账户最多这么多个监控：每个监控都存页面全文，常驻内存
const MAX_MONITORS_PER_ACCOUNT: usize = 1000;
/// [策略] 每个监控保留的检查记录条数上限，和 `retentionDays` 同时生效：5 分钟一次时一年有 10 万次
const MAX_CHECKS_KEPT: usize = 1000;

/// 所有监控（进程内），配了存储时同时落盘（`monitor/<id>`）。
#[derive(Clone, Default)]
pub(crate) struct Monitors {
    pub map: Arc<Mutex<HashMap<String, MonitorState>>>,
    pub wake: Arc<Notify>,
    /// 调度任务在跑：只在有监控时运行（它持有服务状态和存储，空闲时不该占着）。
    pub running: Arc<std::sync::atomic::AtomicBool>,
    pub leader: Arc<std::sync::Mutex<Option<Leader>>>,
    /// 各账户的限额：后台检查按监控的主人扣页面配额。
    pub accounts: Arc<HashMap<String, Arc<KeyLimits>>>,
    pub default_limits: Arc<KeyLimits>,
}

impl Monitors {
    fn caller(&self, owner: &str) -> Caller {
        let limits = self.accounts.get(owner).unwrap_or(&self.default_limits);
        Caller::named(owner, limits.clone())
    }
}

const KNOWN: &[&str] = &[
    "name",
    "schedule",
    "goal",
    "judgeEnabled",
    "targets",
    "webhook",
    "notification",
    "retentionDays",
    "notify",
];

fn owner_of(caller: &Option<Extension<Caller>>) -> String {
    caller.as_ref().map_or_else(
        || "anonymous".to_string(),
        |Extension(c)| c.account.to_string(),
    )
}

pub(crate) async fn create(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    req: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let (req, ignored): (MonitorRequest, _) =
        lenient_body(req, &[KNOWN, crate::fields::CLIENT_IDENTITY])?;
    let mut warnings: Vec<String> = crate::fields::ignored_warning(&ignored)
        .into_iter()
        .collect();
    if req.targets.is_empty() || req.targets.len() > MAX_TARGETS {
        return Err(ApiError::invalid(format!(
            "a monitor needs 1 to {MAX_TARGETS} targets"
        )));
    }
    if req.notification.is_some() {
        return Err(ApiError::unsupported(
            "notification (email, Slack) is not supported: use a webhook",
        ));
    }
    if req.judge_enabled == Some(true) {
        return Err(ApiError::unsupported(
            "judgeEnabled: LLM judging of changes is not supported; changes are classified by a noise-aware block diff",
        ));
    }
    if req.goal.is_some() {
        warnings.push(
            "monitor_goal_not_judged: the goal is stored but changes are not judged against it"
                .into(),
        );
    }
    let notify = req.notify.as_deref().unwrap_or("substantive");
    if !matches!(notify, "substantive" | "any") {
        return Err(ApiError::invalid("notify must be substantive or any"));
    }
    if req
        .retention_days
        .is_some_and(|d| d == 0 || d > MAX_RETENTION_DAYS)
    {
        return Err(ApiError::invalid(format!(
            "retentionDays must be between 1 and {MAX_RETENTION_DAYS}"
        )));
    }
    let sched = req.schedule.clone().unwrap_or_default();
    let period = match (&sched.cron, &sched.text) {
        (Some(c), _) => Some(schedule::cron_period(c)?),
        (None, Some(t)) => Some(schedule::text_period(t)?),
        (None, None) => None,
    };
    if period.is_some_and(|p| p < schedule::MIN_INTERVAL_SECS) {
        return Err(ApiError::invalid("the minimum interval is 5 minutes"));
    }
    if period.is_some_and(|p| p > schedule::MAX_INTERVAL_SECS)
        || sched
            .min_interval_ms
            .into_iter()
            .chain(sched.max_interval_ms)
            .any(|ms| ms / 1000 > schedule::MAX_INTERVAL_SECS)
    {
        return Err(ApiError::invalid(format!(
            "the maximum interval is {} days",
            schedule::MAX_INTERVAL_SECS / 86_400
        )));
    }
    warnings.extend(schedule::unsupported(&sched, period));
    let adaptive =
        sched.min_interval_ms.is_some() || sched.max_interval_ms.is_some() || period.is_none();
    let min_interval = sched
        .min_interval_ms
        .map_or(schedule::MIN_INTERVAL_SECS, |ms| ms.div_ceil(1000))
        .max(schedule::MIN_INTERVAL_SECS);
    let max_interval = sched.max_interval_ms.map(|ms| ms / 1000).or(period);
    if max_interval.is_some_and(|m| m < min_interval) {
        return Err(ApiError::invalid(
            "maxIntervalMs must not be shorter than minIntervalMs",
        ));
    }
    let webhook = match &req.webhook {
        Some(v) => Some(WebhookConfig::parse_monitor(v, &s.webhooks.settings.ssrf)?),
        None => None,
    };
    crate::validate::check_url_count(req.targets.iter().map(|t| t.urls.len()).sum())?;
    let owner = owner_of(&caller);
    let held = s
        .monitors
        .map
        .lock()
        .await
        .values()
        .filter(|m| m.owner == owner)
        .count();
    if held >= MAX_MONITORS_PER_ACCOUNT {
        return Err(ApiError::new(
            ErrorCode::ConcurrencyLimitExceeded,
            format!("this account already has {held} monitors, the maximum"),
        ));
    }
    let now = run::now_secs();
    let id = uuid::Uuid::new_v4().to_string();
    let mut pages = BTreeMap::new();
    let mut targets = req.targets.clone();
    for t in &mut targets {
        t.id.get_or_insert_with(|| uuid::Uuid::new_v4().to_string());
        let opts = t.scrape_options.clone().unwrap_or_default();
        crate::validate::check_scrape_options(&opts, s.engine.as_ref())?;
        match t.kind.as_str() {
            "scrape" => {
                if t.urls.is_empty() {
                    return Err(ApiError::invalid("a scrape target needs urls"));
                }
                for u in &t.urls {
                    crate::validate::check_url(u)?;
                    pages.insert(
                        u.clone(),
                        PageState {
                            next_due: now,
                            target: t.id.clone().unwrap_or_default(),
                            ..Default::default()
                        },
                    );
                }
            }
            "crawl" => {
                let u = t
                    .url
                    .clone()
                    .ok_or_else(|| ApiError::invalid("a crawl target needs url"))?;
                crate::validate::check_url(&u)?;
                crawl::crawl_request(t, &u).and_then(|r| {
                    crate::validate::check_url_count(r.limit)?;
                    crate::firecrawl::crawl::check_crawl_request(&r, s.engine.as_ref())
                })?;
                // 爬取目标的“网址”是起始页：整个目标一起检查，变化率按“有没有任何页面变化”记
                pages.insert(
                    format!("crawl:{}", t.id.clone().unwrap_or_default()),
                    PageState {
                        next_due: now,
                        target: t.id.clone().unwrap_or_default(),
                        ..Default::default()
                    },
                );
            }
            "search" => {
                return Err(ApiError::unsupported(
                    "search targets are not supported; monitor known URLs (scrape) or a site (crawl)",
                ));
            }
            other => return Err(ApiError::invalid(format!("unknown target type {other:?}"))),
        }
    }
    let mut req = req;
    req.targets = targets;
    let state = MonitorState {
        id: id.clone(),
        owner,
        req,
        created_at: run::rfc3339_secs(now),
        status: "active".into(),
        period: (!adaptive).then_some(period).flatten(),
        min_interval,
        max_interval,
        webhook,
        pages,
        checks: VecDeque::new(),
        last_run_at: None,
        warnings,
    };
    let view = state.view();
    run::persist(&s, &state);
    s.monitors.map.lock().await.insert(id, state);
    s.monitors.wake.notify_one();
    run::ensure_running(&s);
    Ok(Json(json!({ "success": true, "data": view })))
}

async fn owned<'a>(
    map: &'a mut HashMap<String, MonitorState>,
    id: &str,
    owner: &str,
) -> Result<&'a mut MonitorState, ApiError> {
    map.get_mut(id)
        .filter(|m| m.owner == owner)
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("monitor {id} not found")))
}

pub(crate) async fn list(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
) -> Json<Value> {
    let owner = owner_of(&caller);
    let map = s.monitors.map.lock().await;
    let mut all: Vec<&MonitorState> = map.values().filter(|m| m.owner == owner).collect();
    all.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    Json(json!({ "success": true, "data": all.iter().map(|m| m.view()).collect::<Vec<_>>() }))
}

pub(crate) async fn get(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let mut map = s.monitors.map.lock().await;
    let m = owned(&mut map, &id, &owner_of(&caller)).await?;
    Ok(Json(json!({ "success": true, "data": m.view() })))
}

pub(crate) async fn delete(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let mut map = s.monitors.map.lock().await;
    owned(&mut map, &id, &owner_of(&caller)).await?;
    map.remove(&id);
    run::forget(&s, &id);
    // 调度任务在等下一个到期时间：叫醒它，监控清空时让它退出
    s.monitors.wake.notify_one();
    Ok(Json(json!({ "success": true })))
}

pub(crate) async fn checks(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let mut map = s.monitors.map.lock().await;
    let m = owned(&mut map, &id, &owner_of(&caller)).await?;
    let data: Vec<Value> = m
        .checks
        .iter()
        .rev()
        .map(|c| json!({ "id": c.id, "monitorId": m.id, "status": c.status, "startedAt": c.started_at, "finishedAt": c.finished_at, "summary": c.summary() }))
        .collect();
    Ok(Json(json!({ "success": true, "data": data })))
}

pub(crate) async fn check(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    Path((id, check_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let mut map = s.monitors.map.lock().await;
    let m = owned(&mut map, &id, &owner_of(&caller)).await?;
    let c =
        m.checks.iter().find(|c| c.id == check_id).ok_or_else(|| {
            ApiError::new(ErrorCode::NotFound, format!("check {check_id} not found"))
        })?;
    Ok(Json(json!({ "success": true, "data": {
        "id": c.id, "monitorId": m.id, "status": c.status, "startedAt": c.started_at,
        "finishedAt": c.finished_at, "summary": c.summary(), "pages": c.pages,
    }})))
}

/// pagesift 扩展 `POST /v2/monitor/{id}/run`：现在就检查全部网址（不等计划），返回这次的检查结果。
pub(crate) async fn run_now(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    {
        let mut map = s.monitors.map.lock().await;
        let m = owned(&mut map, &id, &owner_of(&caller)).await?;
        let now = run::now_secs();
        for p in m.pages.values_mut().filter(|p| p.next_due.is_finite()) {
            p.next_due = p.next_due.min(now);
        }
    }
    run::run_check(&s, &id).await;
    let map = s.monitors.map.lock().await;
    let m = map
        .get(&id)
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("monitor {id} not found")))?;
    let c = m.checks.back().cloned();
    Ok(Json(json!({ "success": true, "data": c.map(|c| json!({
        "id": c.id, "status": c.status, "summary": c.summary(), "pages": c.pages,
        "nextRunAt": m.next_due().map(run::rfc3339_secs),
    })) })))
}

pub(crate) fn routes() -> axum::Router<AppState> {
    use axum::routing::get as g;
    let mut r = axum::Router::new();
    for base in ["/v2/monitor", "/v2/monitors"] {
        r = r
            .route(base, axum::routing::post(create).get(list))
            .route(&format!("{base}/{{id}}"), g(get).delete(delete))
            .route(&format!("{base}/{{id}}/checks"), g(checks))
            .route(&format!("{base}/{{id}}/run"), axum::routing::post(run_now))
            .route(&format!("{base}/{{id}}/checks/{{check_id}}"), g(check));
    }
    r
}

#[cfg(test)]
mod tests;
