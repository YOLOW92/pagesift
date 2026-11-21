//! Unified 1.6 Context dispatcher shared across Rust `Scraper`, HTTP `/v2/context`, CLI, MCP,
//! Python, Node.js, and C FFI.

pub mod action;
pub(crate) mod action_support;
pub(crate) mod cache;
pub mod corpus;
pub mod ingest;
pub mod sync_reconcile;
pub(crate) mod validate;

use std::sync::Arc;
use std::time::Instant;

use pagesift_api::{ApiError, ErrorCode, PageBudget};
use pagesift_extract::context::compile_context_pack;
use pagesift_graph::context::ConsumerCheckpoint;
use pagesift_store::DEFAULT_DELTA_POLL_LIMIT;
use serde_json::{Map, Value, json};

pub use action::{
    execute_action, execute_action_with_budget, load_learn_state, load_learn_state_with_warning,
    save_learn_state, try_load_learn_state,
};
pub use ingest::{
    ContextDocumentInput, LoadedContextCorpus, affinity_key, ingest_documents, load_corpus,
    sync_source_context,
};
use validate::{
    MAX_CONTEXT_RESULT_LIMIT, build_action_request, build_pack_request, build_search_request,
    normalize_keys, opt_bool, opt_f32_bounded, opt_ident, opt_usize_bounded, parse_drift_guard,
    parse_inline_docs, parse_match_mode, parse_spec_stamp, req_ident, req_u64, validate_ident_str,
};

use crate::engine::{Scraper, cpu_task_keyed};

/// Unified JSON-in / JSON-out dispatcher supporting `snake_case` and `camelCase` keys.
pub async fn dispatch(
    scraper: &Scraper,
    tenant: Option<&str>,
    raw_request: Value,
) -> Result<Value, ApiError> {
    dispatch_with_budget(scraper, tenant, raw_request, None).await
}

/// Unified JSON-in / JSON-out dispatcher with optional caller-level daily page quota budget
/// enforced on acquisition actions (`action` / `research` / `verify` / `watch`).
pub async fn dispatch_with_budget(
    scraper: &Scraper,
    tenant: Option<&str>,
    raw_request: Value,
    page_budget: Option<Arc<dyn PageBudget>>,
) -> Result<Value, ApiError> {
    let obj = normalize_keys(raw_request)?;
    let action_field = match obj.get("action") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.trim().to_ascii_lowercase()),
        Some(_) => return Err(ApiError::invalid("'action' must be a string")),
    };
    let op_field = match obj.get("op") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.trim().to_ascii_lowercase()),
        Some(_) => return Err(ApiError::invalid("'op' must be a string")),
    };
    if let (Some(a), Some(o)) = (&action_field, &op_field)
        && a != o
    {
        return Err(ApiError::invalid(format!(
            "conflicting 'action' ('{a}') and 'op' ('{o}') fields in context request"
        )));
    }
    let op = action_field
        .or(op_field)
        .unwrap_or_else(|| "pack".to_owned());

    match op.as_str() {
        "ingest" => handle_ingest(scraper, tenant, &obj).await,
        "sync" => handle_sync(scraper, tenant, &obj).await,
        "search" => handle_search(scraper, tenant, &obj).await,
        "pack" => handle_pack(scraper, tenant, &obj).await,
        "action" | "research" | "verify" | "watch" | "changes" => {
            handle_action(scraper, tenant, &op, &obj, page_budget).await
        }
        "units" => handle_units(scraper, tenant, &obj).await,
        "deltas" | "deltas_poll" | "deltaspoll" => handle_deltas_poll(scraper, tenant, &obj).await,
        "deltas_ack" | "deltasack" | "ack" => handle_deltas_ack(scraper, tenant, &obj).await,
        "learn_status" | "learnstatus" => handle_learn(scraper, tenant, &obj, true).await,
        "learn" => handle_learn(scraper, tenant, &obj, false).await,
        other => Err(ApiError::invalid(format!(
            "unsupported context operation '{other}'; expected ingest, sync, search, pack, action, research, verify, watch, changes, units, deltas, ack, or learn"
        ))),
    }
}

