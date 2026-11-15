//! 图版本+证据+声明+溯源边单事务捆绑提交与历史可用性查询模型。

use pagesift_graph::{
    ClaimLifecycleRecord, ClaimRecord, ClaimTransition, DocNode, ObservationOutcome,
    ProvenanceEdge, VersionRecord,
};
use serde::{Deserialize, Serialize};

use super::history::resolve_forward_in_tx;
use super::{
    GraphStore, PREFIX_CLAIM_LC, PREFIX_PROV, PreparedVersionCommit, VersionCommitOutcome,
    VersionCommitRequest, encode_json, initial_key,
};
use crate::Result;
use crate::engine::{AtomicTx, ns};

/// 历史回溯声明结果，显式暴露是否存在因时间线截断导致的历史盲区。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AsOfClaimsResult {
    pub claims: Vec<ClaimLifecycleRecord>,
    pub history_unavailable: bool,
}

/// 与具体文档观测绑定的时间戳溯源信息，用于历史冲突重放独立源统计（C26 / R06）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredProvenance {
    pub doc_id: String,
    pub observed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<DocNode>,
    #[serde(default)]
    pub edges: Vec<ProvenanceEdge>,
}

/// 版本记录、声明生命周期与溯源关系的捆绑提交请求（C06 / C24）。
pub struct VersionBundleRequest<'a> {
    pub version: Option<VersionCommitRequest<'a>>,
    pub url_or_key: &'a str,
    pub contract_id: &'a str,
    pub observation_outcome: &'a ObservationOutcome,
    pub claims: &'a [ClaimRecord],
    pub observed_at_ms: u64,
    pub doc_id: Option<&'a str>,
    pub doc_node: Option<&'a DocNode>,
    pub provenance: &'a [ProvenanceEdge],
}

/// 捆绑提交结果。
#[derive(Debug, Clone, PartialEq)]
pub enum VersionBundleOutcome {
    Committed {
        snapshot_stored: bool,
        snapshot_warning: Option<String>,
        claims: Vec<ClaimLifecycleRecord>,
        claim_transitions: Vec<ClaimTransition>,
    },
    StaleParent {
        current_latest: VersionRecord,
    },
}

/// 预编译并在无写锁状态下完成快照压缩的页面图提交单元（供 Sync Tick 单事务原子提交复用）。
pub struct PreparedVersionBundle {
    pub(super) url_or_key: String,
    pub(super) version: Option<PreparedVersionCommit>,
    pub(super) contract_id: String,
    pub(super) observation_outcome: ObservationOutcome,
    pub(super) claims: Vec<ClaimRecord>,
    pub(super) observed_at_ms: u64,
    pub(super) doc_id: Option<String>,
    pub(super) prov_bytes: Option<Vec<u8>>,
}

impl PreparedVersionBundle {
    /// Logical resource key targeted by this bundle's version commit, if any.
    pub fn version_resource_key(&self) -> Option<&str> {
        self.version.as_ref().map(|v| v.resource_key.as_str())
    }

    /// Document version ID produced by this bundle's version commit, if any.
    pub fn version_doc_id(&self) -> Option<&str> {
        self.version.as_ref().map(|v| v.doc_id.as_str())
    }
}

impl<'a> GraphStore<'a> {
    /// 在写锁外预处理版本与快照压缩（C06）。
    pub fn prepare_version_bundle(
        &self,
        req: &VersionBundleRequest<'_>,
    ) -> Result<PreparedVersionBundle> {
        let version = req
            .version
            .as_ref()
            .map(|v| self.prepare_version_commit(v))
            .transpose()?;
        let prov_bytes = if let Some(doc_id) = req.doc_id
            && (req.doc_node.is_some() || !req.provenance.is_empty())
        {
            Some(encode_json(
                &StoredProvenance {
                    doc_id: doc_id.to_string(),
                    observed_at_ms: req.observed_at_ms,
                    node: req.doc_node.cloned(),
                    edges: req.provenance.to_vec(),
                },
                "stored provenance",
            )?)
        } else {
            None
        };
        Ok(PreparedVersionBundle {
            url_or_key: req.url_or_key.to_string(),
            version,
            contract_id: req.contract_id.to_string(),
            observation_outcome: req.observation_outcome.clone(),
            claims: req.claims.to_vec(),
            observed_at_ms: req.observed_at_ms,
            doc_id: req.doc_id.map(str::to_string),
            prov_bytes,
        })
    }

    /// 把预先准备好的图版本、声明生命周期与溯源边写入传入的 `AtomicTx`。
    pub(crate) fn stage_prepared_bundle_in_tx(
        &self,
        tx: &mut AtomicTx<'_>,
        prep: &PreparedVersionBundle,
    ) -> Result<VersionBundleOutcome> {
        let (snapshot_stored, snapshot_warning) = if let Some(ref ver) = prep.version {
            match self.stage_prepared_version_in_tx(tx, ver)? {
                VersionCommitOutcome::Committed {
                    snapshot_stored,
                    snapshot_warning,
                } => (snapshot_stored, snapshot_warning),
                VersionCommitOutcome::StaleParent { current_latest } => {
                    return Ok(VersionBundleOutcome::StaleParent { current_latest });
                }
            }
        } else {
            (false, None)
        };

        let canonical =
            resolve_forward_in_tx(tx, self.scope_str(), &initial_key(&prep.url_or_key))?;
        if let Some(ref prov) = prep.prov_bytes
            && let Some(ref doc_id) = prep.doc_id
        {
            let prov_key = self.scoped_compound(PREFIX_PROV, &canonical, doc_id);
            if tx.get(ns::GRAPH_FACT, &prov_key)?.is_none() {
                tx.put(ns::GRAPH_FACT, &prov_key, prov, prep.observed_at_ms)?;
            }
        }

        let claim_key = self.scoped_prefixed(PREFIX_CLAIM_LC, &canonical);
        let prev = match tx.get(ns::GRAPH_FACT, &claim_key)? {
            Some((_, raw)) => super::decode_json(&raw, "claim lifecycle records")?,
            None => Vec::new(),
        };
        let had_prev = !prev.is_empty();
        let (updated, claim_transitions) = pagesift_graph::reconcile_resource_claims_for_contract(
            &prev,
            &prep.observation_outcome,
            &prep.claims,
            prep.observed_at_ms,
            Some(&prep.contract_id),
        );
        if !updated.is_empty() || had_prev {
            let encoded = encode_json(&updated, "claim lifecycle records")?;
            tx.put(ns::GRAPH_FACT, &claim_key, &encoded, prep.observed_at_ms)?;
        }

        Ok(VersionBundleOutcome::Committed {
            snapshot_stored,
            snapshot_warning,
            claims: updated,
            claim_transitions,
        })
    }

    /// 单个 `AtomicTx` 提交版本、快照、别名、契约级声明生命周期及溯源边（C06）。
    pub fn commit_version_bundle(
        &self,
        req: &VersionBundleRequest<'_>,
    ) -> Result<VersionBundleOutcome> {
        let prep = self.prepare_version_bundle(req)?;
        let mut tx = self.store.atomic_tx();
        let outcome = self.stage_prepared_bundle_in_tx(&mut tx, &prep)?;
        if matches!(outcome, VersionBundleOutcome::Committed { .. }) {
            tx.commit()?;
        }
        Ok(outcome)
    }
}
