//! Unit-level delta events, derived-asset dependency validation, adaptive maintenance
//! mode selection, and replay/checkpoint contracts for external index consumers.

use serde::{Deserialize, Serialize};

use crate::context::unit::{KnowledgeUnit, UnitStatus};
use crate::hash::short_id;
use crate::lifecycle::{KnowledgeDelta, ObservationOutcome};

/// Fine-grained transition kind for a [`KnowledgeUnit`], preserving 1.5 observation caution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UnitChangeKind {
    Appeared,
    ContentModified,
    RelocatedOnly,
    Split,
    Merged,
    Reconfirmed,
    EvidenceStateChanged,
    /// Capture encountered 404 pending confirmation, barrier, truncation, or 5xx: not withdrawn.
    ObservationLost,
    ConfirmedWithdrawn,
    Expired,
    AmbiguousLineage,
    SourceAnomaly,
}

/// Single unit transition emitted inside a [`ContextDelta`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitTransition {
    pub unit_id: String,
    pub change_kind: UnitChangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_revision_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curr_revision_id: Option<String>,
    pub resource_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_unit_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_unit_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Categories of derived knowledge assets governed by dependency tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssetKind {
    LexicalIndex,
    StructureIndex,
    GraphIndex,
    SemanticSketch,
    Embedding,
    Summary,
    FieldExtraction,
    PackCache,
}

impl AssetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LexicalIndex => "lexicalIndex",
            Self::StructureIndex => "structureIndex",
            Self::GraphIndex => "graphIndex",
            Self::SemanticSketch => "semanticSketch",
            Self::Embedding => "embedding",
            Self::Summary => "summary",
            Self::FieldExtraction => "fieldExtraction",
            Self::PackCache => "packCache",
        }
    }

    /// Whether physical relocation alone invalidates this derived asset.
    pub fn depends_on_location(self) -> bool {
        matches!(
            self,
            Self::StructureIndex | Self::GraphIndex | Self::FieldExtraction | Self::PackCache
        )
    }

    /// Whether model identifier upgrades invalidate this derived asset.
    pub fn depends_on_model(self) -> bool {
        matches!(
            self,
            Self::Embedding | Self::Summary | Self::FieldExtraction | Self::PackCache
        )
    }

    /// Whether tokenizer/normalization upgrades invalidate this derived asset.
    pub fn depends_on_tokenizer(self) -> bool {
        matches!(
            self,
            Self::LexicalIndex | Self::SemanticSketch | Self::Embedding | Self::PackCache
        )
    }
}

/// Multi-dimensional configuration stamp governing derived-asset compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetSpecStamp {
    pub data_version: u32,
    pub algorithm_version: u32,
    pub tokenizer_version: u32,
    pub model_id: String,
    pub evidence_policy_version: u32,
}

impl Default for AssetSpecStamp {
    fn default() -> Self {
        Self {
            data_version: 1,
            algorithm_version: 1,
            tokenizer_version: 1,
            model_id: "native-cpu-v1".to_owned(),
            evidence_policy_version: 1,
        }
    }
}

impl AssetSpecStamp {
    pub fn fingerprint(&self) -> String {
        let dv = self.data_version.to_string();
        let av = self.algorithm_version.to_string();
        let tv = self.tokenizer_version.to_string();
        let ev = self.evidence_policy_version.to_string();
        short_id("aspec", &[&dv, &av, &tv, &self.model_id, &ev])
    }

    pub fn check_compatible(&self, target: &Self, kind: AssetKind) -> Result<(), String> {
        if self.data_version != target.data_version {
            return Err("data_version_changed".to_owned());
        }
        if self.algorithm_version != target.algorithm_version {
            return Err("algorithm_version_changed".to_owned());
        }
        if kind.depends_on_tokenizer() && self.tokenizer_version != target.tokenizer_version {
            return Err("tokenizer_version_changed".to_owned());
        }
        if kind.depends_on_model() && self.model_id != target.model_id {
            return Err("model_id_changed".to_owned());
        }
        if self.evidence_policy_version != target.evidence_policy_version {
            return Err("evidence_policy_version_changed".to_owned());
        }
        Ok(())
    }
}

/// Dependency record implementing `valid(a, v) = AND_{d in deps(a)} compatible(d, v)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetDependency {
    pub asset_kind: AssetKind,
    pub unit_id: String,
    pub unit_revision_id: String,
    pub content_hash: String,
    pub location_hash: String,
    pub status_tag: String,
    pub spec_stamp: AssetSpecStamp,
}

impl AssetDependency {
    pub fn from_unit(
        asset_kind: AssetKind,
        unit: &KnowledgeUnit,
        spec_stamp: AssetSpecStamp,
    ) -> Self {
        Self {
            asset_kind,
            unit_id: unit.unit_id.clone(),
            unit_revision_id: unit.revision_id.clone(),
            content_hash: unit.content_hash.clone(),
            location_hash: unit.location.location_hash(),
            status_tag: unit.status.tag().to_owned(),
            spec_stamp,
        }
    }