async fn handle_ingest(
    scraper: &Scraper,
    tenant: Option<&str>,
    obj: &Map<String, Value>,
) -> Result<Value, ApiError> {
    let source_id = opt_ident(obj, "sourceId", Some("id"))?.unwrap_or_else(|| "default".to_owned());
    let persist = opt_bool(obj, "persist")?.unwrap_or_else(|| scraper.store_ref().is_some());
    let match_mode = parse_match_mode(obj.get("matchMode"))?;
    let spec = parse_spec_stamp(obj.get("specStamp"))?;
    let docs = parse_inline_docs(obj.get("documents"))?;
    if docs.is_empty() {
        return Err(ApiError::invalid(
            "context ingest requires a non-empty 'documents' array",
        ));
    }
    let (corpus, delta) =
        ingest_documents(scraper, tenant, &source_id, docs, match_mode, spec, persist).await?;
    Ok(json!({
        "success": true,
        "sourceId": source_id,
        "persisted": persist,
        "liveUnitCount": corpus.index.units.len(),
        "visibilities": corpus.visibilities,
        "delta": delta,
    }))
}

async fn handle_sync(
    scraper: &Scraper,
    tenant: Option<&str>,
    obj: &Map<String, Value>,
) -> Result<Value, ApiError> {
    let source_id = req_ident(obj, "sourceId", Some("id"))?;
    let match_mode = parse_match_mode(obj.get("matchMode"))?;
    let spec = parse_spec_stamp(obj.get("specStamp"))?;
    let (delta, vis) = sync_source_context(scraper, tenant, &source_id, match_mode, spec).await?;
    Ok(json!({
        "success": true,
        "sourceId": source_id,
        "visibility": vis,
        "delta": delta,
    }))
}

async fn handle_search(
    scraper: &Scraper,
    tenant: Option<&str>,
    obj: &Map<String, Value>,
) -> Result<Value, ApiError> {
    let sreq = build_search_request(obj, true)?;
    let inline_docs = parse_inline_docs(obj.get("documents"))?;
    let match_mode = parse_match_mode(obj.get("matchMode"))?;

    let corpus = if !inline_docs.is_empty() {
        ingest_documents(
            scraper,
            tenant,
            "ephemeral",
            inline_docs,
            match_mode,
            None,
            false,
        )
        .await?
        .0
    } else {
        load_corpus(scraper, tenant, &sreq.source_ids, sreq.as_of_ms).await?
    };

    let (mut learn, learn_warn) = load_learn_state_with_warning(scraper, tenant).await;
    let key = affinity_key(&sreq.query);
    let idx = corpus.index;
    let conflicts = corpus.conflicts;
    let clusters = corpus.fact_clusters;
    let vis = corpus.visibilities;
    let mut extra_warnings = corpus.warnings;
    if let Some(w) = learn_warn {
        extra_warnings.push(w);
    }
    let report = cpu_task_keyed(key, move || {
        let mut r = idx.search(&sreq, &conflicts, &clusters, vis, &mut learn);
        r.warnings.extend(extra_warnings);
        r
    })
    .await?;

    Ok(json!({
        "success": true,
        "report": report,
    }))
}

async fn handle_pack(
    scraper: &Scraper,
    tenant: Option<&str>,
    obj: &Map<String, Value>,
) -> Result<Value, ApiError> {
    let preq = build_pack_request(obj, true)?;
    let inline_docs = parse_inline_docs(obj.get("documents"))?;
    let match_mode = parse_match_mode(obj.get("matchMode"))?;

    let corpus = if !inline_docs.is_empty() {
        ingest_documents(
            scraper,
            tenant,
            "ephemeral",
            inline_docs,
            match_mode,
            None,
            false,
        )
        .await?
        .0
    } else {
        load_corpus(
            scraper,
            tenant,
            &preq.search.source_ids,
            preq.search.as_of_ms,
        )
        .await?
    };

    let (mut learn, learn_warn) = load_learn_state_with_warning(scraper, tenant).await;
    let key = affinity_key(&preq.search.query);
    let idx = corpus.index;
    let conflicts = corpus.conflicts;
    let clusters = corpus.fact_clusters;
    let vis = corpus.visibilities;
    let mut extra_warnings = corpus.warnings;
    if let Some(w) = learn_warn {
        extra_warnings.push(w);
    }
    let pack = cpu_task_keyed(key, move || {
        let t0 = Instant::now();
        let mut report = idx.search(&preq.search, &conflicts, &clusters, vis, &mut learn);
        report.warnings.extend(extra_warnings);
        let search_us = t0.elapsed().as_micros() as u64;
        compile_context_pack(&preq, &report, search_us)
    })
    .await?;

    Ok(json!({
        "success": true,
        "pack": pack,
    }))
}

