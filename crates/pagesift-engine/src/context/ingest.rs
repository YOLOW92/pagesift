//! Direct multi-format Context document ingestion (`ingest_documents`) & observation status mapping
//! running on `pagesift-flow::Pool`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use pagesift_api::types::Document;
use pagesift_api::{ApiError, ErrorCode};
use pagesift_extract::context::{
    ContextIndex, UnitBuildInput, extract_units_from_document, reconcile_resource_units,
};
use pagesift_graph::context::{AssetSpecStamp, ContextDelta, KnowledgeUnit, UnitMatchMode};
use pagesift_graph::{
    DocNode, ObservationOutcome, SourceKind, SubjectKey, SubjectKeySource, TemporalSemantics,
    host_of, normalize_resource_key, sha256_hex,
};
use pagesift_store::{ContextCommitOutcome, ContextCommitRequest, now_store_ms};
use serde::{Deserialize, Serialize};

pub use crate::context::corpus::{LoadedContextCorpus, load_corpus};
pub use crate::context::sync_reconcile::sync_source_context;
use crate::engine::{Scraper, cpu_task_keyed};
use crate::sync::capture::{assess_capture_reliability, is_barrier_doc};

// [协议] FNV-1a 64 位标准初始偏移基数（用于将 source_id 映射到固定 CPU 工人亲和槽）
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
// [协议] FNV-1a 64 位标准素数乘子
const FNV_PRIME: u64 = 0x0100_0000_01b3;
// [协议] RFC 9110 §15.4.5：HTTP 304 Not Modified
const HTTP_NOT_MODIFIED: u16 = 304;
// [协议] RFC 9110 §15.5/§15.6：HTTP 客户端/服务端错误状态码下界
const HTTP_ERROR_MIN: u16 = 400;
// [协议] RFC 9110 §15.5.4：HTTP 403 Forbidden
const HTTP_FORBIDDEN: u16 = 403;
// [协议] RFC 9110 §15.5.5：HTTP 404 Not Found
const HTTP_NOT_FOUND: u16 = 404;
// [协议] RFC 9110 §15.5.11：HTTP 410 Gone
const HTTP_GONE: u16 = 410;
// [协议] RFC 9110 §15.5.22：HTTP 429 Too Many Requests
const HTTP_TOO_MANY_REQUESTS: u16 = 429;

/// Inline or pre-parsed document item for direct Context ingestion or ephemeral compilation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDocumentInput {
    pub url: String,
    pub markdown: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub source_kind: Option<SourceKind>,
    #[serde(default)]
    pub observed_at_ms: Option<u64>,
    #[serde(default)]
    pub valid_from: Option<String>,
    #[serde(default)]
    pub valid_until: Option<String>,
    #[serde(default)]
    pub subject_key: Option<String>,
    #[serde(default)]
    pub origin_root: Option<String>,
    #[serde(default = "default_true")]
    pub origin_independent: bool,
    #[serde(default)]
    pub status_code: Option<u16>,
    #[serde(default)]
    pub unreliable: bool,
    #[serde(default)]
    pub confirmed_gone: bool,
}

fn default_true() -> bool {
    true
}

impl ContextDocumentInput {
    pub fn from_api_document(doc: &Document, fallback_url: &str) -> Self {
        let url = doc
            .metadata
            .url
            .clone()
            .or_else(|| doc.metadata.source_url.clone())
            .unwrap_or_else(|| fallback_url.to_owned());
        let markdown = doc.markdown.clone().unwrap_or_default();
        let rel = assess_capture_reliability(doc);
        let not_mod = doc
            .metadata
            .pagesift
            .as_ref()
            .is_some_and(|p| p.not_modified)
            && !rel.invalid_304;
        let status_code = if not_mod {
            // [协议] RFC 9110 §15.4.5：304 Not Modified 表示源内容未变，不得误判为空正文撤回
            304
        } else {
            doc.metadata.status_code
        };
        let unreliable = rel.is_unreliable() || is_barrier_doc(doc);
        let source_kind = doc.graph.as_ref().map(|g| g.source_kind.clone());
        let observed = doc.graph.as_ref().map(|g| g.observed_at_ms);
        let (valid_from, valid_until) = doc
            .graph
            .as_ref()
            .and_then(|g| g.temporal.as_ref())
            .map(|t| (t.valid_from.clone(), t.valid_until.clone()))
            .unwrap_or((None, None));
        let subj = doc.graph.as_ref().map(|g| g.subject_key.key.clone());

        Self {
            url,
            markdown,
            title: doc.metadata.title.clone(),
            source_kind,
            observed_at_ms: observed,
            valid_from,
            valid_until,
            subject_key: subj,
            origin_root: None,
            origin_independent: true,
            status_code: Some(status_code),
            unreliable,
            // [协议] RFC 9110 §15.5.11：仅 HTTP 410 Gone 确认永久下线；404 保持 false 以走 SuspectedGone 谨慎降级
            confirmed_gone: matches!(status_code, 410),
        }
    }
}

