//! Unified 1.7 Resolve dispatcher shared across Rust `Scraper`, HTTP `/v2/resolve`, CLI, MCP,
//! Cluster gateway, Python, Node.js, and C FFI.

pub mod live;
pub mod pipeline;
pub(crate) mod validate;

use std::sync::Arc;

use pagesift_api::{ApiError, ErrorCode, PageBudget};
use pagesift_extract::resolve::ResolveLearnState;
use pagesift_graph::resolve::DEFAULT_VERIFIER_VERSION;
use serde_json::{Map, Value, json};

pub use live::{reconcile_changes, revalidate_conclusions};
pub use pipeline::execute_check_with_budget;

use crate::context::validate::{
    MAX_ACTION_ACQUISITION_REQUESTS, normalize_keys, opt_bool, opt_ident, opt_usize_bounded,
    parse_allowed_hosts, parse_inline_docs, parse_opt_time, parse_string_list, req_ident,
};
use crate::engine::Scraper;
use pipeline::map_resolve_store_error;
use validate::{
    build_check_request, parse_learn_feedback, parse_limit, parse_opt_u64, parse_reconcile_mode,
    parse_resolution_state, parse_revalidate_mode, parse_source_id_filter, parse_verifier_mode,
};

// [策略] conclusions / history / events 默认分页上限
const DEFAULT_LIST_LIMIT: usize = 50;
// [策略] 历史修订列表默认返回最近 32 版
const DEFAULT_HISTORY_LIMIT: usize = 32;
// [策略] 结论事件流默认返回最近 100 条
const DEFAULT_EVENTS_LIMIT: usize = 100;

/// Unified JSON-in / JSON-out Resolve dispatcher supporting both `snake_case` and `camelCase` keys.
pub async fn dispatch(
    scraper: &Scraper,
    tenant: Option<&str>,
    raw_request: Value,
) -> Result<Value, ApiError> {
    dispatch_with_budget(scraper, tenant, raw_request, None).await
}

/// Unified JSON-in / JSON-out Resolve dispatcher enforcing an optional caller-level daily [`PageBudget`].
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
            "conflicting 'action' ('{a}') and 'op' ('{o}') fields in resolve request"
        )));
    }
    let op = action_field
        .or(op_field)
        .unwrap_or_else(|| "check".to_owned());

    match op.as_str() {
        "check" | "verify" => handle_check(scraper, tenant, &obj, page_budget).await,
        "conclusions" | "list" => handle_list(scraper, tenant, &obj).await,
        "get" => handle_get(scraper, tenant, &obj).await,
        "history" => handle_history(scraper, tenant, &obj).await,
        "graph" => handle_graph(scraper, tenant, &obj).await,
        "reconcile" => handle_reconcile(scraper, tenant, &obj).await,
        "revalidate" => handle_revalidate(scraper, tenant, &obj, page_budget).await,
        "events" => handle_events(scraper, tenant, &obj).await,
        "learn_status" | "learnstatus" => handle_learn(scraper, tenant, &obj, true).await,
        "learn" => handle_learn(scraper, tenant, &obj, false).await,
        "delete" => handle_delete(scraper, tenant, &obj).await,
        other => Err(ApiError::invalid(format!(
            "unsupported resolve operation '{other}'; expected check, conclusions, get, history, graph, reconcile, revalidate, events, learn, learn_status, or delete"
        ))),
    }
}

async fn handle_check(
    scraper: &Scraper,
    tenant: Option<&str>,
    obj: &Map<String, Value>,
    page_budget: Option<Arc<dyn PageBudget>>,
) -> Result<Value, ApiError> {
    let req = build_check_request(obj)?;
    let inline_docs = parse_inline_docs(obj.get("documents"))?;
    let outcome = execute_check_with_budget(scraper, tenant, req, inline_docs, page_budget).await?;
    Ok(json!({
        "success": true,
        "outcome": outcome,
    }))
}

