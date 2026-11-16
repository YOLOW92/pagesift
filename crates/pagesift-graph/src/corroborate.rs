//! Deterministic entity anchoring (`SubjectKey`), claim records, and multi-source corroboration.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::citation::CitationEdge;
use crate::evidence::{EvidenceSpan, RawFieldTrace};
use crate::hash::{host_of, normalize_resource_key, short_id};
use crate::independence::{SourceLineage, independent_roots_for_claim_group};
use crate::lifecycle::{ClaimLifecycleRecord, ClaimStatus};
use crate::model::SourceKind;
use crate::norm::{
    NormalizedValue, canonical_currency_code, normalize_field_key,
    normalize_field_value_with_currency,
};
use crate::temporal::TemporalSemantics;

const BUILTIN_PRODUCT_KEYS: &[&str] = &["sku", "gtin", "gtin13", "isbn", "doi", "lei_code", "mpn"];

/// Provenance of the entity key anchoring claims from a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubjectKeySource {
    ExplicitConfig,
    JsonLdId,
    ProductCode,
    CanonicalCluster,
    ResourceKey,
}

/// Deterministic anchor grouping facts about the same underlying subject across URLs and formats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubjectKey {
    pub key: String,
    pub source: SubjectKeySource,
    pub raw: String,
}

/// Resolve entity anchor deterministically: explicit `subject_keys` -> JSON-LD `@id`/codes -> `canonical_url` -> `resource_key`.
pub fn resolve_subject_key(
    configured_keys: &[String],
    json_ld: &[Value],
    traces: &[RawFieldTrace],
    canonical_url: Option<&str>,
    resource_key: &str,
) -> SubjectKey {
    let trace_refs: Vec<&RawFieldTrace> = traces.iter().collect();
    if let Some(key) = resolve_identity_key(configured_keys, json_ld, &trace_refs) {
        return key;
    }
    resolve_fallback_subject_key(canonical_url, resource_key)
}

