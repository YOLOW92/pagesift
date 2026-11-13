//! `/v2/agent` 的请求和结果。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ApiError;

/// `POST /v2/agent`。字段照 Firecrawl；`allowedHosts` 是 pagesift 扩展。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRequest {
    pub prompt: String,
    #[serde(default)]
    pub urls: Option<Vec<String>>,
    #[serde(default)]
    pub schema: Option<Value>,
    #[serde(default)]
    pub effort: Option<AgentEffort>,
    /// 只访问 `urls` 里的网址。
    #[serde(default, rename = "strictConstrainToURLs")]
    pub strict_constrain_to_urls: bool,
    /// 额度上限：每抓到一页、每次搜索、每次 map 各算 1。
    #[serde(default)]
    pub max_credits: Option<u64>,
    #[serde(default)]
    pub webhook: Option<Value>,
    /// pagesift 扩展：模型只能打开这些主机（含子域名）上的网址。
    #[serde(default)]
    pub allowed_hosts: Option<Vec<String>>,
}

/// 只改变预算上限，不改变停止判据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentEffort {
    Low,
    Medium,
    High,
}

impl AgentEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// 一次运行的结果。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentOutcome {
    /// 模型的最后回答。结果不完整时对外放在 `partial`。
    pub data: Option<Value>,
    /// 请求带 `schema` 时，`data` 是否符合。
    pub schema_valid: Option<bool>,
    /// `answered`（模型自己判断够了）、`coverage_reached`、`saturated`、`no_relevant_pages`（自适应
    /// 停止判据，和带查询的爬取相同）、`budget_exhausted`、`cancelled`、`error`。
    pub finish_reason: String,
    /// 没有缺失、有回答、符合 schema。
    pub complete: bool,
    /// 模型报告找不到的内容。
    pub missing: Vec<String>,
    pub message: Option<String>,
    pub sources: Vec<AgentSource>,
    /// 键是 `data` 里叶子字段的 JSON Pointer。
    pub field_sources: BTreeMap<String, FieldSource>,
    pub evidence: Vec<AgentEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_spans: Vec<pagesift_graph::EvidenceSpan>,
    pub steps: Vec<AgentStep>,
    pub usage: AgentUsage,
    pub budget: AgentBudget,
    pub warnings: Vec<String>,
    pub error: Option<AgentFailure>,
}

impl AgentOutcome {
    /// 用掉的额度：每抓到一页、每次搜索、每次 map 各算 1。
    pub fn credits(&self) -> u64 {
        self.usage.credits()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentSource {
    pub url: String,
    pub title: Option<String>,
    pub evidence_ids: Vec<String>,
    pub content_hash: Option<String>,
    pub fetched_at: Option<String>,
}

/// 一个字段来自哪里。`verified`：字段值逐词出现在这些证据的文字里（总结出来的长文字、布尔值不会是真）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FieldSource {
    pub sources: Vec<String>,
    pub evidence_ids: Vec<String>,
    pub verified: bool,
}

/// 存档的证据：模型看到的文字。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentEvidence {
    pub id: String,
    pub url: String,
    /// `page`（带 queryFilter 的 scrape）、`search`（搜索结果的摘要）、`extract`（按意图抽取的列表）。
    pub kind: String,
    pub query: Option<String>,
    pub text: String,
    pub relevance: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentStep {
    pub turn: usize,
    /// `search`、`map`、`scrape`、`extract`。
    pub tool: String,
    pub input: Value,
    /// `ok` 或错误码（`ROBOTS_DISALLOWED` 等）。
    pub result: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentUsage {
    pub pages: u64,
    pub pages_failed: u64,
    pub searches: u64,
    pub maps: u64,
    pub llm_calls: u64,
    pub tokens: u64,
    /// 有的调用服务没报用量，用的是估计值。
    pub tokens_estimated: bool,
}

impl AgentUsage {
    pub fn credits(&self) -> u64 {
        self.pages + self.searches + self.maps
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentBudget {
    pub max_pages: u64,
    pub max_llm_calls: u64,
    pub max_tokens: u64,
    pub max_credits: Option<u64>,
    /// 一次调用的提示上限（模型上下文和每分钟 token 限额推出）。
    pub input_tokens_per_call: u64,
    /// 用尽而且结果不完整时：`pages`、`credits`、`quota`、`llmCalls`、`tokens`。
    pub exhausted: Option<String>,
}

/// 运行失败的原因（错误码和说明）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentFailure {
    pub code: String,
    pub message: String,
}

impl From<&ApiError> for AgentFailure {
    fn from(e: &ApiError) -> Self {
        Self {
            code: e.code.as_str().to_string(),
            message: e.message.clone(),
        }
    }
}