async fn handle_list(
    scraper: &Scraper,
    tenant: Option<&str>,
    obj: &Map<String, Value>,
) -> Result<Value, ApiError> {
    let store = require_store(scraper, "list conclusions")?;
    let state_filter = parse_resolution_state(obj.get("state"))?;
    let source_filter = opt_ident(obj, "sourceId", None)?;
    let limit = parse_limit(obj, DEFAULT_LIST_LIMIT)?;
    let t_owned = tenant.map(str::to_owned);
    let (total, conclusions) = store
        .blocking(move |st| {
            st.resolve_for(t_owned.as_deref()).list_conclusions(
                state_filter,
                source_filter.as_deref(),
                limit,
            )
        })
        .await
        .map_err(map_resolve_store_error)?;
    Ok(json!({
        "success": true,
        "total": total,
        "conclusions": conclusions,
    }))
}

async fn handle_get(
    scraper: &Scraper,
    tenant: Option<&str>,
    obj: &Map<String, Value>,
) -> Result<Value, ApiError> {
    let store = require_store(scraper, "get conclusion")?;
    let cid = req_ident(obj, "conclusionId", Some("id"))?;
    let as_of_ms = parse_opt_time(obj.get("asOf").or_else(|| obj.get("asOfMs")), "asOf")?;
    let version = parse_opt_u64(obj, "version")?;
    let t_owned = tenant.map(str::to_owned);

    if let Some(ver) = version {
        let rev = store
            .blocking(move |st| st.resolve_for(t_owned.as_deref()).get_revision(&cid, ver))
            .await
            .map_err(map_resolve_store_error)?;
        return match rev {
            Some(r) => Ok(json!({ "success": true, "revision": r })),
            None => Err(ApiError::new(
                ErrorCode::NotFound,
                format!("conclusion revision version={ver} not found"),
            )),
        };
    }

    if let Some(as_of) = as_of_ms {
        let rev = store
            .blocking(move |st| {
                st.resolve_for(t_owned.as_deref())
                    .get_conclusion_as_of(&cid, as_of)
            })
            .await
            .map_err(map_resolve_store_error)?;
        return match rev {
            Some(r) => Ok(json!({ "success": true, "asOfMs": as_of, "revision": r })),
            None => Err(ApiError::new(
                ErrorCode::NotFound,
                format!("no conclusion revision existed at or before asOf={as_of}"),
            )),
        };
    }

    let rec = store
        .blocking(move |st| st.resolve_for(t_owned.as_deref()).get_conclusion(&cid))
        .await
        .map_err(map_resolve_store_error)?;
    match rec {
        Some(c) => Ok(json!({ "success": true, "conclusion": c })),
        None => Err(ApiError::new(ErrorCode::NotFound, "conclusion not found")),
    }
}

async fn handle_history(
    scraper: &Scraper,
    tenant: Option<&str>,
    obj: &Map<String, Value>,
) -> Result<Value, ApiError> {
    let store = require_store(scraper, "get conclusion history")?;
    let cid = req_ident(obj, "conclusionId", Some("id"))?;
    let limit = parse_limit(obj, DEFAULT_HISTORY_LIMIT)?;
    let t_owned = tenant.map(str::to_owned);
    let cid_copy = cid.clone();
    let revisions = store
        .blocking(move |st| {
            st.resolve_for(t_owned.as_deref())
                .list_history(&cid_copy, limit)
        })
        .await
        .map_err(map_resolve_store_error)?;
    Ok(json!({
        "success": true,
        "conclusionId": cid,
        "revisions": revisions,
    }))
}