/// Resolve per-trace entity anchors so multi-product/list pages isolate sibling entities while
/// single-entity pages keep cross-format claims unified.
pub fn resolve_entity_subject_keys(
    configured_keys: &[String],
    json_ld: &[Value],
    traces: &[RawFieldTrace],
    canonical_url: Option<&str>,
    resource_key: &str,
) -> (SubjectKey, Vec<SubjectKey>) {
    let trace_groups: Vec<TraceEntityGroup> = traces.iter().map(entity_group_of).collect();
    let mut json_ld_ids: BTreeSet<usize> = (0..json_ld.len()).collect();
    let mut microdata_ids = BTreeSet::new();
    let mut extract_ids = BTreeSet::new();
    let mut grouped: BTreeMap<TraceEntityGroup, Vec<&RawFieldTrace>> = BTreeMap::new();

    for (trace, group) in traces.iter().zip(trace_groups.iter()) {
        match *group {
            TraceEntityGroup::JsonLd(idx) => {
                json_ld_ids.insert(idx);
            }
            TraceEntityGroup::Microdata(idx) => {
                microdata_ids.insert(idx);
            }
            TraceEntityGroup::ExtractItem(idx) => {
                extract_ids.insert(idx);
            }
            TraceEntityGroup::Page => {}
        }
        grouped.entry(*group).or_default().push(trace);
    }

    let multi_json_ld = json_ld_ids.len() > 1;
    let multi_microdata = microdata_ids.len() > 1;
    let multi_extract = extract_ids.len() > 1;
    if !multi_json_ld && !multi_microdata && !multi_extract {
        let page_key = resolve_subject_key(
            configured_keys,
            json_ld,
            traces,
            canonical_url,
            resource_key,
        );
        let per_trace = vec![page_key.clone(); traces.len()];
        return (page_key, per_trace);
    }

    let mut resolved_groups: BTreeMap<TraceEntityGroup, SubjectKey> = BTreeMap::new();
    let mut primary_entity_keys: BTreeSet<String> = BTreeSet::new();

    let mut assign_group = |group: TraceEntityGroup, multi_in_fmt: bool| {
        let group_traces = grouped.get(&group).map(Vec::as_slice).unwrap_or(&[]);
        let json_slice: &[Value] = match group {
            TraceEntityGroup::JsonLd(idx) => {
                json_ld.get(idx).map(std::slice::from_ref).unwrap_or(&[])
            }
            _ => &[],
        };
        if let Some(key) = resolve_identity_key(configured_keys, json_slice, group_traces) {
            if !is_boilerplate_entity(json_slice, group_traces) {
                primary_entity_keys.insert(key.key.clone());
            }
            resolved_groups.insert(group, key);
        } else if multi_in_fmt {
            let tag = group.fragment_tag();
            let raw = format!("{resource_key}#{tag}");
            let key = SubjectKey {
                key: format!("resource:{raw}"),
                source: SubjectKeySource::ResourceKey,
                raw,
            };
            if !is_boilerplate_entity(json_slice, group_traces) {
                primary_entity_keys.insert(key.key.clone());
            }
            resolved_groups.insert(group, key);
        }
    };

    for idx in &json_ld_ids {
        assign_group(TraceEntityGroup::JsonLd(*idx), multi_json_ld);
    }
    for idx in &microdata_ids {
        assign_group(TraceEntityGroup::Microdata(*idx), multi_microdata);
    }
    for idx in &extract_ids {
        assign_group(TraceEntityGroup::ExtractItem(*idx), multi_extract);
    }

    let page_traces = grouped
        .get(&TraceEntityGroup::Page)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let page_key = if primary_entity_keys.len() == 1 {
        let sole = primary_entity_keys.iter().next().unwrap();
        resolved_groups
            .values()
            .find(|k| &k.key == sole)
            .cloned()
            .unwrap_or_else(|| resolve_fallback_subject_key(canonical_url, resource_key))
    } else if let Some(key) = resolve_identity_key(configured_keys, &[], page_traces) {
        key
    } else {
        resolve_fallback_subject_key(canonical_url, resource_key)
    };

    let per_trace = trace_groups
        .into_iter()
        .map(|g| {
            resolved_groups
                .get(&g)
                .cloned()
                .unwrap_or_else(|| page_key.clone())
        })
        .collect();
    (page_key, per_trace)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum TraceEntityGroup {
    JsonLd(usize),
    Microdata(usize),
    ExtractItem(usize),
    Page,
}

impl TraceEntityGroup {
    fn fragment_tag(self) -> String {
        match self {
            Self::JsonLd(idx) => format!("json-ld/{idx}"),
            Self::Microdata(idx) => format!("microdata/{idx}"),
            Self::ExtractItem(idx) => format!("item/{idx}"),
            Self::Page => "page".to_owned(),
        }
    }
}

fn entity_group_of(trace: &RawFieldTrace) -> TraceEntityGroup {
    if let Some(rest) = trace.field_path.strip_prefix("/json-ld/")
        && let Some(idx) = parse_leading_index(rest)
    {
        return TraceEntityGroup::JsonLd(idx);
    }
    if let Some(rest) = trace.field_path.strip_prefix("/microdata/")
        && let Some(idx) = parse_leading_index(rest)
    {
        return TraceEntityGroup::Microdata(idx);
    }
    if matches!(
        trace.method,
        crate::evidence::ExtractionMethod::CssSelector { .. }
            | crate::evidence::ExtractionMethod::XPathSelector { .. }
    ) && let Some(rest) = trace.field_path.strip_prefix('/')
        && let Some(idx) = parse_leading_index(rest)
    {
        return TraceEntityGroup::ExtractItem(idx);
    }
    TraceEntityGroup::Page
}

fn parse_leading_index(rest: &str) -> Option<usize> {
    let seg = rest.split('/').next()?;
    (!seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()))
        .then(|| seg.parse().ok())
        .flatten()
}