async fn handle_action(
    scraper: &Scraper,
    tenant: Option<&str>,
    op: &str,
    obj: &Map<String, Value>,
    page_budget: Option<Arc<dyn PageBudget>>,
) -> Result<Value, ApiError> {
    let areq = build_action_request(op, obj)?;
    let inline_docs = parse_inline_docs(obj.get("documents"))?;
    let match_mode = parse_match_mode(obj.get("matchMode"))?;
    let outcome =
        execute_action_with_budget(scraper, tenant, areq, inline_docs, match_mode, page_budget)
            .await?;
    Ok(json!({
        "success": true,
        "outcome": outcome,
    }))
}

async fn handle_units(
    scraper: &Scraper,
    tenant: Option<&str>,
    obj: &Map<String, Value>,
) -> Result<Value, ApiError> {
    let store = scraper
        .store_ref()
        .cloned()
        .ok_or_else(|| ApiError::invalid("listing persisted units requires store configured"))?;
    let source_id = req_ident(obj, "sourceId", Some("id"))?;
    let resource_url = match obj.get("url") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_owned())
        }
        Some(_) => return Err(ApiError::invalid("'url' must be a string")),
    };
    // [策略] 单次 `units` 列表查询默认最多返回 100 条活跃知识单元（兼顾分页可读性与响应体积）
    let limit = opt_usize_bounded(obj, "limit", 1, MAX_CONTEXT_RESULT_LIMIT)?.unwrap_or(100);
    let as_of_ms = validate::parse_opt_time(obj.get("asOf").or_else(|| obj.get("asOfMs")), "asOf")?;
    let t_owned = tenant.map(str::to_owned);

    store
        .blocking(move |st| -> Result<Value, ApiError> {
            let ctx_st = st.context_for(t_owned.as_deref());
            let vis = ctx_st
                .visibility(&source_id)
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
            let (total, units) = match (as_of_ms, resource_url) {
                (Some(cutoff), opt_u) => {
                    let target_rk = opt_u.map(|u| pagesift_graph::normalize_resource_key(&u));
                    let (_, total, hist) = ctx_st
                        .load_units_as_of_filtered(&source_id, cutoff, target_rk.as_deref(), limit)
                        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
                    (total, hist)
                }
                (None, Some(u)) => {
                    let mut list = ctx_st
                        .load_resource_units(
                            &source_id,
                            &pagesift_graph::normalize_resource_key(&u),
                        )
                        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
                    let total = list.len();
                    list.truncate(limit);
                    (total, list)
                }
                (None, None) => ctx_st
                    .load_live_units_limited(&source_id, limit)
                    .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?,
            };
            Ok(json!({
                "success": true,
                "sourceId": source_id,
                "total": total,
                "visibility": vis,
                "units": units,
            }))
        })
        .await
}

async fn handle_deltas_poll(
    scraper: &Scraper,
    tenant: Option<&str>,
    obj: &Map<String, Value>,
) -> Result<Value, ApiError> {
    let store = scraper
        .store_ref()
        .cloned()
        .ok_or_else(|| ApiError::invalid("polling context deltas requires store configured"))?;
    let source_id = req_ident(obj, "sourceId", Some("id"))?;
    let consumer_id =
        opt_ident(obj, "consumerId", None)?.unwrap_or_else(|| "default-consumer".to_owned());
    let spec_fp = match obj.get("specFingerprint") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_owned())
        }
        Some(_) => return Err(ApiError::invalid("'specFingerprint' must be a string")),
    };
    let limit = opt_usize_bounded(obj, "limit", 1, MAX_CONTEXT_RESULT_LIMIT)?
        .unwrap_or(DEFAULT_DELTA_POLL_LIMIT);
    let t_owned = tenant.map(str::to_owned);

    store
        .blocking(move |st| -> Result<Value, ApiError> {
            let poll = st
                .context_for(t_owned.as_deref())
                .poll_consumer_deltas(&consumer_id, &source_id, spec_fp.as_deref(), limit)
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
            Ok(json!({
                "success": true,
                "poll": poll,
            }))
        })
        .await
}

