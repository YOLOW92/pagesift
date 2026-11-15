//! 1.5 Loop 数据源（DataSource）API 类型定义与校验辅助函数。

#[cfg(test)]
mod aba_history_tests;
pub mod routes;
#[cfg(test)]
mod tests;

use pagesift_store::{
    DataSourceSpec, FreshnessPolicy, SourceStateView, SyncBudget, SyncDelta, now_store_ms,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ApiError;

pub(crate) use routes::routes;

pub use pagesift_store::MAX_REQUESTS_PER_TICK_CAP;

const MAX_SOURCE_ID_LEN: usize = 96;
const MAX_SEEDS_PER_SOURCE: usize = 64;

/// 校验单次运行覆盖的 `maxRequests` 参数，所有入口共用统一上限（C11）。
pub fn validate_max_requests(raw: Option<usize>) -> Result<Option<usize>, ApiError> {
    if let Some(n) = raw
        && (n == 0 || n > MAX_REQUESTS_PER_TICK_CAP)
    {
        return Err(ApiError::invalid(format!(
            "maxRequests must be in 1..={MAX_REQUESTS_PER_TICK_CAP}"
        )));
    }
    Ok(raw)
}

/// `POST /v2/sources` 请求体。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSourceRequest {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    pub seeds: Vec<String>,
    #[serde(default)]
    pub include_paths: Vec<String>,
    #[serde(default)]
    pub exclude_paths: Vec<String>,
    #[serde(default = "default_max_depth")]
    pub max_depth: usize,
    #[serde(default)]
    pub allow_subdomains: bool,
    #[serde(default)]
    pub freshness: FreshnessPolicy,
    #[serde(default)]
    pub budget: SyncBudget,
    #[serde(default)]
    pub extract_schema: Option<Value>,
    #[serde(default)]
    pub extract_prompt: Option<String>,
    #[serde(default)]
    pub required_fields: Vec<String>,
    #[serde(default = "default_allow_browser")]
    pub allow_browser: bool,
}

fn default_max_depth() -> usize {
    2
}

fn default_allow_browser() -> bool {
    true
}

/// `POST /v2/sources/{id}/sync` 可选请求体。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggerSyncRequest {
    #[serde(default)]
    pub force_all: bool,
    #[serde(default)]
    pub max_requests: Option<usize>,
}

/// `GET /v2/sources/{id}/state` 查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SourceStateQuery {
    #[serde(rename = "asOf", alias = "as_of", default)]
    pub as_of: Option<String>,
}

/// `GET /v2/sources/{id}/deltas` 查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SourceDeltasQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceResponse {
    pub success: bool,
    pub source: DataSourceSpec,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcesListResponse {
    pub success: bool,
    pub total: usize,
    pub sources: Vec<DataSourceSpec>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSourceResponse {
    pub success: bool,
    pub id: String,
    pub deleted: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncTickResponse {
    pub success: bool,
    pub delta: SyncDelta,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStateResponse {
    pub success: bool,
    pub state: SourceStateView,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDeltasResponse {
    pub success: bool,
    pub source_id: String,
    pub total: usize,
    pub deltas: Vec<SyncDelta>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDeltaResponse {
    pub success: bool,
    pub delta: SyncDelta,
}

pub fn validate_source_id(id: &str) -> Result<String, ApiError> {
    let trimmed = id.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_SOURCE_ID_LEN {
        return Err(ApiError::invalid(format!(
            "source id must be 1..={MAX_SOURCE_ID_LEN} characters"
        )));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(ApiError::invalid(
            "source id may only contain ASCII letters, digits, '-', '_', or '.'",
        ));
    }
    Ok(trimmed.to_string())
}

pub fn build_source_spec(
    req: CreateSourceRequest,
    now_ms: Option<u64>,
) -> Result<DataSourceSpec, ApiError> {
    let id = match req.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => validate_source_id(raw)?,
        None => format!("src-{}", uuid::Uuid::new_v4().simple()),
    };
    if req.seeds.is_empty() || req.seeds.len() > MAX_SEEDS_PER_SOURCE {
        return Err(ApiError::invalid(format!(
            "seeds must contain between 1 and {MAX_SEEDS_PER_SOURCE} HTTP(S) URLs"
        )));
    }
    let mut seeds = Vec::with_capacity(req.seeds.len());
    for raw in req.seeds {
        let trimmed = raw.trim();
        crate::validate::check_url(trimmed)?;
        seeds.push(trimmed.to_string());
    }
    if req.freshness.min_interval_secs == 0 {
        return Err(ApiError::invalid("freshness.minIntervalSecs must be >= 1"));
    }
    if req.freshness.default_interval_secs < req.freshness.min_interval_secs
        || req.freshness.max_interval_secs < req.freshness.default_interval_secs
    {
        return Err(ApiError::invalid(
            "freshness intervals must satisfy minIntervalSecs <= defaultIntervalSecs <= maxIntervalSecs",
        ));
    }
    if req.freshness.confirm_gone_after_404s == 0 {
        return Err(ApiError::invalid(
            "freshness.confirmGoneAfter404s must be >= 1",
        ));
    }
    if req.budget.max_requests_per_tick == 0
        || req.budget.max_requests_per_tick > MAX_REQUESTS_PER_TICK_CAP
    {
        return Err(ApiError::invalid(format!(
            "budget.maxRequestsPerTick must be in 1..={MAX_REQUESTS_PER_TICK_CAP}"
        )));
    }
    if req.budget.max_wall_clock_ms < 500 {
        return Err(ApiError::invalid("budget.maxWallClockMs must be >= 500"));
    }
    if let Some(schema) = &req.extract_schema
        && !schema.is_object()
    {
        return Err(ApiError::invalid("extractSchema must be a JSON object"));
    }
    let ts = now_ms.unwrap_or_else(now_store_ms);
    let spec = DataSourceSpec {
        id,
        instance_id: String::new(),
        revision: 1,
        name: req.name.and_then(|v| {
            let t = v.trim().to_string();
            (!t.is_empty()).then_some(t)
        }),
        seeds,
        include_paths: req.include_paths,
        exclude_paths: req.exclude_paths,
        max_depth: req.max_depth.min(crate::validate::MAX_CRAWL_DEPTH as usize),
        allow_subdomains: req.allow_subdomains,
        freshness: req.freshness,
        budget: req.budget,
        extract_schema: req.extract_schema,
        extract_prompt: req.extract_prompt.and_then(|v| {
            let t = v.trim().to_string();
            (!t.is_empty()).then_some(t)
        }),
        required_fields: req.required_fields,
        allow_browser: req.allow_browser,
        created_at_ms: ts,
        updated_at_ms: ts,
    };
    for seed in &spec.seeds {
        let parsed = url::Url::parse(seed)
            .map_err(|_| ApiError::invalid(format!("invalid seed URL '{seed}'")))?;
        if !spec.allows_path(parsed.path()) {
            return Err(ApiError::invalid(format!(
                "seed URL '{seed}' violates includePaths/excludePaths scope"
            )));
        }
    }
    Ok(spec)
}
