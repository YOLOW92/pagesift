//! Strict request normalization, JS safe-integer (`f64`) handling, and parameter bounds validation
//! for all 1.6 Context operations across HTTP, MCP, CLI, FFI, Python, and Node.js.

use pagesift_api::ApiError;
use pagesift_extract::context::DriftGuardKind;
use pagesift_graph::context::{
    ActionMode, AssetSpecStamp, ContextActionRequest, ContextPackRequest, ContextSearchRequest,
    PackOrdering, PackSelector, UnitMatchMode,
};
use pagesift_graph::parse_timestamp_ms;
use serde_json::{Map, Value};

use crate::context::ingest::ContextDocumentInput;

// [协议] IEEE-754 双精度浮点数精确表示的最大整数上限（JavaScript `Number.MAX_SAFE_INTEGER`）
const MAX_SAFE_INT_F64: f64 = 9_007_199_254_740_991.0;
// [策略] ContextPack 单次最大上下文预算上限 1,048,576 tokens（覆盖长上下文模型窗口，拦截溢出滥用）
pub const MAX_PACK_BUDGET_TOKENS: usize = 1_048_576;
// [策略] 单次 Search / Units / Deltas 返回结果条数硬上限
pub const MAX_CONTEXT_RESULT_LIMIT: usize = 10_000;
// [策略] Action 单次目标导向补充获取的网络请求预算硬上限
pub const MAX_ACTION_ACQUISITION_REQUESTS: usize = 64;
// [策略] Action 要求的最小独立信源数合理上限
const MAX_MIN_INDEPENDENT_SOURCES: usize = 256;
// [策略] Context 逻辑标识符（sourceId / consumerId / instanceId）最大字节长度
const MAX_IDENT_BYTES: usize = 256;

pub(crate) fn normalize_keys(value: Value) -> Result<Map<String, Value>, ApiError> {
    let Value::Object(raw) = value else {
        return Err(ApiError::invalid("context request must be a JSON object"));
    };
    Ok(normalize_map(raw))
}

fn normalize_map(raw: Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::with_capacity(raw.len());
    for (k, v) in raw {
        let nv = match v {
            Value::Object(m) => Value::Object(normalize_map(m)),
            Value::Array(arr) => Value::Array(normalize_array(arr)),
            other => other,
        };
        let camel = to_camel(&k);
        // Prefer an explicit camelCase key if caller accidentally sent both snake_case and camelCase
        if k == camel || !out.contains_key(&camel) {
            out.insert(camel, nv);
        }
    }
    out
}

fn normalize_array(arr: Vec<Value>) -> Vec<Value> {
    arr.into_iter()
        .map(|item| match item {
            Value::Object(m) => Value::Object(normalize_map(m)),
            Value::Array(inner) => Value::Array(normalize_array(inner)),
            other => other,
        })
        .collect()
}

pub(crate) fn to_camel(key: &str) -> String {
    if !key.contains('_') {
        return key.to_owned();
    }
    let mut out = String::with_capacity(key.len());
    let mut upper_next = false;
    for ch in key.chars() {
        if ch == '_' {
            upper_next = !out.is_empty();
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Parse a JSON integer, including `f64`-backed safe integers produced by `napi-rs` for JS numbers `> i32::MAX`.
pub(crate) fn json_u64(v: &Value, field: &str) -> Result<u64, ApiError> {
    if let Value::Number(n) = v {
        if let Some(u) = n.as_u64() {
            return Ok(u);
        }
        if let Some(f) = n.as_f64()
            && f.is_finite()
            && f >= 0.0
            && f.fract() == 0.0
            && f <= MAX_SAFE_INT_F64
        {
            return Ok(f as u64);
        }
    }
    Err(ApiError::invalid(format!(
        "'{field}' must be a non-negative integer"
    )))
}

pub(crate) fn opt_u64(obj: &Map<String, Value>, key: &str) -> Result<Option<u64>, ApiError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => json_u64(v, key).map(Some),
    }
}

pub(crate) fn req_u64(obj: &Map<String, Value>, key: &str) -> Result<u64, ApiError> {
    match obj.get(key) {
        None | Some(Value::Null) => Err(ApiError::invalid(format!(
            "missing required integer '{key}'"
        ))),
        Some(v) => json_u64(v, key),
    }
}

pub(crate) fn opt_usize_bounded(
    obj: &Map<String, Value>,
    key: &str,
    min: usize,
    max: usize,
) -> Result<Option<usize>, ApiError> {
    let Some(raw) = opt_u64(obj, key)? else {
        return Ok(None);
    };
    let val = usize::try_from(raw)
        .map_err(|_| ApiError::invalid(format!("'{key}' exceeds maximum allowed value {max}")))?;
    if val < min || val > max {
        return Err(ApiError::invalid(format!(
            "'{key}' must be between {min} and {max} (got {val})"
        )));
    }
    Ok(Some(val))
}

pub(crate) fn opt_bool(obj: &Map<String, Value>, key: &str) -> Result<Option<bool>, ApiError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(ApiError::invalid(format!("'{key}' must be a boolean"))),
    }
}