pub fn affinity_key(label: &str) -> u64 {
    let mut h = FNV_OFFSET;
    for &b in label.as_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

pub(crate) fn summarize_batch_outcome(outcomes: &[ObservationOutcome]) -> ObservationOutcome {
    if outcomes.is_empty() {
        return ObservationOutcome::NotModified304;
    }
    if outcomes
        .iter()
        .any(|o| matches!(o, ObservationOutcome::VerifiedContent))
    {
        return ObservationOutcome::VerifiedContent;
    }
    if outcomes
        .iter()
        .all(|o| matches!(o, ObservationOutcome::NotModified304))
    {
        return ObservationOutcome::NotModified304;
    }
    outcomes[0].clone()
}

/// Ingest explicit documents (HTML, PDF, Office, Code, or inline Markdown) into persistent `ContextStore`
/// or build an ephemeral in-memory corpus when `persist` is false.
pub async fn ingest_documents(
    scraper: &Scraper,
    tenant: Option<&str>,
    source_id: &str,
    docs: Vec<ContextDocumentInput>,
    match_mode: UnitMatchMode,
    spec_override: Option<AssetSpecStamp>,
    persist: bool,
) -> Result<(LoadedContextCorpus, Option<ContextDelta>), ApiError> {
    let target_spec = spec_override.unwrap_or_default();
    let sid = source_id.to_owned();
    let tenant_owned = tenant.map(str::to_owned);

    let _src_guard = if persist {
        Some(
            scraper
                .context_source_lock(tenant, source_id)
                .lock_owned()
                .await,
        )
    } else {
        None
    };

    let (mut index, mut prev_by_res, instance_id, next_tick) = if persist {
        let store = scraper.store_ref().cloned().ok_or_else(|| {
            ApiError::invalid("persistent context ingest requires store configured on Scraper")
        })?;
        let t_read = tenant_owned.clone();
        let sid_read = sid.clone();
        let spec_read = target_spec.clone();
        let res_keys: BTreeSet<String> = docs
            .iter()
            .map(|d| normalize_resource_key(&d.url))
            .collect();
        let cached_probe = scraper.context_cache().peek(tenant, &sid);
        store
            .blocking(move |st| -> Result<_, ApiError> {
                let ctx_st = st.context_for(t_read.as_deref());
                let sync_st = st.sync_for(t_read.as_deref());
                let sync_spec = sync_st
                    .get_source(&sid_read)
                    .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
                let meta = ctx_st
                    .get_meta(&sid_read)
                    .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
                let live_meta = match &meta {
                    Some(m) if ctx_st.is_meta_live(m).unwrap_or(false) => Some(m),
                    _ => None,
                };
                let (inst, tick) = match &sync_spec {
                    Some(spec) => {
                        let vis = ctx_st
                            .visibility(&sid_read)
                            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
                        // Managed Loop source: bind to `spec.instance_id` and preserve `vis.store_tick_seq`
                        // so `indexed_tick_seq` never jumps ahead of `SyncStore.total_ticks`!
                        (spec.instance_id.clone(), vis.store_tick_seq)
                    }
                    None => {
                        let i = live_meta
                            .map(|m| m.instance_id.clone())
                            .unwrap_or_else(|| format!("ctx-manual-{sid_read}"));
                        let t = live_meta
                            .map(|m| m.indexed_tick_seq.saturating_add(1))
                            .unwrap_or(1);
                        (i, t)
                    }
                };
                let idx: ContextIndex =
                    if let (Some(m), Some((c_inst, c_dseq, c_tick, c_spec, arc_idx))) =
                        (live_meta, cached_probe)
                        && c_inst == m.instance_id
                        && c_dseq == m.latest_delta_seq
                        && c_tick == m.indexed_tick_seq
                        && c_spec == m.spec_stamp
                    {
                        (*arc_idx).clone()
                    } else if let Some(m) = live_meta {
                        let loaded_idx = match ctx_st.load_index_blob(&sid_read) {
                            Ok(Some(raw)) => match serde_json::from_slice::<ContextIndex>(&raw) {
                                Ok(parsed) => Some(parsed),
                                Err(e) => {
                                    tracing::warn!(
                                        source_id = %sid_read,
                                        error = %e,
                                        "corrupt context index JSON before ingest; rebuilding baseline from authoritative units"
                                    );
                                    None
                                }
                            },
                            Ok(None) => None,
                            Err(e) => {
                                tracing::warn!(
                                    source_id = %sid_read,
                                    error = %e,
                                    "corrupt context index blob before ingest; rebuilding baseline from authoritative units"
                                );
                                None
                            }
                        };
                        match loaded_idx {
                            Some(parsed) => parsed,
                            None => {
                                let mut rebuilt = ContextIndex::new(m.spec_stamp.clone());
                                if !m.resource_keys.is_empty() {
                                    let all_units = ctx_st
                                        .load_all_live_units(&sid_read)
                                        .map_err(|e| {
                                            ApiError::new(ErrorCode::Internal, e.to_string())
                                        })?;
                                    if !all_units.is_empty() {
                                        rebuilt.apply_updates(&all_units, &[], &m.spec_stamp);
                                    }
                                }
                                rebuilt
                            }
                        }
                    } else {
                        ContextIndex::new(spec_read)
                    };
                let mut prev = BTreeMap::new();
                if live_meta.is_some() {
                    for rk in res_keys {
                        let u = ctx_st
                            .load_resource_units(&sid_read, &rk)
                            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
                        prev.insert(rk, u);
                    }
                }
                Ok((idx, prev, inst, tick))
            })
            .await?
    } else {
        (
            ContextIndex::new(target_spec.clone()),
            BTreeMap::new(),
            "ephemeral-0".to_owned(),
            0u64,
        )
    };

    let key = affinity_key(&sid);
    let sid_cpu = sid.clone();
    let inst_cpu = instance_id.clone();
    let spec_cpu = target_spec.clone();

    let (index_arc, res_map, retired, delta) = cpu_task_keyed(key, move || {
        let now = now_store_ms();
        let mut res_map: BTreeMap<String, Vec<KnowledgeUnit>> = BTreeMap::new();
        let mut retired = Vec::new();
        let mut transitions = Vec::new();
        let mut seen_outcomes = Vec::new();

        for d in docs {
            let rk = normalize_resource_key(&d.url);
            // Chain sequential updates if the same normalized URL appears multiple times in `docs`!
            let prev_u = res_map
                .remove(&rk)
                .or_else(|| prev_by_res.remove(&rk))
                .unwrap_or_default();
            let obs_ms = d.observed_at_ms.unwrap_or(now);

            let outcome = if d.confirmed_gone || matches!(d.status_code, Some(HTTP_GONE)) {
                ObservationOutcome::ConfirmedGone {
                    reason: "document_confirmed_gone".to_owned(),
                }
            } else if matches!(d.status_code, Some(HTTP_NOT_MODIFIED)) && !d.unreliable {
                ObservationOutcome::NotModified304
            } else if d.unreliable
                || matches!(d.status_code, Some(HTTP_FORBIDDEN | HTTP_TOO_MANY_REQUESTS))
            {
                ObservationOutcome::SuspectEmptyOrBarrier {
                    reason: "document_marked_unreliable".to_owned(),
                }
            } else if matches!(d.status_code, Some(HTTP_NOT_FOUND)) {
                ObservationOutcome::SuspectedGone {
                    status_code: Some(HTTP_NOT_FOUND),
                }
            } else if let Some(code) = d.status_code.filter(|&c| c >= HTTP_ERROR_MIN) {
                ObservationOutcome::TransientFetchError {
                    error: format!("http_status_{code}"),
                }
            } else {
                ObservationOutcome::VerifiedContent
            };

            let fresh = if matches!(outcome, ObservationOutcome::VerifiedContent) {
                let ch = sha256_hex(d.markdown.as_bytes());
                let doc_id = format!("doc_{}", &ch[..16]);
                let node = DocNode {
                    doc_id,
                    resource_key: rk.clone(),
                    requested_url: d.url.clone(),
                    final_url: d.url.clone(),
                    canonical_url: None,
                    source_kind: d.source_kind.unwrap_or(SourceKind::Html),
                    observed_at_ms: obs_ms,
                    status_code: d.status_code.unwrap_or(200), // [协议] HTTP 200 OK 默认状态码
                    engine: "ingest".to_owned(),
                    content_hash: Some(ch),
                    raw_hash: None,
                    text_hash: None,
                    etag: None,
                    last_modified: None,
                    title: d.title,
                };
                let temp = TemporalSemantics {
                    observed_at_ms: obs_ms,
                    published_at: None,
                    published_at_ms: None,
                    modified_at: None,
                    modified_at_ms: None,
                    valid_from: d.valid_from,
                    valid_until: d.valid_until,
                };
                let sk = d.subject_key.map(|k| SubjectKey {
                    key: k.clone(),
                    source: SubjectKeySource::ExplicitConfig,
                    raw: k,
                });
                let root = d.origin_root.unwrap_or_else(|| {
                    let h = host_of(&d.url);
                    if h.is_empty() { rk.clone() } else { h }
                });
                extract_units_from_document(&UnitBuildInput {
                    doc_node: &node,
                    markdown: &d.markdown,
                    subject_key: sk.as_ref(),
                    temporal: Some(&temp),
                    blocks: &[],
                    claims: &[],
                    evidence: &[],
                    origin_root: &root,
                    origin_independent: d.origin_independent,
                    max_table_unit_tokens: 512, // [策略] 默认大表格行分片软上限 512 tokens
                })
            } else {
                Vec::new()
            };

            let rec = reconcile_resource_units(
                &prev_u,
                fresh,
                &outcome,
                d.unreliable,
                obs_ms,
                match_mode,
            );
            seen_outcomes.push(outcome);
            res_map.insert(rk, rec.live_units);
            retired.extend(rec.retired_units);
            transitions.extend(rec.transitions);
        }

        // Derive final `upserts` & `ret_ids` strictly from terminal `res_map` so duplicate URLs in a batch leave zero ghosts!
        let upserts: Vec<KnowledgeUnit> = res_map.values().flatten().cloned().collect();
        let final_live_ids: BTreeSet<&str> = upserts.iter().map(|u| u.unit_id.as_str()).collect();
        let mut ret_id_set: BTreeSet<String> = BTreeSet::new();
        for u in &retired {
            if !final_live_ids.contains(u.unit_id.as_str()) {
                ret_id_set.insert(u.unit_id.clone());
            }
        }
        let ret_ids: Vec<String> = ret_id_set.into_iter().collect();

        let sum = index.apply_updates(&upserts, &ret_ids, &spec_cpu);
        let batch_outcome = summarize_batch_outcome(&seen_outcomes);
        let delta = ContextDelta {
            source_id: sid_cpu,
            instance_id: inst_cpu,
            delta_seq: 0,
            store_tick_seq: next_tick,
            created_at_ms: now,
            observation_outcome: batch_outcome,
            unit_transitions: transitions,
            invalidated_assets: sum.invalidated_assets,
            reused_asset_count: sum.reused_asset_count,
            recomputed_asset_count: sum.recomputed_asset_count,
            maintenance_mode: sum.mode,
            knowledge_delta: None,
        };
        (Arc::new(index), res_map, retired, delta)
    })
    .await?;

    if !persist {
        return Ok((
            LoadedContextCorpus {
                index: index_arc,
                conflicts: Vec::new(),
                fact_clusters: Vec::new(),
                visibilities: Vec::new(),
                warnings: Vec::new(),
            },
            Some(delta),
        ));
    }

    let store = scraper.store_ref().cloned().unwrap();
    let idx_bytes = serde_json::to_vec(&*index_arc)
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("serialize context index: {e}")))?;
    let t_commit = tenant_owned.clone();
    let sid_commit = sid.clone();
    let (saved_delta, vis, saved_meta_opt) = store
        .blocking(move |st| -> Result<_, ApiError> {
            let ctx_st = st.context_for(t_commit.as_deref());
            let mut d_copy = delta.clone();
            let out = ctx_st
                .commit_delta(ContextCommitRequest {
                    source_id: &sid_commit,
                    instance_id: &instance_id,
                    store_tick_seq: next_tick,
                    spec_stamp: target_spec,
                    resource_units: &res_map,
                    retired_units: &retired,
                    index_blob: Some(&idx_bytes),
                    delta,
                    allow_same_zero_tick: true,
                })
                .map_err(|e| ApiError::new(ErrorCode::Internal, format!("commit ingest: {e}")))?;
            let (seq, committed) = match out {
                ContextCommitOutcome::Committed { delta_seq }
                | ContextCommitOutcome::InstanceReplacedAndCommitted { delta_seq } => {
                    (delta_seq, true)
                }
                ContextCommitOutcome::IdempotentReplaySkipped { existing_delta_seq } => {
                    (existing_delta_seq, false)
                }
            };
            d_copy.delta_seq = seq;
            let vis = ctx_st
                .visibility(&sid_commit)
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
            let saved_meta = if committed {
                ctx_st.get_meta(&sid_commit).ok().flatten()
            } else {
                None
            };
            Ok((d_copy, vis, saved_meta))
        })
        .await?;

    if let Some(meta) = saved_meta_opt {
        scraper
            .context_cache()
            .put(tenant, &meta, index_arc.clone());
    }

    Ok((
        LoadedContextCorpus {
            index: index_arc,
            conflicts: Vec::new(),
            fact_clusters: Vec::new(),
            visibilities: vec![vis],
            warnings: Vec::new(),
        },
        Some(saved_delta),
    ))
}
