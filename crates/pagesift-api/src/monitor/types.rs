use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::types::ScrapeOptions;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_ref: Option<String>,
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
    pub(super) fn summary(&self) -> Value {
        let n = |s: &str| self.pages.iter().filter(|p| p.status == s).count();
        json!({
            "totalPages": self.pages.len(), "same": n("same"), "changed": n("changed"),
            "new": n("new"), "removed": n("removed"), "error": n("error"),
        })
    }
}