    /// Check whether a previously computed asset remains valid for `unit` and `target_spec`.
    pub fn validate(
        &self,
        unit: &KnowledgeUnit,
        target_spec: &AssetSpecStamp,
    ) -> Result<(), String> {
        if self.unit_id != unit.unit_id {
            return Err("unit_identity_changed".to_owned());
        }
        self.spec_stamp
            .check_compatible(target_spec, self.asset_kind)?;
        if matches!(
            unit.status,
            UnitStatus::Withdrawn { .. }
                | UnitStatus::Superseded { .. }
                | UnitStatus::Expired { .. }
        ) {
            return Err(format!("unit_status_{}", unit.status.tag()));
        }
        if self.content_hash != unit.content_hash {
            return Err("content_modified".to_owned());
        }
        if self.asset_kind.depends_on_location() {
            if self.location_hash != unit.location.location_hash() {
                return Err("location_or_structure_changed".to_owned());
            }
            if self.unit_revision_id != unit.revision_id {
                return Err("revision_claims_or_temporal_changed".to_owned());
            }
            if self.status_tag != unit.status.tag() {
                return Err("evidence_status_changed".to_owned());
            }
        } else if matches!(self.asset_kind, AssetKind::Summary) {
            if self.unit_revision_id != unit.revision_id {
                return Err("revision_claims_or_temporal_changed".to_owned());
            }
            if self.status_tag != unit.status.tag() {
                return Err("evidence_status_changed".to_owned());
            }
        }
        Ok(())
    }
}

/// Explicit record explaining why a derived asset was invalidated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetInvalidation {
    pub asset_kind: AssetKind,
    pub unit_id: String,
    pub reason: String,
}

/// Adaptive decision between localized incremental patching and compact full index rebuild.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum MaintenanceMode {
    Incremental,
    FullRebuild { reason: String },
}

/// Cost-boundary decision `C_incremental(delta_U, tombstones) < C_rebuild(U)`.
pub fn choose_maintenance_mode(
    live_units: usize,
    changed_or_removed_units: usize,
    accumulated_tombstones: usize,
) -> MaintenanceMode {
    // [策略] 至少 32 个存量单元才按变更比例触发全量重建；小型源单文档局部修改直接走增量更新
    const MIN_REBUILD_CORPUS_UNITS: usize = 32;
    // [推导] 倒排表局部删改需读改写词项链表（约 2.4x 单单元正向追加成本），变更率 >= 0.42 时重建更省
    const REBUILD_CHANGE_RATIO: f64 = 0.42;
    // [策略] 累积失效墓碑占比 >= 0.30 时执行紧凑重建回收碎片
    const REBUILD_TOMBSTONE_RATIO: f64 = 0.30;

    if live_units == 0 {
        return MaintenanceMode::FullRebuild {
            reason: "initial_index_build".to_owned(),
        };
    }
    if changed_or_removed_units >= live_units {
        return MaintenanceMode::FullRebuild {
            reason: "full_corpus_replacement".to_owned(),
        };
    }
    if live_units >= MIN_REBUILD_CORPUS_UNITS {
        let change_ratio = changed_or_removed_units as f64 / live_units as f64;
        if change_ratio >= REBUILD_CHANGE_RATIO {
            return MaintenanceMode::FullRebuild {
                reason: format!("high_churn_ratio:{change_ratio:.3}"),
            };
        }
    }
    let total_slots = live_units.saturating_add(accumulated_tombstones).max(1);
    let tomb_ratio = accumulated_tombstones as f64 / total_slots as f64;
    if tomb_ratio >= REBUILD_TOMBSTONE_RATIO {
        return MaintenanceMode::FullRebuild {
            reason: format!("tombstone_fragmentation:{tomb_ratio:.3}"),
        };
    }
    MaintenanceMode::Incremental
}

/// Explicit visibility report answering which `Store` version the Context index reflects right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexVisibility {
    pub source_id: String,
    pub instance_id: String,
    pub store_tick_seq: u64,
    pub indexed_tick_seq: u64,
    pub delta_seq: u64,
    pub lag_ticks: u64,
    pub instance_matched: bool,
    pub is_caught_up: bool,
}

/// Replayable unit-level delta event persisted with ABA-safe `instance_id` and monotonic `delta_seq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDelta {
    pub source_id: String,
    pub instance_id: String,
    pub delta_seq: u64,
    pub store_tick_seq: u64,
    pub created_at_ms: u64,
    pub observation_outcome: ObservationOutcome,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unit_transitions: Vec<UnitTransition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invalidated_assets: Vec<AssetInvalidation>,
    pub reused_asset_count: usize,
    pub recomputed_asset_count: usize,
    pub maintenance_mode: MaintenanceMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_delta: Option<KnowledgeDelta>,
}

/// External index consumer checkpoint for at-least-once idempotent delivery and crash recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerCheckpoint {
    pub consumer_id: String,
    pub source_id: String,
    pub instance_id: String,
    pub acked_delta_seq: u64,
    pub spec_fingerprint: String,
    pub updated_at_ms: u64,
}

/// Outcome of polling deltas for an external vector/search consumer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ReplayStatus {
    UpToDate,
    IncrementalReady,
    FullResyncRequired { reason: String },
}

/// Response returned when an external consumer polls incremental [`ContextDelta`] batches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDeltaPollResult {
    pub status: ReplayStatus,
    pub source_id: String,
    pub current_instance_id: String,
    pub latest_delta_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<ConsumerCheckpoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deltas: Vec<ContextDelta>,
}