pub(crate) fn opt_f32_bounded(
    obj: &Map<String, Value>,
    key: &str,
    min: f32,
    max: f32,
) -> Result<Option<f32>, ApiError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            let f = n
                .as_f64()
                .filter(|x| x.is_finite())
                .ok_or_else(|| ApiError::invalid(format!("'{key}' must be a finite number")))?
                as f32;
            if !f.is_finite() || f < min || f > max {
                return Err(ApiError::invalid(format!(
                    "'{key}' must be between {min} and {max}"
                )));
            }
            Ok(Some(f))
        }
        Some(_) => Err(ApiError::invalid(format!("'{key}' must be a number"))),
    }
}

pub(crate) fn validate_ident_str(raw: &str, field: &str) -> Result<String, ApiError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ApiError::invalid(format!("'{field}' must not be empty")));
    }
    if trimmed.len() > MAX_IDENT_BYTES {
        return Err(ApiError::invalid(format!(
            "'{field}' exceeds {MAX_IDENT_BYTES} bytes"
        )));
    }
    // [协议] US-ASCII (RFC 20)：0x00..0x1F 与 0x7F 为控制字符，禁止进入键值存储复合键与租户标识符
    if trimmed.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(ApiError::invalid(format!(
            "'{field}' must not contain control characters"
        )));
    }
    Ok(trimmed.to_owned())
}

pub(crate) fn opt_ident(
    obj: &Map<String, Value>,
    primary: &str,
    alias: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let val = obj.get(primary).or_else(|| alias.and_then(|a| obj.get(a)));
    match val {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => {
            Err(ApiError::invalid(format!("'{primary}' must not be blank")))
        }
        Some(Value::String(s)) => validate_ident_str(s, primary).map(Some),
        Some(_) => Err(ApiError::invalid(format!("'{primary}' must be a string"))),
    }
}

pub(crate) fn req_ident(
    obj: &Map<String, Value>,
    primary: &str,
    alias: Option<&str>,
) -> Result<String, ApiError> {
    opt_ident(obj, primary, alias)?
        .ok_or_else(|| ApiError::invalid(format!("missing required '{primary}'")))
}

pub(crate) fn parse_string_list(v: Option<&Value>, field: &str) -> Result<Vec<String>, ApiError> {
    let Some(val) = v else {
        return Ok(Vec::new());
    };
    if val.is_null() {
        return Ok(Vec::new());
    }
    let arr = val
        .as_array()
        .ok_or_else(|| ApiError::invalid(format!("'{field}' must be an array of strings")))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item
            .as_str()
            .ok_or_else(|| ApiError::invalid(format!("'{field}' entries must be strings")))?;
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_owned());
        }
    }
    Ok(out)
}

pub(crate) fn parse_allowed_hosts(v: Option<&Value>) -> Result<Vec<String>, ApiError> {
    let Some(val) = v else {
        return Ok(Vec::new());
    };
    if val.is_null() {
        return Ok(Vec::new());
    }
    let arr = val
        .as_array()
        .ok_or_else(|| ApiError::invalid("'allowedHosts' must be an array of host strings"))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let raw = item
            .as_str()
            .ok_or_else(|| ApiError::invalid("'allowedHosts' entries must be strings"))?;
        let trimmed = raw.trim().trim_end_matches('.');
        if trimmed.is_empty() {
            return Err(ApiError::invalid(
                "'allowedHosts' entries must not be empty",
            ));
        }
        if trimmed.contains("://")
            || trimmed.contains('/')
            || trimmed.bytes().any(|b| b.is_ascii_control() || b == b' ')
        {
            return Err(ApiError::invalid(format!(
                "invalid host '{raw}' in 'allowedHosts': provide hostnames or IPs without scheme/path"
            )));
        }
        out.push(trimmed.to_ascii_lowercase());
    }
    Ok(out)
}

