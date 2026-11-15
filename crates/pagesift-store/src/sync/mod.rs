//! 持续数据同步（1.5 Loop Sync）的持久化数据模型。

mod store;

pub use store::{SyncStore, TickCommitRequest};

fn default_history_status_complete() -> SourceHistoryStatus {
    SourceHistoryStatus::Complete
}

use std::collections::BTreeMap;

use pagesift_graph::{ClaimLifecycleRecord, ClaimTransition, ConflictRecord};
use serde::{Deserialize, Serialize};

use crate::to_ms;

pub const MAX_DELTAS_PER_SOURCE: usize = 64; // [策略] 每数据源保留的最近同步周期增量上限
pub const MAX_CHECKPOINTS_PER_SOURCE: usize = 256; // [策略] 每数据源保留的历史状态快照检查点上限
pub const MAX_REQUESTS_PER_TICK_CAP: usize = 10_000; // [策略] 单次同步周期最大请求数硬上限

fn default_max_depth() -> usize {
    2
}
fn default_true() -> bool {
    true
}
fn default_revision() -> u64 {
    1
}
fn default_min_interval_secs() -> u64 {
    60
}
fn default_max_interval_secs() -> u64 {
    86_400
}
fn default_interval_secs() -> u64 {
    3_600
}
fn default_confirm_gone_404s() -> u32 {
    2
}
fn default_max_requests() -> usize {
    50
}
fn default_max_browser_renders() -> usize {
    10
}
fn default_max_wall_clock_ms() -> u64 {
    60_000
}

/// 资源刷新与失联判定策略。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FreshnessPolicy {
    #[serde(default = "default_min_interval_secs")]
    pub min_interval_secs: u64,
    #[serde(default = "default_max_interval_secs")]
    pub max_interval_secs: u64,
    #[serde(default = "default_interval_secs")]
    pub default_interval_secs: u64,
    #[serde(default = "default_confirm_gone_404s")]
    pub confirm_gone_after_404s: u32,
}

impl Default for FreshnessPolicy {
    fn default() -> Self {
        Self {
            min_interval_secs: default_min_interval_secs(),
            max_interval_secs: default_max_interval_secs(),
            default_interval_secs: default_interval_secs(),
            confirm_gone_after_404s: default_confirm_gone_404s(),
        }
    }
}

/// 单次同步周期（Tick）的资源预算。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncBudget {
    #[serde(default = "default_max_requests")]
    pub max_requests_per_tick: usize,
    #[serde(default = "default_max_browser_renders")]
    pub max_browser_renders_per_tick: usize,
    #[serde(default = "default_max_wall_clock_ms")]
    pub max_wall_clock_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_items: Option<usize>,
}

impl Default for SyncBudget {
    fn default() -> Self {
        Self {
            max_requests_per_tick: default_max_requests(),
            max_browser_renders_per_tick: default_max_browser_renders(),
            max_wall_clock_ms: default_max_wall_clock_ms(),
            target_items: None,
        }
    }
}

/// 长期维护的数据源定义（`DataSource`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataSourceSpec {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instance_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extract_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extract_prompt: Option<String>,
    #[serde(default)]
    pub required_fields: Vec<String>,
    #[serde(default = "default_true")]
    pub allow_browser: bool,
    #[serde(default = "default_revision")]
    pub revision: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

impl DataSourceSpec {
    /// 生成与该数据源及提取契约（`id` + `extract_schema` + `extract_prompt` + `required_fields`）绑定的稳定契约标识（C15 / C23 / C24）。
    pub fn contract_id(&self) -> String {
        let mut h = blake3::Hasher::new();
        h.update(self.id.as_bytes());
        h.update(b"\0");
        if let Some(ref schema) = self.extract_schema
            && let Ok(raw) = serde_json::to_vec(schema)
        {
            h.update(&raw);
        }
        h.update(b"\0");
        if let Some(ref prompt) = self.extract_prompt {
            h.update(prompt.as_bytes());
        }
        h.update(b"\0");
        let mut fields = self.required_fields.clone();
        fields.sort();
        for f in fields {
            h.update(f.as_bytes());
            h.update(b"\0");
        }
        let hex = h.finalize().to_hex();
        format!("ctr:{}", &hex.as_str()[..24])
    }

    /// 判定归一化 URL 路径是否满足数据源 `include_paths` / `exclude_paths` 范围约束（C21）。
    pub fn allows_path(&self, path: &str) -> bool {
        if !self.include_paths.is_empty()
            && !self
                .include_paths
                .iter()
                .any(|inc| !inc.is_empty() && path.starts_with(inc.as_str()))
        {
            return false;
        }
        if self
            .exclude_paths
            .iter()
            .any(|exc| !exc.is_empty() && path.starts_with(exc.as_str()))
        {
            return false;
        }
        true
    }
}

/// 资源在数据源中的拓扑角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceRole {
    Seed,
    Hub,
    Detail,
    Pagination,
    FeedOrSitemap,
    Attachment,
}

impl ResourceRole {
    pub fn is_hub_like(self) -> bool {
        matches!(
            self,
            Self::Seed | Self::Hub | Self::Pagination | Self::FeedOrSitemap
        )
    }
}

/// 资源在同步清单中的生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceLifecycleState {
    Discovered,
    Active,
    Migrated,
    TransientFailure,
    SuspectedGone,
    ConfirmedGone,
}