fn is_boilerplate_entity(json_ld: &[Value], traces: &[&RawFieldTrace]) -> bool {
    const BOILERPLATE: &[&str] = &[
        "website",
        "webpage",
        "organization",
        "breadcrumblist",
        "searchaction",
        "sitenavigationelement",
    ];
    let has_boilerplate_type = |t: &str| {
        let lower = t.to_ascii_lowercase();
        BOILERPLATE
            .iter()
            .any(|b| lower == *b || lower.ends_with(&format!("/{b}")))
    };
    if let Some(obj) = json_ld.first().and_then(Value::as_object)
        && let Some(ty) = obj.get("@type").and_then(|v| {
            v.as_str()
                .or_else(|| v.as_array().and_then(|a| a.first()?.as_str()))
        })
    {
        return has_boilerplate_type(ty);
    }
    for trace in traces {
        if let crate::evidence::ExtractionMethod::JsonLd {
            entity_type: Some(ty),
            ..
        } = &trace.method
        {
            return has_boilerplate_type(ty);
        }
    }
    false
}

fn resolve_identity_key(
    configured_keys: &[String],
    json_ld: &[Value],
    traces: &[&RawFieldTrace],
) -> Option<SubjectKey> {
    for wanted in configured_keys {
        let wanted_norm = normalize_field_key(wanted);
        if let Some(val) = find_key_value(&wanted_norm, json_ld, traces) {
            return Some(SubjectKey {
                key: format!("{wanted_norm}:{}", val.trim().to_ascii_uppercase()),
                source: SubjectKeySource::ExplicitConfig,
                raw: val,
            });
        }
    }

    for obj in json_ld.iter().filter_map(Value::as_object) {
        if let Some(id) = obj.get("@id").and_then(Value::as_str)
            && !id.trim().is_empty()
            && !id.trim().starts_with("_:")
        {
            let norm = normalize_resource_key(id);
            return Some(SubjectKey {
                key: format!("id:{norm}"),
                source: SubjectKeySource::JsonLdId,
                raw: id.trim().to_owned(),
            });
        }
    }
    for trace in traces {
        if let crate::evidence::ExtractionMethod::JsonLd {
            entity_id: Some(id),
            ..
        } = &trace.method
            && !id.trim().is_empty()
            && !id.trim().starts_with("_:")
        {
            let norm = normalize_resource_key(id);
            return Some(SubjectKey {
                key: format!("id:{norm}"),
                source: SubjectKeySource::JsonLdId,
                raw: id.trim().to_owned(),
            });
        }
    }

    for code in BUILTIN_PRODUCT_KEYS {
        if let Some(val) = find_key_value(code, json_ld, traces) {
            return Some(SubjectKey {
                key: format!("{code}:{}", val.trim().to_ascii_uppercase()),
                source: SubjectKeySource::ProductCode,
                raw: val,
            });
        }
    }
    None
}

fn resolve_fallback_subject_key(canonical_url: Option<&str>, resource_key: &str) -> SubjectKey {
    if let Some(can) = canonical_url.filter(|c| !c.trim().is_empty())
        && crate::model::DocNode::is_trusted_canonical(can, resource_key)
    {
        let key = normalize_resource_key(can);
        return SubjectKey {
            key: format!("canonical:{key}"),
            source: SubjectKeySource::CanonicalCluster,
            raw: can.to_owned(),
        };
    }

    SubjectKey {
        key: format!("resource:{resource_key}"),
        source: SubjectKeySource::ResourceKey,
        raw: resource_key.to_owned(),
    }
}

fn find_key_value(
    wanted_norm: &str,
    json_ld: &[Value],
    traces: &[&RawFieldTrace],
) -> Option<String> {
    for trace in traces {
        if normalize_field_key(&trace.field_name) == wanted_norm
            && let Some(s) = scalar_to_nonempty_string(&trace.value)
        {
            return Some(s);
        }
    }
    for item in json_ld {
        if let Some(s) = search_json_key(wanted_norm, item) {
            return Some(s);
        }
    }
    None
}