async fn handle_graph(
    scraper: &Scraper,
    tenant: Option<&str>,
    obj: &Map<String, Value>,
) -> Result<Value, ApiError> {
    let store = require_store(scraper, "inspect conclusion dependency graph")?;
    let cid = opt_ident(obj, "conclusionId", Some("id"))?;
    let limit = parse_limit(obj, DEFAULT_LIST_LIMIT)?;
    let t_owned = tenant.map(str::to_owned);

    store
        .blocking(move |st| -> Result<Value, ApiError> {
            let rs = st.resolve_for(t_owned.as_deref());
            if let Some(target_id) = cid {
                let Some(rec) = rs
                    .get_conclusion(&target_id)
                    .map_err(map_resolve_store_error)?
                else {
                    return Err(ApiError::new(ErrorCode::NotFound, "conclusion not found"));
                };
                let children: Vec<String> = rs
                    .find_children(&target_id)
                    .map_err(map_resolve_store_error)?
                    .into_iter()
                    .collect();
                let mut seed = std::collections::BTreeSet::new();
                seed.insert(target_id.clone());
                let mut downstream = rs
                    .expand_downstream_conclusions(&seed)
                    .map_err(map_resolve_store_error)?;
                downstream.remove(&target_id);
                Ok(json!({
                    "success": true,
                    "conclusionId": rec.conclusion_id,
                    "version": rec.version,
                    "state": rec.state,
                    "belnap": rec.belnap,
                    "parentConclusionIds": rec.parent_conclusion_ids,
                    "directChildConclusionIds": children,
                    "transitiveDownstreamIds": downstream.into_iter().collect::<Vec<_>>(),
                    "dependentUnitIds": rec.dependent_unit_ids,
                    "dependentResourceKeys": rec.dependent_resource_keys,
                    "originRoots": rec.origin_roots,
                    "scope": rec.scope,
                    "provenance": rec.provenance,
                    "cutSetAnalysis": rec.cut_set_analysis,
                }))
            } else {
                let (total, list) = rs
                    .list_conclusions(None, None, limit)
                    .map_err(map_resolve_store_error)?;
                let nodes: Vec<Value> = list
                    .iter()
                    .map(|c| {
                        json!({
                            "conclusionId": c.conclusion_id,
                            "version": c.version,
                            "state": c.state,
                            "belnap": c.belnap,
                            "parentConclusionIds": c.parent_conclusion_ids,
                            "dependentUnitCount": c.dependent_unit_ids.len(),
                            "originRootCount": c.origin_roots.len(),
                            "singlePointOfFailureCount": c
                                .cut_set_analysis
                                .as_ref()
                                .map_or(0, |csa| csa.single_points_of_failure.len()),
                            "hasNegativeOrUniversalScope": c.scope.has_negative_or_universal_dependency,
                        })
                    })
                    .collect();
                let mut edges = Vec::new();
                for c in &list {
                    for pid in &c.parent_conclusion_ids {
                        edges.push(json!({
                            "fromParent": pid,
                            "toChild": c.conclusion_id,
                            "kind": "conclusion_dependency",
                        }));
                    }
                }
                Ok(json!({
                    "success": true,
                    "total": total,
                    "nodes": nodes,
                    "edges": edges,
                }))
            }
        })
        .await
}

async fn handle_reconcile(
    scraper: &Scraper,
    tenant: Option<&str>,
    obj: &Map<String, Value>,
) -> Result<Value, ApiError> {
    let source_ids = parse_source_id_filter(obj)?;
    let mode = parse_reconcile_mode(obj.get("mode"))?;
    let report = reconcile_changes(scraper, tenant, source_ids, mode).await?;
    Ok(json!({
        "success": true,
        "report": report,
    }))
}

async fn handle_revalidate(
    scraper: &Scraper,
    tenant: Option<&str>,
    obj: &Map<String, Value>,
    page_budget: Option<Arc<dyn PageBudget>>,
) -> Result<Value, ApiError> {
    let mut conclusion_ids = parse_string_list(obj.get("conclusionIds"), "conclusionIds")?;
    if let Some(single) = opt_ident(obj, "conclusionId", Some("id"))?
        && !conclusion_ids.contains(&single)
    {
        conclusion_ids.push(single);
    }
    let source_ids = parse_source_id_filter(obj)?;
    let mode = parse_revalidate_mode(obj.get("mode"))?;
    let verifier_mode = parse_verifier_mode(obj.get("verifierMode"))?;
    let allow_network_acquisition = opt_bool(obj, "allowNetworkAcquisition")?.unwrap_or(false);
    let allow_external_search = opt_bool(obj, "allowExternalSearch")?.unwrap_or(false);
    let allowed_hosts = parse_allowed_hosts(obj.get("allowedHosts"))?;
    // [策略] revalidate 默认网络请求上限为 4，允许调用方显式调整 0..=64
    let max_acquisition_requests = opt_usize_bounded(
        obj,
        "maxAcquisitionRequests",
        0,
        MAX_ACTION_ACQUISITION_REQUESTS,
    )?
    .unwrap_or(4);

    let outcome = revalidate_conclusions(
        scraper,
        tenant,
        conclusion_ids,
        source_ids,
        mode,
        verifier_mode,
        allow_network_acquisition,
        allow_external_search,
        allowed_hosts,
        max_acquisition_requests,
        page_budget,
    )
    .await?;
    Ok(json!({
        "success": true,
        "outcome": outcome,
    }))
}