pub(crate) fn parse_opt_time(v: Option<&Value>, field: &str) -> Result<Option<u64>, ApiError> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(num @ Value::Number(_)) => json_u64(num, field).map(Some),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if let Ok(ms) = trimmed.parse::<u64>() {
                Ok(Some(ms))
            } else if let Some(ms) = parse_timestamp_ms(trimmed) {
                Ok(Some(ms))
            } else {
                Err(ApiError::invalid(format!(
                    "invalid '{field}' timestamp '{s}': use Unix ms or RFC-3339 with explicit timezone"
                )))
            }
        }
        _ => Err(ApiError::invalid(format!(
            "'{field}' must be integer Unix ms or RFC-3339 string"
        ))),
    }
}

pub(crate) fn parse_match_mode(v: Option<&Value>) -> Result<UnitMatchMode, ApiError> {
    match v {
        None | Some(Value::Null) => Ok(UnitMatchMode::BoundedUot),
        Some(val) => serde_json::from_value(val.clone())
            .map_err(|e| ApiError::invalid(format!("invalid 'matchMode': {e}"))),
    }
}

pub(crate) fn parse_spec_stamp(v: Option<&Value>) -> Result<Option<AssetSpecStamp>, ApiError> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(val) => {
            let stamp: AssetSpecStamp = serde_json::from_value(val.clone())
                .map_err(|e| ApiError::invalid(format!("invalid 'specStamp': {e}")))?;
            if stamp.model_id.trim().is_empty() {
                return Err(ApiError::invalid("'specStamp.modelId' must not be empty"));
            }
            Ok(Some(stamp))
        }
    }
}

pub(crate) fn parse_inline_docs(v: Option<&Value>) -> Result<Vec<ContextDocumentInput>, ApiError> {
    let Some(val) = v else {
        return Ok(Vec::new());
    };
    if val.is_null() {
        return Ok(Vec::new());
    }
    let arr = val
        .as_array()
        .ok_or_else(|| ApiError::invalid("'documents' must be an array of document objects"))?;
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let mut item_val = item.clone();
        // Convert JS `f64` safe-integer `observedAtMs` into `u64` JSON integer before serde deserialization
        if let Value::Object(m) = &mut item_val
            && let Some(obs_v) = m.get("observedAtMs").cloned()
            && !obs_v.is_null()
        {
            let ms = json_u64(&obs_v, &format!("documents[{idx}].observedAtMs"))?;
            m.insert("observedAtMs".to_owned(), Value::Number(ms.into()));
        }
        let doc: ContextDocumentInput = serde_json::from_value(item_val)
            .map_err(|e| ApiError::invalid(format!("invalid 'documents[{idx}]': {e}")))?;
        if doc.url.trim().is_empty() {
            return Err(ApiError::invalid(format!(
                "'documents[{idx}].url' must not be empty"
            )));
        }
        let vf_ms = match doc
            .valid_from
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(raw) => Some(
                parse_timestamp_ms(raw)
                    .or_else(|| raw.parse::<u64>().ok())
                    .ok_or_else(|| {
                        ApiError::invalid(format!(
                            "invalid 'documents[{idx}].validFrom' timestamp '{raw}'"
                        ))
                    })?,
            ),
            None => None,
        };
        let vu_ms = match doc
            .valid_until
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(raw) => Some(
                parse_timestamp_ms(raw)
                    .or_else(|| raw.parse::<u64>().ok())
                    .ok_or_else(|| {
                        ApiError::invalid(format!(
                            "invalid 'documents[{idx}].validUntil' timestamp '{raw}'"
                        ))
                    })?,
            ),
            None => None,
        };
        if let (Some(s), Some(e)) = (vf_ms, vu_ms)
            && s > e
        {
            return Err(ApiError::invalid(format!(
                "'documents[{idx}].validFrom' must be <= 'validUntil'"
            )));
        }
        out.push(doc);
    }
    Ok(out)
}