fn search_json_key(wanted_norm: &str, val: &Value) -> Option<String> {
    match val {
        Value::Object(map) => {
            for (k, v) in map {
                if normalize_field_key(k) == wanted_norm
                    && let Some(s) = scalar_to_nonempty_string(v)
                {
                    return Some(s);
                }
                if matches!(v, Value::Object(_) | Value::Array(_))
                    && let Some(s) = search_json_key(wanted_norm, v)
                {
                    return Some(s);
                }
            }
            None
        }
        Value::Array(arr) => arr
            .iter()
            .find_map(|item| search_json_key(wanted_norm, item)),
        _ => None,
    }
}

fn scalar_to_nonempty_string(v: &Value) -> Option<String> {
    let s = match v {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    (!s.is_empty()).then_some(s)
}

pub const DEFAULT_CONTRACT_ID: &str = "default";

fn default_contract_id() -> String {
    DEFAULT_CONTRACT_ID.to_string()
}

fn is_default_contract(s: &str) -> bool {
    s.is_empty() || s == DEFAULT_CONTRACT_ID
}

/// Normalized claim extracted from one document along with its grounded evidence span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimRecord {
    pub claim_id: String,
    pub subject_key: String,
    pub resource_key: String,
    pub field_path: String,
    pub normalized_field: String,
    pub normalized_value: NormalizedValue,
    pub raw_value: Value,
    pub doc_id: String,
    pub source_url: String,
    pub host: String,
    pub source_kind: SourceKind,
    pub engine: String,
    pub observed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporal: Option<TemporalSemantics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    pub evidence: EvidenceSpan,
    #[serde(
        default = "default_contract_id",
        skip_serializing_if = "is_default_contract"
    )]
    pub contract_id: String,
}