async fn handle_deltas_ack(
    scraper: &Scraper,
    tenant: Option<&str>,
    obj: &Map<String, Value>,
) -> Result<Value, ApiError> {
    let store = scraper.store_ref().cloned().ok_or_else(|| {
        ApiError::invalid("acknowledging context deltas requires store configured")
    })?;
    let source_id = req_ident(obj, "sourceId", Some("id"))?;
    let consumer_id = req_ident(obj, "consumerId", None)?;
    let instance_id = req_ident(obj, "instanceId", None)?;
    let acked_seq = req_u64(obj, "ackedDeltaSeq")?;
    let spec_fp = match obj.get("specFingerprint") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => {
            let t = s.trim();
            if !t.is_empty() {
                validate_ident_str(t, "specFingerprint")?
            } else {
                String::new()
            }
        }
        Some(_) => return Err(ApiError::invalid("'specFingerprint' must be a string")),
    };
    let t_owned = tenant.map(str::to_owned);

    store
        .blocking(move |st| -> Result<Value, ApiError> {
            let saved = st
                .context_for(t_owned.as_deref())
                .ack_consumer_checkpoint(ConsumerCheckpoint {
                    consumer_id,
                    source_id,
                    instance_id,
                    acked_delta_seq: acked_seq,
                    spec_fingerprint: spec_fp,
                    updated_at_ms: 0,
                })
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
            Ok(json!({
                "success": true,
                "checkpoint": saved,
            }))
        })
        .await
}

async fn handle_learn(
    scraper: &Scraper,
    tenant: Option<&str>,
    obj: &Map<String, Value>,
    read_only: bool,
) -> Result<Value, ApiError> {
    if read_only
        && ["enabled", "frozen", "driftGuard", "feedback"]
            .iter()
            .any(|k| obj.get(*k).is_some_and(|v| !v.is_null()))
    {
        return Err(ApiError::invalid(
            "learn_status (GET /v2/context/learn) is read-only and does not accept mutating parameters; use POST /v2/context/learn",
        ));
    }
    let _learn_guard = scraper
        .context_source_lock(tenant, "__learn__")
        .lock_owned()
        .await;
    let mut state = try_load_learn_state(scraper, tenant)
        .await?
        .unwrap_or_default();
    let mut mutated = false;

    if let Some(en) = opt_bool(obj, "enabled")? {
        state.enabled = en;
        mutated = true;
    }
    if let Some(fr) = opt_bool(obj, "frozen")? {
        state.frozen = fr;
        mutated = true;
    }
    if let Some(dg_val) = obj.get("driftGuard")
        && !dg_val.is_null()
    {
        let dg_str = dg_val
            .as_str()
            .ok_or_else(|| ApiError::invalid("'driftGuard' must be a string"))?;
        state.drift_guard = parse_drift_guard(dg_str)?;
        mutated = true;
    }
    let mut drift_triggered = false;
    if let Some(fb_val) = obj.get("feedback")
        && !fb_val.is_null()
    {
        let fb = fb_val
            .as_object()
            .ok_or_else(|| ApiError::invalid("'feedback' must be a JSON object"))?;
        let score = opt_f32_bounded(fb, "rawScore", 0.0, 1.0)?
            .ok_or_else(|| ApiError::invalid("missing required 'feedback.rawScore'"))?;
        let correct = opt_bool(fb, "groundedCorrect")?.unwrap_or(false);
        let hard_gt = opt_bool(fb, "isHardGroundTruth")?.unwrap_or(false);
        drift_triggered = state.observe_feedback(score, correct, hard_gt);
        mutated = true;
    }
    if mutated && scraper.store_ref().is_some() {
        save_learn_state(scraper, tenant, &state).await?;
    }
    Ok(json!({
        "success": true,
        "driftTriggered": drift_triggered,
        "learn": {
            "version": state.version,
            "enabled": state.enabled,
            "frozen": state.frozen,
            "driftGuard": state.drift_guard,
            "sampleCount": state.grounded_scores.len(),
            "driftFallbacks": state.drift_fallbacks,
            "lastFallbackReason": state.last_fallback_reason,
        }
    }))
}