async fn handle_events(
    scraper: &Scraper,
    tenant: Option<&str>,
    obj: &Map<String, Value>,
) -> Result<Value, ApiError> {
    let store = require_store(scraper, "list resolve events")?;
    let after_seq = parse_opt_u64(obj, "afterSeq")?.or(parse_opt_u64(obj, "sinceEventSeq")?);
    let limit = parse_limit(obj, DEFAULT_EVENTS_LIMIT)?;
    let t_owned = tenant.map(str::to_owned);
    let events = store
        .blocking(move |st| {
            st.resolve_for(t_owned.as_deref())
                .list_events(after_seq, limit)
        })
        .await
        .map_err(map_resolve_store_error)?;
    Ok(json!({
        "success": true,
        "events": events,
    }))
}

async fn handle_delete(
    scraper: &Scraper,
    tenant: Option<&str>,
    obj: &Map<String, Value>,
) -> Result<Value, ApiError> {
    let store = require_store(scraper, "delete conclusion")?;
    let cid = req_ident(obj, "conclusionId", Some("id"))?;
    let reason = obj
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("explicit_delete")
        .to_owned();
    let lock = scraper.resolve_tenant_lock(tenant);
    let _guard = lock.lock().await;
    let t_owned = tenant.map(str::to_owned);
    let cid_copy = cid.clone();
    let deleted = store
        .blocking(move |st| {
            st.resolve_for(t_owned.as_deref())
                .delete_conclusion(&cid_copy, &reason)
        })
        .await
        .map_err(map_resolve_store_error)?;
    if !deleted {
        return Err(ApiError::new(ErrorCode::NotFound, "conclusion not found"));
    }
    Ok(json!({
        "success": true,
        "deleted": true,
        "conclusionId": cid,
    }))
}

async fn handle_learn(
    scraper: &Scraper,
    tenant: Option<&str>,
    obj: &Map<String, Value>,
    read_only: bool,
) -> Result<Value, ApiError> {
    let reset = opt_bool(obj, "reset")?.unwrap_or(false);
    let enabled = opt_bool(obj, "enabled")?;
    let frozen = opt_bool(obj, "frozen")?;
    let feedback = parse_learn_feedback(obj.get("feedback"))?;
    let verifier_version = match obj.get("verifierVersion") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        Some(_) => {
            return Err(ApiError::invalid(
                "'verifierVersion' must be a non-empty string",
            ));
        }
    };

    let has_mutation = reset
        || enabled.is_some()
        || frozen.is_some()
        || !feedback.is_empty()
        || verifier_version.is_some();

    if read_only {
        if has_mutation {
            return Err(ApiError::invalid(
                "GET /v2/resolve/learn (learn_status) is strictly read-only; use POST /v2/resolve/learn to mutate learn state",
            ));
        }
        let (state, warn) = load_resolve_learn_state_with_warning(scraper, tenant).await;
        return Ok(json!({
            "success": true,
            "summary": state.summary(),
            "warning": warn,
        }));
    }

    if !has_mutation {
        let (state, warn) = load_resolve_learn_state_with_warning(scraper, tenant).await;
        return Ok(json!({
            "success": true,
            "summary": state.summary(),
            "warning": warn,
        }));
    }

    let lock = scraper.resolve_tenant_lock(tenant);
    let _guard = lock.lock().await;

    // Fail closed on corrupt persisted learn state during mutations unless explicitly resetting!
    let mut state = if reset {
        ResolveLearnState::default()
    } else {
        try_load_resolve_learn_state(scraper, tenant)
            .await?
            .unwrap_or_default()
    };

    if let Some(vv) = verifier_version {
        state.ensure_verifier_version(&vv);
    } else {
        state.ensure_verifier_version(DEFAULT_VERIFIER_VERSION);
    }
    if let Some(e) = enabled {
        state.enabled = e;
    }
    if let Some(f) = frozen {
        state.frozen = f;
    }
    let mut accepted = 0usize;
    let mut ignored = 0usize;
    let mut drift_triggered = false;
    for fb in &feedback {
        if !state.enabled || state.frozen || !fb.is_hard_ground_truth || !fb.raw_score.is_finite() {
            ignored += 1;
        } else {
            accepted += 1;
            if state.observe_feedback(fb) {
                drift_triggered = true;
            }
        }
    }

    save_resolve_learn_state(scraper, tenant, &state).await?;
    Ok(json!({
        "success": true,
        "acceptedFeedback": accepted,
        "ignoredWeakOrFrozenFeedback": ignored,
        "driftTriggered": drift_triggered,
        "summary": state.summary(),
    }))
}