impl ClaimRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn from_trace(
        subject_key: &str,
        resource_key: &str,
        doc_id: &str,
        source_url: &str,
        source_kind: SourceKind,
        engine: &str,
        observed_at_ms: u64,
        canonical_url: Option<String>,
        trace: &RawFieldTrace,
        evidence: EvidenceSpan,
    ) -> Self {
        Self::from_trace_with_currency(
            subject_key,
            resource_key,
            doc_id,
            source_url,
            source_kind,
            engine,
            observed_at_ms,
            canonical_url,
            trace,
            evidence,
            Some("USD"),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_trace_with_currency(
        subject_key: &str,
        resource_key: &str,
        doc_id: &str,
        source_url: &str,
        source_kind: SourceKind,
        engine: &str,
        observed_at_ms: u64,
        canonical_url: Option<String>,
        trace: &RawFieldTrace,
        evidence: EvidenceSpan,
        context_currency: Option<&str>,
    ) -> Self {
        let normalized_field = normalize_field_key(&trace.field_name);
        let normalized_value =
            normalize_field_value_with_currency(&trace.field_name, &trace.value, context_currency);
        let claim_id = short_id(
            "clm",
            &[
                subject_key,
                doc_id,
                &trace.field_path,
                &normalized_value.bucket_key(),
            ],
        );
        Self {
            claim_id,
            subject_key: subject_key.to_owned(),
            resource_key: resource_key.to_owned(),
            field_path: trace.field_path.clone(),
            normalized_field,
            normalized_value,
            raw_value: trace.value.clone(),
            doc_id: doc_id.to_owned(),
            source_url: source_url.to_owned(),
            host: host_of(source_url),
            source_kind,
            engine: engine.to_owned(),
            observed_at_ms,
            temporal: None,
            canonical_url,
            evidence,
            contract_id: default_contract_id(),
        }
    }

    pub fn with_temporal(mut self, temporal: TemporalSemantics) -> Self {
        self.temporal = Some(temporal);
        self
    }

    pub fn with_contract(mut self, contract_id: impl Into<String>) -> Self {
        let c = contract_id.into();
        self.contract_id = if c.trim().is_empty() {
            default_contract_id()
        } else {
            c
        };
        self
    }
}

fn is_currency_field(field_name: &str) -> bool {
    let k = normalize_field_key(field_name);
    matches!(
        k.as_str(),
        "price_currency" | "currency" | "currency_code" | "money_currency"
    ) || k.ends_with("_currency")
}

fn parent_field_prefix(path: &str) -> &str {
    match (path.rfind('.'), path.rfind(':')) {
        (Some(d), Some(c)) => &path[..d.max(c)],
        (Some(d), None) => &path[..d],
        (None, Some(c)) => &path[..c],
        (None, None) => "",
    }
}

/// 为每个预对齐字段追踪推理上下文货币（同父前缀 `priceCurrency` -> 同实体组一致货币 -> 整页单一确认货币）。
///
/// 若当前页面与实体上下文完全缺少明确货币信号（或存在互相矛盾的货币），返回 `None`，
/// 使下游归一化保留 `"UNKNOWN"` 而非盲目假定 `"USD"`（1.5.5 G5）。
pub fn resolve_trace_context_currencies(traces: &[RawFieldTrace]) -> Vec<Option<String>> {
    let mut sibling_currency: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut group_currencies: BTreeMap<TraceEntityGroup, BTreeSet<String>> = BTreeMap::new();
    let mut doc_currencies: BTreeSet<String> = BTreeSet::new();

    for trace in traces {
        let group = entity_group_of(trace);
        let iso = if is_currency_field(&trace.field_name) {
            trace.value.as_str().and_then(canonical_currency_code)
        } else {
            let k = normalize_field_key(&trace.field_name);
            let is_money_key = k.contains("price")
                || k.contains("cost")
                || k.contains("fee")
                || k.contains("amount")
                || k.contains("salary")
                || k.contains("revenue");
            trace.value.as_str().and_then(|s| {
                if is_money_key && s.len() <= 36 {
                    canonical_currency_code(s)
                } else {
                    None
                }
            })
        };
        if let Some(cur) = iso {
            if is_currency_field(&trace.field_name) {
                let prefix = parent_field_prefix(&trace.field_path).to_string();
                sibling_currency
                    .entry(prefix)
                    .or_default()
                    .insert(cur.clone());
            }
            group_currencies
                .entry(group)
                .or_default()
                .insert(cur.clone());
            doc_currencies.insert(cur);
        }
    }

    traces
        .iter()
        .map(|trace| {
            // 1. 精确同父路径兄弟货币键（如 offers.priceCurrency 对 offers.price）
            let mut prefix = parent_field_prefix(&trace.field_path);
            loop {
                if let Some(set) = sibling_currency.get(prefix)
                    && set.len() == 1
                {
                    return set.iter().next().cloned();
                }
                if prefix.is_empty() {
                    break;
                }
                prefix = parent_field_prefix(prefix);
            }
            // 2. 同实体组内部一致货币
            let group = entity_group_of(trace);
            if let Some(set) = group_currencies.get(&group)
                && set.len() == 1
            {
                return set.iter().next().cloned();
            }
            // 3. 整页所有明确货币线索一致
            if doc_currencies.len() == 1 {
                return doc_currencies.iter().next().cloned();
            }
            None
        })
        .collect()
}

/// Corroborated agreement cluster where one or more sources assert the same normalized fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FactCluster {
    pub cluster_id: String,
    pub subject_key: String,
    pub normalized_field: String,
    pub value: NormalizedValue,
    /// Verified (`Active`) supporting claims (equals all claims when constructed statelessly without lifecycle).
    pub support_count: usize,
    /// Distinct verified (`Active`) supporting documents.
    pub distinct_sources: usize,
    /// Currently valid independent origin roots backed by verified (`Active`) claims.
    #[serde(default)]
    pub independent_sources: usize,
    /// Explicit verified (`Active`) support count (distinguished from `unverified_support_count`).
    #[serde(default)]
    pub verified_support_count: usize,
    /// Unverified (`Unverified`) stale/pending-reverification support count; never counted as verified support.
    #[serde(default)]
    pub unverified_support_count: usize,
    /// Distinct documents holding `Unverified` claims in this cluster.
    #[serde(default)]
    pub unverified_distinct_sources: usize,
    /// Combined independent origin roots across both `Active` and `Unverified` claims.
    #[serde(default)]
    pub total_independent_sources: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub origin_roots: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unverified_origin_roots: Vec<String>,
    pub distinct_hosts: usize,
    pub distinct_formats: Vec<String>,
    pub claim_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unverified_claim_ids: Vec<String>,
    pub evidence: Vec<EvidenceSpan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unverified_evidence: Vec<EvidenceSpan>,
}

