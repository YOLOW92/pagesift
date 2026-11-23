//! Strict request normalization and parameter bounds validation for all 1.7 Resolve operations
//! across Rust, HTTP `/v2/resolve`, CLI, MCP, Python, Node.js, and C FFI.

use pagesift_api::ApiError;
use pagesift_graph::resolve::{
    CounterProbeKind, ProofStandard, ReconcileMode, ResolutionState, ResolveCheckRequest,
    ResolveLearnFeedback, RevalidateMode, ScopeDomainKind, VerifierMode,
};
use serde_json::{Map, Value};

use crate::context::validate::{
    MAX_ACTION_ACQUISITION_REQUESTS, opt_bool, opt_f32_bounded, opt_ident, opt_u64,
    opt_usize_bounded, parse_allowed_hosts, parse_opt_time, parse_string_list, validate_ident_str,
};

// [策略] 单次 conclusions / history / events 查询条数硬上限
pub const MAX_RESOLVE_RESULT_LIMIT: usize = 1_000;
// [策略] 单次 check 显式传入 claims 条数硬上限（与 decompose 上限一致）
pub const MAX_RESOLVE_CLAIMS: usize = 64;
// [策略] 最小独立信源要求硬上限
const MAX_MIN_INDEPENDENT_SOURCES: usize = 256;
// [策略] 单条 answer 文本字节硬上限 262,144 (256 KiB)，防止恶意超大负载耗尽 CPU/内存
const MAX_ANSWER_BYTES: usize = 262_144;

pub(crate) fn build_check_request(
    obj: &Map<String, Value>,
) -> Result<ResolveCheckRequest, ApiError> {
    let answer = match obj.get("answer").or_else(|| obj.get("text")) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => {
            if s.len() > MAX_ANSWER_BYTES {
                return Err(ApiError::invalid(format!(
                    "'answer' exceeds maximum allowed length of {MAX_ANSWER_BYTES} bytes"
                )));
            }
            s.clone()
        }
        Some(_) => return Err(ApiError::invalid("'answer' must be a string")),
    };

    let claims = parse_string_list(obj.get("claims"), "claims")?;
    if claims.len() > MAX_RESOLVE_CLAIMS {
        return Err(ApiError::invalid(format!(
            "'claims' exceeds maximum allowed count of {MAX_RESOLVE_CLAIMS}"
        )));
    }
    if answer.trim().is_empty() && claims.is_empty() {
        return Err(ApiError::invalid(
            "resolve check requires non-empty 'answer' (or 'text') or 'claims'",
        ));
    }

    let query = match obj.get("query") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.trim().is_empty() => None,
        Some(Value::String(s)) => Some(s.trim().to_owned()),
        Some(_) => return Err(ApiError::invalid("'query' must be a string")),
    };
    let domain = match obj.get("domain") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.trim().is_empty() => None,
        Some(Value::String(s)) => Some(validate_ident_str(s, "domain")?),
        Some(_) => return Err(ApiError::invalid("'domain' must be a string")),
    };

    let mut source_ids = Vec::new();
    for sid in parse_string_list(obj.get("sourceIds"), "sourceIds")? {
        let valid = validate_ident_str(&sid, "sourceIds[]")?;
        if !source_ids.contains(&valid) {
            source_ids.push(valid);
        }
    }
    if let Some(single) = opt_ident(obj, "sourceId", None)?
        && !source_ids.contains(&single)
    {
        source_ids.push(single);
    }

    let mut parent_conclusion_ids = Vec::new();
    for pid in parse_string_list(obj.get("parentConclusionIds"), "parentConclusionIds")? {
        let valid = validate_ident_str(&pid, "parentConclusionIds[]")?;
        if !parent_conclusion_ids.contains(&valid) {
            parent_conclusion_ids.push(valid);
        }
    }

    let conclusion_id = opt_ident(obj, "conclusionId", Some("id"))?;
    let subject_key = opt_ident(obj, "subjectKey", None)?;
    let proof_standard = parse_proof_standard(obj.get("proofStandard"))?;
    let domain_kind = parse_domain_kind(obj.get("domainKind"))?;
    let verifier_mode = parse_verifier_mode(obj.get("verifierMode"))?;
    let min_independent_sources =
        opt_usize_bounded(obj, "minIndependentSources", 1, MAX_MIN_INDEPENDENT_SOURCES)?
            .unwrap_or(1);

    let as_of_ms = parse_opt_time(obj.get("asOf").or_else(|| obj.get("asOfMs")), "asOf")?;
    let effective_at_ms = parse_opt_time(
        obj.get("effectiveAt").or_else(|| obj.get("effectiveAtMs")),
        "effectiveAt",
    )?;
    let assumptions = parse_string_list(obj.get("assumptions"), "assumptions")?;
    let enable_gap_resolution = opt_bool(obj, "enableGapResolution")?.unwrap_or(true);
    let enable_counter_search = opt_bool(obj, "enableCounterSearch")?.unwrap_or(true);
    let allow_network_acquisition = opt_bool(obj, "allowNetworkAcquisition")?.unwrap_or(false);
    let allow_external_search = opt_bool(obj, "allowExternalSearch")?.unwrap_or(false);
    let strict_constrain_to_urls = opt_bool(obj, "strictConstrainToUrls")?.unwrap_or(false);
    let allowed_hosts = parse_allowed_hosts(obj.get("allowedHosts"))?;
    let candidate_urls = parse_string_list(
        obj.get("candidateUrls").or_else(|| obj.get("urls")),
        "candidateUrls",
    )?;
    // [策略] Resolve 默认单次补采请求上限为 4，允许调用方显式配置 0..=64
    let max_acquisition_requests = opt_usize_bounded(
        obj,
        "maxAcquisitionRequests",
        0,
        MAX_ACTION_ACQUISITION_REQUESTS,
    )?
    .unwrap_or(4);
    let include_unverified = opt_bool(obj, "includeUnverified")?.unwrap_or(false);
    let learn_enabled = opt_bool(obj, "learnEnabled")?.unwrap_or(true);
    let persist = opt_bool(obj, "persist")?.unwrap_or(false);

    Ok(ResolveCheckRequest {
        answer,
        claims,
        query,
        domain,
        source_ids,
        proof_standard,
        domain_kind,
        min_independent_sources,
        as_of_ms,
        effective_at_ms,
        verifier_mode,
        assumptions,
        parent_conclusion_ids,
        conclusion_id,
        subject_key,
        enable_gap_resolution,
        enable_counter_search,
        allow_network_acquisition,
        allow_external_search,
        strict_constrain_to_urls,
        allowed_hosts,
        candidate_urls,
        max_acquisition_requests,
        include_unverified,
        learn_enabled,
        persist,
    })
}