fn require_store(scraper: &Scraper, action: &str) -> Result<pagesift_store::Store, ApiError> {
    scraper.store_ref().cloned().ok_or_else(|| {
        ApiError::invalid(format!(
            "resolve '{action}' requires a persistent store configured on Scraper"
        ))
    })
}

/// Read persisted [`ResolveLearnState`] without swallowing storage or JSON corruption.
pub async fn try_load_resolve_learn_state(
    scraper: &Scraper,
    tenant: Option<&str>,
) -> Result<Option<ResolveLearnState>, ApiError> {
    let Some(store) = scraper.store_ref().cloned() else {
        return Ok(None);
    };
    let t = tenant.map(str::to_owned);
    store
        .blocking(move |st| -> Result<Option<ResolveLearnState>, ApiError> {
            let raw_opt = st
                .resolve_for(t.as_deref())
                .load_learn_bytes("default")
                .map_err(map_resolve_store_error)?;
            match raw_opt {
                None => Ok(None),
                Some(bytes) => {
                    let parsed =
                        serde_json::from_slice::<ResolveLearnState>(&bytes).map_err(|e| {
                            ApiError::new(
                                ErrorCode::Internal,
                                format!("corrupt resolve learn state JSON: {e}"),
                            )
                        })?;
                    Ok(Some(parsed))
                }
            }
        })
        .await
}

/// Load [`ResolveLearnState`] for read paths: cold start returns clean default without warning;
/// corruption degrades to neutral (`enabled=false`, uncalibrated) AND surfaces an explicit warning.
pub async fn load_resolve_learn_state_with_warning(
    scraper: &Scraper,
    tenant: Option<&str>,
) -> (ResolveLearnState, Option<String>) {
    match try_load_resolve_learn_state(scraper, tenant).await {
        Ok(Some(mut state)) => {
            state.ensure_verifier_version(DEFAULT_VERIFIER_VERSION);
            (state, None)
        }
        Ok(None) => (ResolveLearnState::default(), None),
        Err(err) => {
            tracing::warn!(
                error = %err,
                "corrupt persisted resolve learn state; degrading to neutral uncalibrated mode"
            );
            let neutral = ResolveLearnState {
                enabled: false,
                ..Default::default()
            };
            (
                neutral,
                Some(format!("resolve_learn_state_corrupt: {}", err.message)),
            )
        }
    }
}

pub async fn save_resolve_learn_state(
    scraper: &Scraper,
    tenant: Option<&str>,
    state: &ResolveLearnState,
) -> Result<(), ApiError> {
    let store = require_store(scraper, "save learn state")?;
    let bytes =
        serde_json::to_vec(state).map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
    let t = tenant.map(str::to_owned);
    store
        .blocking(move |st| {
            st.resolve_for(t.as_deref())
                .put_learn_bytes("default", &bytes)
        })
        .await
        .map_err(map_resolve_store_error)
}