/// Aggregate claims into corroborated `(subject_key, normalized_field, canonical_value)` clusters.
pub fn build_fact_clusters(
    claims: &[ClaimRecord],
    max_claims_per_bucket: usize,
) -> Vec<FactCluster> {
    build_fact_clusters_with_lineage(claims, max_claims_per_bucket, &BTreeMap::new(), &[])
}

/// Aggregate claims into corroborated clusters while collapsing dependent/derived sources via `lineage`.
pub fn build_fact_clusters_with_lineage(
    claims: &[ClaimRecord],
    max_claims_per_bucket: usize,
    lineage: &BTreeMap<String, SourceLineage>,
    citations: &[CitationEdge],
) -> Vec<FactCluster> {
    build_fact_clusters_with_lineage_checked(claims, max_claims_per_bucket, lineage, citations).0
}

/// Variant of [`build_fact_clusters_with_lineage`] that also reports whether any bucket hit `max_claims_per_bucket`.
pub fn build_fact_clusters_with_lineage_checked(
    claims: &[ClaimRecord],
    max_claims_per_bucket: usize,
    lineage: &BTreeMap<String, SourceLineage>,
    citations: &[CitationEdge],
) -> (Vec<FactCluster>, bool) {
    build_fact_clusters_internal(
        claims.iter().map(|c| (c, true)),
        max_claims_per_bucket,
        lineage,
        citations,
    )
}

/// Aggregate lifecycle-aware claims into corroborated clusters while preserving the strict semantic
/// distinction between currently verified (`Active`) support and pending-reverification (`Unverified`)
/// support. Withdrawn and superseded claims are ignored.
pub fn build_fact_clusters_from_lifecycles_checked(
    records: &[ClaimLifecycleRecord],
    max_claims_per_bucket: usize,
    lineage: &BTreeMap<String, SourceLineage>,
    citations: &[CitationEdge],
) -> (Vec<FactCluster>, bool) {
    // Active entries precede Unverified so Active support is never crowded out when a bucket caps out.
    let active_iter = records
        .iter()
        .filter(|r| matches!(r.status, ClaimStatus::Active))
        .map(|r| (&r.claim, true));
    let unverified_iter = records
        .iter()
        .filter(|r| r.status.is_active_or_unverified() && !matches!(r.status, ClaimStatus::Active))
        .map(|r| (&r.claim, false));
    build_fact_clusters_internal(
        active_iter.chain(unverified_iter),
        max_claims_per_bucket,
        lineage,
        citations,
    )
}

type FactClusterBucketKey = (String, String, String);
type FactClusterBucket<'a> = Vec<(&'a ClaimRecord, bool)>;