pub(crate) fn parse_proof_standard(v: Option<&Value>) -> Result<ProofStandard, ApiError> {
    match v {
        None | Some(Value::Null) => Ok(ProofStandard::Preponderance),
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "scintilla" => Ok(ProofStandard::Scintilla),
            "preponderance" => Ok(ProofStandard::Preponderance),
            "clear_and_convincing" | "clearandconvincing" => Ok(ProofStandard::ClearAndConvincing),
            "strict_universal" | "strictuniversal" => Ok(ProofStandard::StrictUniversal),
            other => Err(ApiError::invalid(format!(
                "invalid 'proofStandard' '{other}'; expected scintilla, preponderance, clear_and_convincing, or strict_universal"
            ))),
        },
        Some(_) => Err(ApiError::invalid("'proofStandard' must be a string")),
    }
}

pub(crate) fn parse_domain_kind(v: Option<&Value>) -> Result<ScopeDomainKind, ApiError> {
    match v {
        None | Some(Value::Null) => Ok(ScopeDomainKind::OpenWorld),
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "open_world" | "openworld" | "open" => Ok(ScopeDomainKind::OpenWorld),
            "closed_observed_set" | "closedobservedset" | "closed" => {
                Ok(ScopeDomainKind::ClosedObservedSet)
            }
            other => Err(ApiError::invalid(format!(
                "invalid 'domainKind' '{other}'; expected open_world or closed_observed_set"
            ))),
        },
        Some(_) => Err(ApiError::invalid("'domainKind' must be a string")),
    }
}

pub(crate) fn parse_verifier_mode(v: Option<&Value>) -> Result<VerifierMode, ApiError> {
    match v {
        None | Some(Value::Null) => Ok(VerifierMode::DeterministicOnly),
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "deterministic" | "deterministic_only" | "deterministiconly" | "det" => {
                Ok(VerifierMode::DeterministicOnly)
            }
            "hybrid_assisted" | "hybridassisted" | "hybrid" => Ok(VerifierMode::HybridAssisted),
            other => Err(ApiError::invalid(format!(
                "invalid 'verifierMode' '{other}'; expected deterministic_only or hybrid_assisted"
            ))),
        },
        Some(_) => Err(ApiError::invalid("'verifierMode' must be a string")),
    }
}

pub(crate) fn parse_reconcile_mode(v: Option<&Value>) -> Result<ReconcileMode, ApiError> {
    match v {
        None | Some(Value::Null) => Ok(ReconcileMode::Incremental),
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "incremental" => Ok(ReconcileMode::Incremental),
            "full_source" | "fullsource" | "full" => Ok(ReconcileMode::FullSource),
            other => Err(ApiError::invalid(format!(
                "invalid 'mode' '{other}'; expected incremental or full_source"
            ))),
        },
        Some(_) => Err(ApiError::invalid("'mode' must be a string")),
    }
}