pub(crate) fn build_search_request(
    obj: &Map<String, Value>,
    require_query_or_subject: bool,
) -> Result<ContextSearchRequest, ApiError> {
    let req = if let Some(nested) = obj.get("search") {
        let m = nested
            .as_object()
            .ok_or_else(|| ApiError::invalid("'search' must be a JSON object"))?;
        build_search_from_map(m)?
    } else {
        build_search_from_map(obj)?
    };
    validate_search_request(&req, require_query_or_subject)?;
    Ok(req)
}

fn build_search_from_map(obj: &Map<String, Value>) -> Result<ContextSearchRequest, ApiError> {
    let query = match obj.get("query") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(_) => return Err(ApiError::invalid("'query' must be a string")),
    };
    let mut source_ids = Vec::new();
    for raw_sid in parse_string_list(obj.get("sourceIds"), "sourceIds")? {
        let valid = validate_ident_str(&raw_sid, "sourceIds[]")?;
        if !source_ids.contains(&valid) {
            source_ids.push(valid);
        }
    }
    if let Some(single) = opt_ident(obj, "sourceId", None)?
        && !source_ids.contains(&single)
    {
        source_ids.push(single);
    }
    let subject_key = match obj.get("subjectKey") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_owned())
        }
        Some(_) => return Err(ApiError::invalid("'subjectKey' must be a string")),
    };
    // [策略] Search 默认返回 Top-16 证据单元（与 ContextSearchRequest::default 一致，平衡召回覆盖与下游打包耗时）
    let limit = opt_usize_bounded(obj, "limit", 1, MAX_CONTEXT_RESULT_LIMIT)?.unwrap_or(16);

    Ok(ContextSearchRequest {
        query,
        source_ids,
        subject_key,
        required_fields: parse_string_list(obj.get("requiredFields"), "requiredFields")?,
        as_of_ms: parse_opt_time(obj.get("asOf").or_else(|| obj.get("asOfMs")), "asOf")?,
        effective_at_ms: parse_opt_time(
            obj.get("effectiveAt").or_else(|| obj.get("effectiveAtMs")),
            "effectiveAt",
        )?,
        limit,
        include_unverified: opt_bool(obj, "includeUnverified")?.unwrap_or(false),
        include_conflicts: opt_bool(obj, "includeConflicts")?.unwrap_or(true),
        enable_semantic_sketch: opt_bool(obj, "enableSemanticSketch")?.unwrap_or(true),
        learn_enabled: opt_bool(obj, "learnEnabled")?.unwrap_or(true),
    })
}

pub(crate) fn validate_search_request(
    req: &ContextSearchRequest,
    require_query_or_subject: bool,
) -> Result<(), ApiError> {
    if require_query_or_subject
        && req.query.trim().is_empty()
        && req
            .subject_key
            .as_deref()
            .is_none_or(|s| s.trim().is_empty())
    {
        return Err(ApiError::invalid(
            "context search/pack requires a non-empty 'query' or 'subjectKey'",
        ));
    }
    if req.limit == 0 || req.limit > MAX_CONTEXT_RESULT_LIMIT {
        return Err(ApiError::invalid(format!(
            "'limit' must be between 1 and {MAX_CONTEXT_RESULT_LIMIT}"
        )));
    }
    for sid in &req.source_ids {
        validate_ident_str(sid, "sourceIds[]")?;
    }
    Ok(())
}

pub(crate) fn build_pack_request(
    obj: &Map<String, Value>,
    require_query_or_subject: bool,
) -> Result<ContextPackRequest, ApiError> {
    if let Some(nested) = obj.get("pack") {
        let m = nested
            .as_object()
            .ok_or_else(|| ApiError::invalid("'pack' must be a JSON object"))?;
        return build_pack_from_map(m, require_query_or_subject);
    }
    build_pack_from_map(obj, require_query_or_subject)
}