fn build_fact_clusters_internal<'a>(
    items: impl Iterator<Item = (&'a ClaimRecord, bool)>,
    max_claims_per_bucket: usize,
    lineage: &BTreeMap<String, SourceLineage>,
    citations: &[CitationEdge],
) -> (Vec<FactCluster>, bool) {
    let mut buckets: BTreeMap<FactClusterBucketKey, FactClusterBucket<'a>> = BTreeMap::new();
    let mut truncated = false;
    for (claim, is_active) in items {
        let key = (
            claim.subject_key.clone(),
            claim.normalized_field.clone(),
            claim.normalized_value.bucket_key(),
        );
        let slot = buckets.entry(key).or_default();
        if slot.len() < max_claims_per_bucket {
            slot.push((claim, is_active));
        } else {
            truncated = true;
        }
    }

    let clusters = buckets
        .into_iter()
        .map(|((subject_key, normalized_field, val_key), items)| {
            let active_group: Vec<&ClaimRecord> = items
                .iter()
                .filter_map(|&(c, active)| active.then_some(c))
                .collect();
            let unverified_group: Vec<&ClaimRecord> = items
                .iter()
                .filter_map(|&(c, active)| (!active).then_some(c))
                .collect();
            let all_group: Vec<&ClaimRecord> = items.iter().map(|&(c, _)| c).collect();

            let distinct_sources: BTreeSet<&str> =
                active_group.iter().map(|c| c.doc_id.as_str()).collect();
            let unverified_distinct_sources: BTreeSet<&str> =
                unverified_group.iter().map(|c| c.doc_id.as_str()).collect();
            let origin_roots = if active_group.is_empty() {
                Vec::new()
            } else {
                independent_roots_for_claim_group(&active_group, lineage, citations)
            };
            let unverified_origin_roots = if unverified_group.is_empty() {
                Vec::new()
            } else {
                independent_roots_for_claim_group(&unverified_group, lineage, citations)
            };
            let total_independent_sources = if unverified_group.is_empty() {
                origin_roots.len()
            } else {
                independent_roots_for_claim_group(&all_group, lineage, citations).len()
            };
            let distinct_hosts: BTreeSet<&str> = all_group
                .iter()
                .map(|c| c.host.as_str())
                .filter(|h| !h.is_empty())
                .collect();
            let distinct_formats: BTreeSet<String> = all_group
                .iter()
                .map(|c| c.source_kind.format_label())
                .collect();
            let first = all_group[0];

            FactCluster {
                cluster_id: short_id("fct", &[&subject_key, &normalized_field, &val_key]),
                subject_key,
                normalized_field,
                value: first.normalized_value.clone(),
                support_count: active_group.len(),
                distinct_sources: distinct_sources.len(),
                independent_sources: origin_roots.len(),
                verified_support_count: active_group.len(),
                unverified_support_count: unverified_group.len(),
                unverified_distinct_sources: unverified_distinct_sources.len(),
                total_independent_sources,
                origin_roots,
                unverified_origin_roots,
                distinct_hosts: distinct_hosts.len(),
                distinct_formats: distinct_formats.into_iter().collect(),
                claim_ids: active_group.iter().map(|c| c.claim_id.clone()).collect(),
                unverified_claim_ids: unverified_group
                    .iter()
                    .map(|c| c.claim_id.clone())
                    .collect(),
                evidence: active_group.iter().map(|c| c.evidence.clone()).collect(),
                unverified_evidence: unverified_group
                    .iter()
                    .map(|c| c.evidence.clone())
                    .collect(),
            }
        })
        .collect();
    (clusters, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{EvidenceAlignment, ExtractionMethod};
    use serde_json::json;

    #[test]
    fn resolves_subject_key_priority_and_corroborates_cross_format() {
        let json_ld = vec![json!({
            "@id": "https://example.com/products/pro",
            "sku": "PRO-100",
            "price": "$49/mo"
        })];
        let sk = resolve_subject_key(&[], &json_ld, &[], None, "https://example.com/a");
        assert_eq!(sk.source, SubjectKeySource::JsonLdId);

        let sk_explicit = resolve_subject_key(
            &["sku".into()],
            &json_ld,
            &[],
            None,
            "https://example.com/a",
        );
        assert_eq!(sk_explicit.source, SubjectKeySource::ExplicitConfig);
        assert_eq!(sk_explicit.key, "sku:PRO-100");

        let mk = |doc_id: &str, url: &str, kind: SourceKind, raw: &str| {
            let trace = RawFieldTrace {
                field_path: "/price".into(),
                field_name: "price".into(),
                value: json!(raw),
                raw_quote: raw.into(),
                attr_name: None,
                method: ExtractionMethod::CssSelector {
                    selector: ".price".into(),
                    relocated_from: None,
                    step: None,
                },
                source_url: None,
                visible_in_dom: true,
            };
            let span = EvidenceSpan {
                doc_id: doc_id.into(),
                url: url.into(),
                block_id: Some(format!("{doc_id}#b0")),
                chunk_idx: None,
                start: Some(0),
                end: Some(10),
                excerpt: raw.into(),
                confidence: 1.0,
                alignment: EvidenceAlignment::ExactBlock,
                verified: true,
                field_path: "/price".into(),
                method: trace.method.clone(),
            };
            ClaimRecord::from_trace(
                "sku:PRO-100",
                url,
                doc_id,
                url,
                kind,
                "http",
                100,
                None,
                &trace,
                span,
            )
        };
        let c1 = mk("d1", "https://example.com/p", SourceKind::Html, "$49/mo");
        let c2 = mk(
            "d2",
            "https://example.com/spec.pdf",
            SourceKind::Pdf,
            "USD 49.00 per month",
        );
        let clusters = build_fact_clusters(&[c1, c2], 64);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].distinct_sources, 2);
        assert_eq!(clusters[0].distinct_formats, ["html", "pdf"]);
    }

    #[test]
    fn lifecycle_clusters_distinguish_active_from_unverified_support_and_truncation() {
        let mk_rec = |doc_id: &str, url: &str, active: bool| -> ClaimLifecycleRecord {
            let span = EvidenceSpan {
                doc_id: doc_id.into(),
                url: url.into(),
                block_id: None,
                chunk_idx: None,
                start: None,
                end: None,
                excerpt: "price: $49/mo".into(),
                confidence: 0.9,
                alignment: EvidenceAlignment::ExactBlock,
                verified: true,
                field_path: "price".into(),
                method: ExtractionMethod::JsonLd {
                    entity_type: None,
                    entity_id: None,
                },
            };
            let trace = RawFieldTrace {
                field_path: "price".into(),
                field_name: "price".into(),
                value: serde_json::json!("$49/mo"),
                raw_quote: "$49/mo".into(),
                attr_name: None,
                method: span.method.clone(),
                source_url: None,
                visible_in_dom: true,
            };
            let claim = ClaimRecord::from_trace(
                "sku:PRO-100",
                url,
                doc_id,
                url,
                SourceKind::Html,
                "http",
                100,
                None,
                &trace,
                span,
            );
            ClaimLifecycleRecord {
                lifecycle_id: format!("clc-{doc_id}"),
                claim,
                status: if active {
                    ClaimStatus::Active
                } else {
                    ClaimStatus::Unverified {
                        reason: "transient decode error".into(),
                        since_ms: 200,
                    }
                },
                first_observed_at_ms: 100,
                last_confirmed_at_ms: 100,
                observation_count: 1,
                history: Vec::new(),
                timeline: Vec::new(),
                history_truncated: false,
            }
        };

        let r_active = mk_rec("d1", "https://a.example.com/p", true);
        let r_unv = mk_rec("d2", "https://b.example.com/p", false);
        let r_unv2 = mk_rec("d3", "https://c.example.com/p", false);

        let (clusters, truncated) = build_fact_clusters_from_lifecycles_checked(
            &[r_unv.clone(), r_active, r_unv2],
            2,
            &BTreeMap::new(),
            &[],
        );
        assert!(truncated, "bucket cap=2 with 3 claims must set truncated");
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].verified_support_count, 1);
        assert_eq!(clusters[0].support_count, 1);
        assert_eq!(clusters[0].unverified_support_count, 1);
        assert_eq!(clusters[0].independent_sources, 1);
        assert_eq!(clusters[0].total_independent_sources, 2);

        let (only_unv, truncated_unv) =
            build_fact_clusters_from_lifecycles_checked(&[r_unv], 64, &BTreeMap::new(), &[]);
        assert!(!truncated_unv);
        assert_eq!(only_unv[0].support_count, 0);
        assert_eq!(only_unv[0].verified_support_count, 0);
        assert_eq!(only_unv[0].independent_sources, 0);
        assert_eq!(only_unv[0].unverified_support_count, 1);
        assert_eq!(only_unv[0].total_independent_sources, 1);
    }
}