pub(crate) fn parse_revalidate_mode(v: Option<&Value>) -> Result<RevalidateMode, ApiError> {
    match v {
        None | Some(Value::Null) => Ok(RevalidateMode::Incremental),
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "incremental" => Ok(RevalidateMode::Incremental),
            "full" => Ok(RevalidateMode::Full),
            other => Err(ApiError::invalid(format!(
                "invalid 'mode' '{other}'; expected incremental or full"
            ))),
        },
        Some(_) => Err(ApiError::invalid("'mode' must be a string")),
    }
}

pub(crate) fn parse_resolution_state(
    v: Option<&Value>,
) -> Result<Option<ResolutionState>, ApiError> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(val) => serde_json::from_value::<ResolutionState>(val.clone())
            .map(Some)
            .map_err(|e| ApiError::invalid(format!("invalid 'state': {e}"))),
    }
}

pub(crate) fn parse_learn_feedback(
    v: Option<&Value>,
) -> Result<Vec<ResolveLearnFeedback>, ApiError> {
    let Some(val) = v else {
        return Ok(Vec::new());
    };
    if val.is_null() {
        return Ok(Vec::new());
    }
    let arr = val
        .as_array()
        .ok_or_else(|| ApiError::invalid("'feedback' must be an array of feedback objects"))?;
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let m = item
            .as_object()
            .ok_or_else(|| ApiError::invalid(format!("'feedback[{idx}]' must be a JSON object")))?;
        let domain = match m.get("domain") {
            None | Some(Value::Null) => "general".to_owned(),
            Some(Value::String(s)) if !s.trim().is_empty() => {
                validate_ident_str(s, &format!("feedback[{idx}].domain"))?
            }
            _ => {
                return Err(ApiError::invalid(format!(
                    "'feedback[{idx}].domain' must be a non-empty string"
                )));
            }
        };
        let claim_kind = match m.get("claimKind") {
            None | Some(Value::Null) => "Specific".to_owned(),
            Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_owned(),
            _ => {
                return Err(ApiError::invalid(format!(
                    "'feedback[{idx}].claimKind' must be a non-empty string"
                )));
            }
        };
        let raw_score = opt_f32_bounded(m, "rawScore", 0.0, 1.0)?.ok_or_else(|| {
            ApiError::invalid(format!(
                "missing required 'feedback[{idx}].rawScore' in [0, 1]"
            ))
        })?;
        let grounded_supported = opt_bool(m, "groundedSupported")?
            .or(opt_bool(m, "labelSupported")?)
            .ok_or_else(|| {
                ApiError::invalid(format!(
                    "missing required boolean 'feedback[{idx}].groundedSupported'"
                ))
            })?;
        let is_hard_ground_truth = opt_bool(m, "isHardGroundTruth")?.unwrap_or(false);
        let counter_probe_kind = match m.get("counterProbeKind").or_else(|| m.get("probeKind")) {
            None | Some(Value::Null) => None,
            Some(pv) => Some(
                serde_json::from_value::<CounterProbeKind>(pv.clone()).map_err(|e| {
                    ApiError::invalid(format!("invalid 'feedback[{idx}].counterProbeKind': {e}"))
                })?,
            ),
        };
        let counter_found = opt_bool(m, "counterFound")?.or(opt_bool(m, "probeFoundCounter")?);
        let verifier_version = match m.get("verifierVersion") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_owned()),
            _ => {
                return Err(ApiError::invalid(format!(
                    "'feedback[{idx}].verifierVersion' must be a non-empty string"
                )));
            }
        };
        out.push(ResolveLearnFeedback {
            domain,
            claim_kind,
            raw_score,
            grounded_supported,
            is_hard_ground_truth,
            counter_probe_kind,
            counter_found,
            verifier_version,
        });
    }
    Ok(out)
}

pub(crate) fn parse_source_id_filter(obj: &Map<String, Value>) -> Result<Vec<String>, ApiError> {
    let mut source_ids = Vec::new();
    for sid in parse_string_list(obj.get("sourceIds"), "sourceIds")? {
        let valid = validate_ident_str(&sid, "sourceIds[]")?;
        if !source_ids.contains(&valid) {
            source_ids.push(valid);
        }
    }
    if let Some(single) = opt_ident(obj, "sourceId", None)?
        && !source_ids.contains(&single)
    {
        source_ids.push(single);
    }
    Ok(source_ids)
}

pub(crate) fn parse_limit(obj: &Map<String, Value>, default_val: usize) -> Result<usize, ApiError> {
    Ok(opt_usize_bounded(obj, "limit", 1, MAX_RESOLVE_RESULT_LIMIT)?.unwrap_or(default_val))
}

pub(crate) fn parse_opt_u64(obj: &Map<String, Value>, key: &str) -> Result<Option<u64>, ApiError> {
    opt_u64(obj, key)
}