fn build_pack_from_map(
    obj: &Map<String, Value>,
    require_query_or_subject: bool,
) -> Result<ContextPackRequest, ApiError> {
    let search = build_search_request(obj, require_query_or_subject)?;
    let selector = match obj.get("selector") {
        None | Some(Value::Null) => PackSelector::default(),
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| ApiError::invalid(format!("invalid 'selector': {e}")))?,
    };
    let ordering = match obj.get("ordering") {
        None | Some(Value::Null) => PackOrdering::default(),
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| ApiError::invalid(format!("invalid 'ordering': {e}")))?,
    };
    let budget_tokens = match opt_usize_bounded(obj, "budgetTokens", 1, MAX_PACK_BUDGET_TOKENS)? {
        Some(b) => b,
        // [策略] ContextPack 默认预算 2048 tokens（与 ContextPackRequest::default 对齐）
        None => opt_usize_bounded(obj, "maxTokens", 1, MAX_PACK_BUDGET_TOKENS)?.unwrap_or(2048),
    };
    // [策略] 默认预留 5% (0.05) token 安全边际防跨分词器漂移超支
    let token_safety_margin = opt_f32_bounded(obj, "tokenSafetyMargin", 0.0, 0.5)?.unwrap_or(0.05);

    Ok(ContextPackRequest {
        search,
        budget_tokens,
        selector,
        ordering,
        token_safety_margin,
        must_keep_conflicts: opt_bool(obj, "mustKeepConflicts")?.unwrap_or(true),
        must_keep_table_headers: opt_bool(obj, "mustKeepTableHeaders")?.unwrap_or(true),
        must_keep_qualifiers: opt_bool(obj, "mustKeepQualifiers")?.unwrap_or(true),
    })
}

pub(crate) fn build_action_request(
    op: &str,
    obj: &Map<String, Value>,
) -> Result<ContextActionRequest, ApiError> {
    let mode_str = if op == "action" {
        match obj.get("mode") {
            None | Some(Value::Null) => "context",
            Some(Value::String(s)) => s.as_str(),
            Some(_) => return Err(ApiError::invalid("'mode' must be a string")),
        }
    } else {
        op
    };
    let mode: ActionMode = serde_json::from_value(Value::String(to_camel(mode_str)))
        .map_err(|e| ApiError::invalid(format!("invalid action mode '{mode_str}': {e}")))?;

    let candidate_urls = parse_string_list(
        obj.get("candidateUrls").or_else(|| obj.get("urls")),
        "candidateUrls",
    )?;
    // `Changes` mode inspects delta streams without compiling a query; `Watch` with explicit seeds may also omit query
    let require_query = !matches!(mode, ActionMode::Changes)
        && !(mode == ActionMode::Watch && !candidate_urls.is_empty());
    let pack = build_pack_request(obj, require_query)?;

    let min_independent_sources =
        opt_usize_bounded(obj, "minIndependentSources", 1, MAX_MIN_INDEPENDENT_SOURCES)?
            .unwrap_or(1);
    // [策略] 单次 Action 默认最多执行 4 次补充采集请求（与 ContextActionRequest::default 一致）
    let max_acquisition_requests = opt_usize_bounded(
        obj,
        "maxAcquisitionRequests",
        0,
        MAX_ACTION_ACQUISITION_REQUESTS,
    )?
    .unwrap_or(4);

    Ok(ContextActionRequest {
        mode,
        pack,
        candidate_urls,
        max_staleness_ms: opt_u64(obj, "maxStalenessMs")?,
        min_independent_sources,
        allow_network_acquisition: opt_bool(obj, "allowNetworkAcquisition")?
            .unwrap_or(matches!(mode, ActionMode::Research | ActionMode::Watch)),
        allow_external_search: opt_bool(obj, "allowExternalSearch")?.unwrap_or(false),
        strict_constrain_to_urls: opt_bool(obj, "strictConstrainToUrls")?.unwrap_or(false),
        allowed_hosts: parse_allowed_hosts(obj.get("allowedHosts"))?,
        max_acquisition_requests,
        since_delta_seq: opt_u64(obj, "sinceDeltaSeq")?,
    })
}

pub(crate) fn parse_drift_guard(raw: &str) -> Result<DriftGuardKind, ApiError> {
    match raw.trim() {
        "cusumPageHinkley" | "cusum" | "cusum_page_hinkley" => Ok(DriftGuardKind::CusumPageHinkley),
        "bernoulliBocpd" | "bocpd" | "bernoulli_bocpd" => Ok(DriftGuardKind::BernoulliBocpd),
        "none" => Ok(DriftGuardKind::None),
        other => Err(ApiError::invalid(format!(
            "invalid 'driftGuard' '{other}'; expected 'cusumPageHinkley', 'bernoulliBocpd', or 'none'"
        ))),
    }
}