impl ResourceLifecycleState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discovered => "discovered",
            Self::Active => "active",
            Self::Migrated => "migrated",
            Self::TransientFailure => "transient_failure",
            Self::SuspectedGone => "suspected_gone",
            Self::ConfirmedGone => "confirmed_gone",
        }
    }
}

/// 数据源资源清单（`ResourceInventory`）中的单条资源记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceRecord {
    pub resource_key: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrated_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_url: Option<String>,
    pub depth: usize,
    pub role: ResourceRole,
    pub state: ResourceLifecycleState,
    pub discovered_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_substantive_change_at_ms: Option<u64>,
    pub next_due_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status_code: Option<u16>,
    pub consecutive_failures: u32,
    pub consecutive_404s: u32,
    pub importance_weight: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_strategy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub claim_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_validator_contract_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_authoritative_status: Option<u16>,
    #[serde(default)]
    pub baseline_sufficient: bool,
    #[serde(default)]
    pub has_embedded_json: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_signature: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed_gone_at_ms: Option<u64>,
}

/// 单条资源在一次同步周期中的变更摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceChangeEntry {
    pub resource_key: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_state: Option<ResourceLifecycleState>,
    pub current_state: ResourceLifecycleState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrated_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_doc_id: Option<String>,
    pub substantive_blocks_changed: usize,
    pub noise_blocks_changed: usize,
    pub strategy_used: String,
    pub not_modified_304: bool,
}

/// 单次同步周期的执行统计。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncTickStats {
    pub discovered_total: usize,
    pub candidates_due: usize,
    pub requests_executed: usize,
    pub browser_renders_used: usize,
    pub conditional_304_hits: usize,
    pub substantive_updates: usize,
    pub noise_only_updates: usize,
    pub unchanged_200_hits: usize,
    pub transient_failures: usize,
    pub access_restricted_count: usize,
    pub deferred_count: usize,
    #[serde(default)]
    pub authoritative_successes: usize,
    #[serde(default)]
    pub attempted_total: usize,
    #[serde(default)]
    pub insufficient_captures: usize,
    #[serde(default)]
    pub unchecked_discovered: usize,
    pub elapsed_ms: u64,
}

/// 一次同步周期产出的结构化增量（`SyncDelta`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncDelta {
    pub source_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instance_id: String,
    pub tick_id: String,
    #[serde(default)]
    pub tick_seq: u64,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    pub coverage_status: String,
    pub coverage_reasons: Vec<String>,
    pub stats: SyncTickStats,
    pub added_resources: Vec<ResourceChangeEntry>,
    pub modified_resources: Vec<ResourceChangeEntry>,
    pub migrated_resources: Vec<ResourceChangeEntry>,
    pub disappeared_resources: Vec<ResourceChangeEntry>,
    pub claim_transitions: Vec<ClaimTransition>,
    pub conflicts: Vec<ConflictRecord>,
    pub adapt_decisions: Vec<serde_json::Value>,
}

/// 数据源规格变更快照（用于 `asOf` 精确回溯当时生效的监控配置）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSpecRevision {
    pub revision: u64,
    pub updated_at_ms: u64,
    pub spec: DataSourceSpec,
}

/// 数据源单次 Tick 完成时的完整库存状态检查点（C03 / C30）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStateCheckpoint {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instance_id: String,
    pub tick_seq: u64,
    pub tick_id: String,
    pub completed_at_ms: u64,
    pub spec_revision: u64,
    pub coverage_status: String,
    pub resources: Vec<ResourceRecord>,
}

/// 数据源生命周期元数据（持久化真实累计 Tick 数，独立于 64 条增量保留窗口）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceMeta {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instance_id: String,
    pub total_ticks: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_tick_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub earliest_retained_checkpoint_at_ms: Option<u64>,
}

/// 历史查询的可用性语义分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceHistoryStatus {
    Complete,
    NotCreatedYet,
    HistoryUnavailable,
}

/// 数据源规格 CAS 更新结果（R02）。
#[derive(Debug, Clone, PartialEq)]
pub enum SpecCasOutcome {
    Updated(DataSourceSpec),
    Conflict { current: DataSourceSpec },
    NotFound,
}

/// 单次同步周期原子提交结果（C05 / C06 / R02）。
#[derive(Debug, Clone, PartialEq)]
pub enum TickCommitOutcome {
    Committed { tick_seq: u64 },
    StaleSpec { current: Box<DataSourceSpec> },
    SourceDeleted,
}

/// 数据源当前（或指定历史时刻 `as_of_ms`）的完整状态视图（`SourceStateView`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStateView {
    #[serde(default)]
    pub source_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<DataSourceSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tick_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at_ms: Option<u64>,
    pub total_ticks: u64,
    #[serde(default)]
    pub retained_ticks: u64,
    #[serde(default = "default_history_status_complete")]
    pub history_status: SourceHistoryStatus,
    #[serde(default)]
    pub history_unavailable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_status: Option<String>,
    pub inventory_counts: BTreeMap<String, usize>,
    pub resources: Vec<ResourceRecord>,
    pub active_claims: Vec<ClaimLifecycleRecord>,
    pub conflicts: Vec<ConflictRecord>,
}

pub fn now_store_ms() -> u64 {
    to_ms(std::time::SystemTime::now())
}
